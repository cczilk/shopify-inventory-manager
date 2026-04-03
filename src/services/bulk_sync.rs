use crate::models::Product;
use anyhow::{anyhow, Result};
use reqwest::Client;
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn, error};

#[derive(Debug)]
struct CachedVariant {
    variant_id: i64,
    inv_item_id: i64,
    location_id: i64,
}

fn is_rate_limited(body: &str) -> bool {
    body.contains("Exceeded 2 calls per second")
}

async fn fetch_all_variants(client: &Client, base_url: &str) -> Result<HashMap<String, CachedVariant>> {
    let url = format!("{}/graphql.json", base_url);
    let mut map: HashMap<String, CachedVariant> = HashMap::new();
    let mut cursor: Option<String> = None;
    let mut page = 0u32;

    loop {
        page += 1;
        let after_clause = match &cursor {
            Some(c) => format!(r#", after: "{}""#, c),
            None => String::new(),
        };

        let query = format!(
            r#"{{
                productVariants(first: 250{}) {{
                    pageInfo {{ hasNextPage endCursor }}
                    edges {{
                        node {{
                            id
                            sku
                            inventoryItem {{
                                id
                                inventoryLevels(first: 1) {{
                                    edges {{
                                        node {{
                                            location {{ id }}
                                        }}
                                    }}
                                }}
                            }}
                        }}
                    }}
                }}
            }}"#,
            after_clause
        );

        let body = json!({ "query": query });
        let response = client.post(&url).json(&body).send().await?;
        let data: serde_json::Value = response.json().await?;

        if let Some(errors) = data["errors"].as_array() {
            return Err(anyhow!("GraphQL error: {}", errors[0]["message"].as_str().unwrap_or("unknown")));
        }

        let edges = data["data"]["productVariants"]["edges"]
            .as_array()
            .ok_or_else(|| anyhow!("Unexpected GraphQL response on page {}", page))?;

        for edge in edges {
            let node = &edge["node"];

            let sku = node["sku"].as_str().unwrap_or("").trim().to_string();
            if sku.is_empty() { continue; }

            let variant_id = node["id"].as_str().unwrap_or("")
                .split('/').last().unwrap_or("0").parse::<i64>().unwrap_or(0);

            let inv_item_id = node["inventoryItem"]["id"].as_str().unwrap_or("")
                .split('/').last().unwrap_or("0").parse::<i64>().unwrap_or(0);

            let location_id = node["inventoryItem"]["inventoryLevels"]["edges"]
                .as_array()
                .and_then(|e| e.first())
                .and_then(|e| e["node"]["location"]["id"].as_str())
                .and_then(|s| s.split('/').last())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);

            map.insert(sku.to_uppercase(), CachedVariant { variant_id, inv_item_id, location_id });
        }

        let page_info = &data["data"]["productVariants"]["pageInfo"];
        if page_info["hasNextPage"].as_bool().unwrap_or(false) {
            cursor = page_info["endCursor"].as_str().map(String::from);
            // respect the rate limit between pages
            tokio::time::sleep(Duration::from_millis(500)).await;
        } else {
            break;
        }
    }

    info!("Bulk fetch complete: {} variants across {} pages", map.len(), page);
    Ok(map)
}

