use crate::models::*;
use anyhow::{anyhow, Result};
use reqwest::{header, Client};
use serde_json::json;
use std::env;
use std::fs;
use std::io::Write;
use tracing::info;

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
        
        
        let mut retry_count = 0;
        while retry_count < 2 {
            let search_url = format!("{}/variants.json?sku={}", self.base_url, product.sku);
            let response = self.client.get(&search_url).send().await?;
            
            if response.status() == 401 {
                self.refresh_daily_token().await?;
                retry_count += 1;
                continue;
            }

            let data: serde_json::Value = response.json().await?;
            let variants = data["variants"].as_array().ok_or_else(|| anyhow!("Invalid response for SKU {}", product.sku))?;
            
            let variant = variants.iter().find(|v| v["sku"] == product.sku)
                .ok_or_else(|| anyhow!("SKU {} not found on Shopify", product.sku))?;

            let inventory_item_id = variant["inventory_item_id"].as_i64()
                .ok_or_else(|| anyhow!("No inventory_item_id for SKU {}", product.sku))?;

            let update_url = format!("{}/inventory_levels/set.json", self.base_url);
            let body = json!({
                "location_id": loc_id,
                "inventory_item_id": inventory_item_id,
                "available": product.inventory_quantity
            });

            let update_response = self.client.post(&update_url).json(&body).send().await?;

            if update_response.status().is_success() {
                info!("Successfully updated SKU: {} to qty: {}", product.sku, product.inventory_quantity);
                return Ok(());
            } else {
                let err_msg = update_response.text().await?;
                return Err(anyhow!("Inventory update failed for {}: {}", product.sku, err_msg));
            }
        }
        
        Err(anyhow!("Failed to update inventory for {} after token refresh", product.sku))
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
                    
                    
                    let cost_res = self.client.put(&cost_url).json(&cost_body).send().await?;
                    if cost_res.status().is_success() {
                        println!("Created {} with Cost: {}", product.sku, product.cost);
                    }
                }
            }
            Ok(())
        } else {
            let err = response.text().await?;
            Err(anyhow!("Failed to create product: {}", err))
        }
    }
}