use crate::models::*;
use anyhow::{anyhow, Result};
use reqwest::{header, Client};
use serde_json::json;
use std::env;
use std::fs;
use std::io::Write;
use tracing::{info, error};

#[derive(Clone)]
pub struct ShopifyService {
    client: Client,
    base_url: String,
    location_id: Option<i64>,
}

fn normalize_sku_for_comparison(sku: &str) -> String {
    sku.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_uppercase()
}

impl ShopifyService {
    pub fn new(store_url: String, access_token: String) -> Self {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            "X-Shopify-Access-Token",
            header::HeaderValue::from_str(&access_token).unwrap(),
        );
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );

        let client = Client::builder()
            .default_headers(headers)
            .build()
            .expect("Failed to build HTTP client");

        let base_url = format!("https://{}/admin/api/2024-01", store_url);

        Self {
            client,
            base_url,
            location_id: None,
        }
    }

    pub async fn refresh_daily_token(&mut self) -> Result<()> {
        let store_url = env::var("SHOPIFY_STORE_URL")?;
        let client_id = env::var("CLIENT_ID")?;
        let client_secret = env::var("CLIENT_SECRET")?;

        let url = format!("https://{}/admin/oauth/access_token", store_url);
        let body = json!({
            "client_id": client_id,
            "client_secret": client_secret,
            "grant_type": "client_credentials"
        });

        let response = self.client.post(&url).json(&body).send().await?;

        if response.status().is_success() {
            let data: serde_json::Value = response.json().await?;
            let new_token = data["access_token"]
                .as_str()
                .ok_or_else(|| anyhow!("No access token in response"))?;

            self.update_env_file(new_token)?;
            env::set_var("SHOPIFY_ACCESS_TOKEN", new_token);

            let mut headers = header::HeaderMap::new();
            headers.insert("X-Shopify-Access-Token", header::HeaderValue::from_str(new_token).unwrap());
            headers.insert(header::CONTENT_TYPE, header::HeaderValue::from_static("application/json"));

            self.client = Client::builder().default_headers(headers).build()?;

            info!("Token updated successfully");
            Ok(())
        } else {
            let err = response.text().await?;
            Err(anyhow!("Token refresh failed: {}", err))
        }
    }

    fn update_env_file(&self, new_token: &str) -> Result<()> {
        let path = ".env";
        let content = fs::read_to_string(path).unwrap_or_default();
        let mut lines: Vec<String> = Vec::new();
        let mut found = false;

        for line in content.lines() {
            if line.starts_with("SHOPIFY_ACCESS_TOKEN=") || line.starts_with("ACCESS_TOKEN=") {
                lines.push(format!("SHOPIFY_ACCESS_TOKEN={}", new_token));
                found = true;
            } else {
                lines.push(line.to_string());
            }
        }
        if !found { lines.push(format!("SHOPIFY_ACCESS_TOKEN={}", new_token)); }

        let mut file = fs::File::create(path)?;
        for line in lines { writeln!(file, "{}", line)?; }
        Ok(())
    }

    async fn get_location_id(&mut self) -> Result<i64> {
        if let Some(id) = self.location_id { return Ok(id); }
        if let Ok(id_str) = env::var("SHOPIFY_LOCATION_ID") {
            if let Ok(id) = id_str.parse::<i64>() {
                self.location_id = Some(id);
                return Ok(id);
            }
        }

        let url = format!("{}/locations.json", self.base_url);
        let response = self.client.get(&url).send().await?;
        let locations: ShopifyLocationsResponse = response.json().await?;
        let location_id = locations.locations.first().ok_or_else(|| anyhow!("No locations found"))?.id;
        self.location_id = Some(location_id);
        Ok(location_id)
    }

    async fn find_variant_by_sku(&self, sku: &str) -> Result<Option<serde_json::Value>> {
        let url = format!("{}/graphql.json", self.base_url);

        let query = json!({
            "query": r#"
                query($sku: String!) {
                    productVariants(first: 1, query: $sku) {
                        edges {
                            node {
                                id
                                sku
                                price
                                inventoryItem {
                                    id
                                }
                            }
                        }
                    }
                }
            "#,
            "variables": { "sku": format!("sku:{}", sku) }
        });

        let response = self.client.post(&url).json(&query).send().await?;
        let data: serde_json::Value = response.json().await?;

        if let Some(edges) = data["data"]["productVariants"]["edges"].as_array() {
            if let Some(node) = edges.first().map(|e| &e["node"]) {
                if normalize_sku_for_comparison(node["sku"].as_str().unwrap_or(""))
                    == normalize_sku_for_comparison(sku)
                {
                    let id_raw = node["id"].as_str().unwrap_or("");
                    let variant_id = id_raw.split('/').last().unwrap_or("0").parse::<i64>().unwrap_or(0);

                    let inv_id_raw = node["inventoryItem"]["id"].as_str().unwrap_or("");
                    let inv_item_id = inv_id_raw.split('/').last().unwrap_or("0").parse::<i64>().unwrap_or(0);

                    return Ok(Some(json!({
                        "id": variant_id,
                        "inventory_item_id": inv_item_id,
                        "price": node["price"].as_str().unwrap_or("0.00"),
                        "sku": node["sku"].as_str().unwrap_or(sku)
                    })));
                }
            }
        }
        Ok(None)
    }

    pub async fn upsert_product(&mut self, product: &Product) -> Result<()> {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;

        let existing = match self.find_variant_by_sku(&product.sku).await {
            Ok(v) => v,
            Err(e) if e.to_string() == "UNAUTHORIZED" => {
                self.refresh_daily_token().await?;
                self.find_variant_by_sku(&product.sku).await?
            }
            Err(e) => return Err(e),
        };

        match existing {
            Some(variant) => {
                let variant_id = variant["id"].as_i64().unwrap();
                let inv_item_id = variant["inventory_item_id"].as_i64().unwrap();

                let levels_url = format!("{}/inventory_levels.json?inventory_item_ids={}", self.base_url, inv_item_id);
                let levels_res = self.client.get(&levels_url).send().await?;
                let levels_data: serde_json::Value = levels_res.json().await?;

                let actual_loc_id = levels_data["inventory_levels"]
                    .as_array()
                    .and_then(|list| list.first())
                    .and_then(|lvl| lvl["location_id"].as_i64())
                    .unwrap_or_else(|| {
                        env::var("SHOPIFY_LOCATION_ID").unwrap_or_default().parse().unwrap_or(0)
                    });

                let inventory_url = format!("{}/inventory_levels/set.json", self.base_url);
                let inventory_body = json!({
                    "location_id": actual_loc_id,
                    "inventory_item_id": inv_item_id,
                    "available": product.inventory_quantity
                });

                let inv_res = self.client.post(&inventory_url).json(&inventory_body).send().await?;
                if !inv_res.status().is_success() {
                    error!("Inventory update failed at location {}: {}", actual_loc_id, inv_res.text().await?);
                    return Err(anyhow!("Inventory update failed"));
                }

                let variant_url = format!("{}/variants/{}.json", self.base_url, variant_id);
                let price_body = json!({
                    "variant": {
                        "id": variant_id,
                        "price": product.price.to_string(),
                        "compare_at_price": product.compare_at_price.to_string()
                    }
                });

                let price_res = self.client.put(&variant_url).json(&price_body).send().await?;
                if price_res.status().is_success() {
                    info!("Successfully updated SKU: {} at Location: {}", product.sku, actual_loc_id);
                    Ok(())
                } else {
                    error!("Price update failed for SKU {}: {}", product.sku, price_res.text().await?);
                    Err(anyhow!("Price update failed"))
                }
            }
            None => {
                self.create_product(product).await
            }
        }
    }

    pub async fn update_inventory(&mut self, product: &Product) -> Result<()> {
        self.upsert_product(product).await
    }

    pub async fn create_product(&self, product: &Product) -> Result<()> {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let url = format!("{}/products.json", self.base_url);
        let body = json!({
            "product": {
                "title": product.title,
                "vendor": product.oem,
                "body_html": product.description,
                "handle": product.handle,
                "variants": [{
                    "sku": product.sku,
                    "price": product.price.to_string(),
                    "compare_at_price": product.compare_at_price.to_string(),
                    "inventory_management": "shopify",
                    "inventory_quantity": product.inventory_quantity,
                    "barcode": product.barcode,
                    "weight": product.weight
                }]
            }
        });

        let response = self.client.post(&url).json(&body).send().await?;
        if response.status().is_success() {
            let res_json: serde_json::Value = response.json().await?;
            if let Some(variant) = res_json["product"]["variants"].as_array().and_then(|v| v.first()) {
                if let Some(inv_id) = variant["inventory_item_id"].as_i64() {
                    let cost_url = format!("{}/inventory_items/{}.json", self.base_url, inv_id);
                    let cost_body = json!({ "inventory_item": { "id": inv_id, "cost": product.cost.to_string(), "tracked": true } });
                    let _ = self.client.put(&cost_url).json(&cost_body).send().await?;
                }
            }
            Ok(())
        } else {
            Err(anyhow!("Failed to create product: {}", response.text().await?))
        }
    }
}