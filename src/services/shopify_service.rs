use crate::models::*;
use anyhow::{anyhow, Result};
use reqwest::{header, Client};
use serde_json::json;
use std::env;
use std::fs;
use std::io::Write;
use tracing::{info, warn, error};

#[derive(Clone)]
pub struct ShopifyService {
    client: Client,
    base_url: String,
    location_id: Option<i64>,
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
            headers.insert(
                "X-Shopify-Access-Token",
                header::HeaderValue::from_str(new_token).unwrap(),
            );
            headers.insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            );

            self.client = Client::builder()
                .default_headers(headers)
                .build()?;

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

        if !found {
            lines.push(format!("SHOPIFY_ACCESS_TOKEN={}", new_token));
        }

        let mut file = fs::File::create(path)?;
        for line in lines {
            writeln!(file, "{}", line)?;
        }
        Ok(())
    }

    async fn get_location_id(&mut self) -> Result<i64> {
        if let Some(id) = self.location_id {
            return Ok(id);
        }

        if let Ok(id_str) = env::var("SHOPIFY_LOCATION_ID") {
            if let Ok(id) = id_str.parse::<i64>() {
                self.location_id = Some(id);
                return Ok(id);
            }
        }

        let url = format!("{}/locations.json", self.base_url);
        let response = self.client.get(&url).send().await?;

        if !response.status().is_success() {
            return Err(anyhow!("Failed to fetch locations: {}. Please set SHOPIFY_LOCATION_ID in your .env", response.status()));
        }

        let locations: ShopifyLocationsResponse = response.json().await?;
        let location_id = locations
            .locations
            .first()
            .ok_or_else(|| anyhow!("No locations found"))?
            .id;

        self.location_id = Some(location_id);
        Ok(location_id)
    }

    pub async fn update_inventory(&mut self, product: &Product) -> Result<()> {
        let loc_id = self.get_location_id().await?;
        info!("🔎 Power-Syncing SKU: {}", product.sku);

        
        let graphql_url = format!("{}/graphql.json", self.base_url);
        let query = json!({
            "query": format!(r#"
                {{
                    productVariants(first: 1, query: "sku:{}") {{
                        edges {{
                            node {{
                                inventoryItem {{
                                    id
                                }}
                            }}
                        }}
                    }}
                }}
            "#, product.sku)
        });

        let res = self.client.post(&graphql_url).json(&query).send().await?;
        let data: serde_json::Value = res.json().await?;

        // Extract the ID from the GraphQL response
        let inventory_item_id_raw = data["data"]["productVariants"]["edges"]
            .as_array()
            .and_then(|edges| edges.first())
            .and_then(|edge| edge["node"]["inventoryItem"]["id"].as_str());

        let inv_id = match inventory_item_id_raw.and_then(|id| id.split('/').last()).and_then(|id| id.parse::<i64>().ok()) {
            Some(id) => id,
            None => {
                warn!("⚠️ SKU {} not found via GraphQL. Skipping.", product.sku);
                return Err(anyhow!("SKU {} not found", product.sku));
            }
        };

        
        let update_url = format!("{}/inventory_levels/set.json", self.base_url);
        let body = json!({
            "location_id": loc_id,
            "inventory_item_id": inv_id,
            "available": product.inventory_quantity,
            "disconnect_if_necessary": true
        });

        let res = self.client.post(&update_url).json(&body).send().await?;
        let mut status = res.status();
        let mut text = res.text().await?;

        
        if text.contains("inventory tracking enabled") {
            info!("🔧 Auto-Enabling tracking for SKU: {}", product.sku);
            let track_url = format!("{}/inventory_items/{}.json", self.base_url, inv_id);
            let track_body = json!({
                "inventory_item": {
                    "id": inv_id,
                    "tracked": true
                }
            });
            
            // Turn on tracking
            let _ = self.client.put(&track_url).json(&track_body).send().await?;
            
            // Retry the original inventory update
            let retry_res = self.client.post(&update_url).json(&body).send().await?;
            status = retry_res.status();
            text = retry_res.text().await?;
        }

        if status.is_success() {
            println!("SUCCESS: {} updated to {}", product.sku, product.inventory_quantity);
            info!("Successfully updated SKU: {} to qty: {}", product.sku, product.inventory_quantity);
            Ok(())
        } else {
            println!("SHOPIFY REJECTED {}: {}", product.sku, text);
            error!("Inventory update failed for {}: {}", product.sku, text);
            Err(anyhow!("Shopify API rejection"))
        }
    }

    pub async fn create_product(&self, product: &Product) -> Result<()> {
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
                    "inventory_quantity": product.inventory_quantity,
                    "inventory_management": "shopify", 
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
                    let cost_body = json!({
                        "inventory_item": {
                            "id": inv_id,
                            "cost": product.cost.to_string(),
                            "tracked": true 
                        }
                    });
                    
                    let _ = self.client.put(&cost_url).json(&cost_body).send().await?;
                }
            }
            Ok(())
        } else {
            let err = response.text().await?;
            Err(anyhow!("Failed to create product: {}", err))
        }
    }
}