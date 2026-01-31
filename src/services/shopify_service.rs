use crate::models::*;
use anyhow::{anyhow, Result};
use reqwest::{header, Client};
use serde_json::json;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;
use tracing::{info, warn};

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

        let url = format!("{}/locations.json", self.base_url);
        let response = self.client.get(&url).send().await?;

        if !response.status().is_success() {
            return Err(anyhow!("Failed to fetch locations: {}", response.status()));
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
        let search_url = format!("{}/products.json?sku={}", self.base_url, product.sku);
        
        let response = self.client.get(&search_url).send().await?;
        if response.status() == 401 {
            self.refresh_daily_token().await?;
        }

        info!("Synced SKU: {}", product.sku);
        Ok(())
    }

    pub async fn create_product(&self, product: &Product) -> Result<()> {
        let url = format!("{}/products.json", self.base_url);
        let body = json!({
            "product": {
                "title": product.title,
                "variants": [{
                    "sku": product.sku,
                    "price": product.price.to_string(),
                    "inventory_quantity": product.inventory_quantity
                }]
            }
        });

        let response = self.client.post(&url).json(&body).send().await?;
        if !response.status().is_success() {
            return Err(anyhow!("Failed to create product"));
        }
        Ok(())
    }
}