async fn update_variant(
    client: &Client,
    base_url: &str,
    product: &Product,
    cached: &CachedVariant,
) -> Result<()> {
    let mut loc_id = cached.location_id;

    // If location_id wasnt in the GraphQL response, fall back to env var
    if loc_id == 0 {
        loc_id = std::env::var("SHOPIFY_LOCATION_ID")
            .unwrap_or_default()
            .parse()
            .unwrap_or(0);

        if loc_id != 0 {
            let connect_url = format!("{}/inventory_levels/connect.json", base_url);
            let connect_body = json!({
                "location_id": loc_id,
                "inventory_item_id": cached.inv_item_id
            });
            let _ = client.post(&connect_url).json(&connect_body).send().await?;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    let inv_url = format!("{}/inventory_levels/set.json", base_url);
    let inv_body = json!({
        "location_id": loc_id,
        "inventory_item_id": cached.inv_item_id,
        "available": product.inventory_quantity,
        "disconnect_if_necessary": true
    });

    let inv_text = client.post(&inv_url).json(&inv_body).send().await?.text().await?;
    let inv_text = if is_rate_limited(&inv_text) {
        warn!("Rate limited on inventory for SKU {}, retrying after 4s...", product.sku);
        tokio::time::sleep(Duration::from_secs(4)).await;
        client.post(&inv_url).json(&inv_body).send().await?.text().await?
    } else {
        inv_text
    };

    let inv_data: serde_json::Value = serde_json::from_str(&inv_text)?;
    if inv_data.get("errors").is_some() {
        return Err(anyhow!("Inventory update failed: {}", inv_data["errors"]));
    }

    // Update price
    tokio::time::sleep(Duration::from_millis(500)).await;
    let variant_url = format!("{}/variants/{}.json", base_url, cached.variant_id);
    let price_body = json!({
        "variant": {
            "id": cached.variant_id,
            "price": product.price.to_string(),
            "compare_at_price": product.compare_at_price.to_string()
        }
    });

    let price_res = client.put(&variant_url).json(&price_body).send().await?;
    if price_res.status().is_success() {
        info!("Updated SKU: {} qty={} price={}", product.sku, product.inventory_quantity, product.price);
        Ok(())
    } else {
        Err(anyhow!("Price update failed for {}: {}", product.sku, price_res.text().await?))
    }
}

async fn create_product(client: &Client, base_url: &str, product: &Product) -> Result<()> {
    tokio::time::sleep(Duration::from_millis(600)).await;
    let url = format!("{}/products.json", base_url);
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

    let response = client.post(&url).json(&body).send().await?;
    if response.status().is_success() {
        let res_json: serde_json::Value = response.json().await?;
        if let Some(variant) = res_json["product"]["variants"].as_array().and_then(|v| v.first()) {
            if let Some(inv_id) = variant["inventory_item_id"].as_i64() {
                let cost_url = format!("{}/inventory_items/{}.json", base_url, inv_id);
                let cost_body = json!({
                    "inventory_item": {
                        "id": inv_id,
                        "cost": product.cost.to_string(),
                        "tracked": true
                    }
                });
                let _ = client.put(&cost_url).json(&cost_body).send().await?;
            }
        }
        info!("Created new product SKU: {}", product.sku);
        Ok(())
    } else {
        Err(anyhow!("Failed to create product {}: {}", product.sku, response.text().await?))
    }
}

pub async fn run_bulk_sync(
    client: &Client,
    base_url: &str,
    products: &[Product],
    mode: &str,
) -> Result<(u32, u32, u32)> {
    let upsert = mode == "upsert";

    info!("Bulk sync starting: {} products from price file, mode={}", products.len(), mode);
    println!("  Fetching all Shopify variants (one-time pull)…");

    let shopify_map = fetch_all_variants(client, base_url).await?;
    println!("  {} variants cached from Shopify.", shopify_map.len());
    println!("  Comparing and pushing changes…\n");

    let mut updated = 0u32;
    let mut created = 0u32;
    let mut skipped = 0u32;

    for product in products {
        tokio::time::sleep(Duration::from_millis(500)).await;

        match shopify_map.get(&product.sku.to_uppercase()) {
            Some(cached) => {
                match update_variant(client, base_url, product, cached).await {
                    Ok(_) => updated += 1,
                    Err(e) => {
                        error!("Failed to update SKU {}: {}", product.sku, e);
                        skipped += 1;
                    }
                }
            }
            None => {
                if upsert {
                    match create_product(client, base_url, product).await {
                        Ok(_) => created += 1,
                        Err(e) => {
                            error!("Failed to create SKU {}: {}", product.sku, e);
                            skipped += 1;
                        }
                    }
                } else {
                    info!("SKU {} not in Shopify, skipping (bulk update mode).", product.sku);
                    skipped += 1;
                }
            }
        }
    }

    Ok((updated, created, skipped))
}