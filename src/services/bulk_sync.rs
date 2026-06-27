use crate::models::Product;
use anyhow::{anyhow, Result};
use reqwest::Client;
use serde_json::json;
use std::io::Write;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn, error};

#[derive(Debug)]
struct CachedVariant {
    variant_id: i64,
    inv_item_id: i64,
    location_id: i64,
    current_price: f64,
    current_qty: i32,
    tracked: bool,
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
                            price
                            inventoryQuantity
                            inventoryItem {{
                                id
                                tracked 
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

        let edges = match data["data"]["productVariants"]["edges"].as_array() {
            Some(e) => e,
            None => {
                return Err(anyhow!(
                    "Unexpected GraphQL response on page {}. Full response: {}",
                    page,
                    serde_json::to_string_pretty(&data).unwrap_or_else(|_| data.to_string())
                ));
            }
        };

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

            let current_price = node["price"].as_str()
                .and_then(|p| p.parse::<f64>().ok())
                .unwrap_or(0.0);

            let current_qty = node["inventoryQuantity"].as_i64().unwrap_or(0) as i32;

            let tracked = node["inventoryItem"]["tracked"].as_bool().unwrap_or(false);

            map.insert(sku.to_uppercase(), CachedVariant {
                variant_id,
                inv_item_id,
                location_id,
                current_price,
                current_qty,
                tracked,
            });
        }

        print!("\r  Fetching variants… page {} ({} cached)", page, map.len());
        std::io::stdout().flush().ok();

        let page_info = &data["data"]["productVariants"]["pageInfo"];
        if page_info["hasNextPage"].as_bool().unwrap_or(false) {
            cursor = page_info["endCursor"].as_str().map(String::from);
            tokio::time::sleep(Duration::from_millis(500)).await;
        } else {
            break;
        }
    }

    println!();
    info!("Bulk fetch complete: {} variants across {} pages", map.len(), page);
    Ok(map)
}

async fn update_variant(
    client: &Client,
    base_url: &str,
    product: &Product,
    cached: &CachedVariant,
) -> Result<()> {

    if !cached.tracked {
        let item_url = format!("{}/inventory_items/{}.json", base_url, cached.inv_item_id);
        let track_body = json!({
            "inventory_item": {
                "id": cached.inv_item_id,
                "tracked": true
            }
        });
        let _ = client.put(&item_url).json(&track_body).send().await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let mut loc_id = cached.location_id;

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
            "status": "draft",
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
) -> Result<(u32, u32, u32, String)> {
    let upsert = mode == "upsert";

    info!("Bulk sync starting: {} products, mode={}", products.len(), mode);
    println!("  Fetching all Shopify variants (one-time pull)…");

    let shopify_map = fetch_all_variants(client, base_url).await?;
    println!("  {} variants cached from Shopify.", shopify_map.len());

    // ── Pre-scan: diff locally before touching the API ───────────────────────
    struct PendingChange<'a> {
        product: &'a Product,
        cached: &'a CachedVariant,
        change_desc: String,
    }

    let mut to_update: Vec<PendingChange> = Vec::new();
    let mut to_create: Vec<&Product> = Vec::new();
    let mut already_current = 0u32;

    for product in products {
        match shopify_map.get(&product.sku.to_uppercase()) {
            Some(cached) => {
                let price_changed = (cached.current_price - product.price).abs() > 0.001;
                let qty_changed = cached.current_qty != product.inventory_quantity;

                if !price_changed && !qty_changed {
                    already_current += 1;
                    continue;
                }

                let mut parts = Vec::new();
                if qty_changed {
                    parts.push(format!("qty {} → {}", cached.current_qty, product.inventory_quantity));
                }
                if price_changed {
                    parts.push(format!("price ${:.2} → ${:.2}", cached.current_price, product.price));
                }

                to_update.push(PendingChange {
                    product,
                    cached,
                    change_desc: parts.join("  |  "),
                });
            }
            None => {
                if upsert {
                    to_create.push(product);
                } else {
                    already_current += 1;
                }
            }
        }
    }

    println!(
        "\n  Pre-scan complete — {} to update, {} to create, {} already current.",
        to_update.len(), to_create.len(), already_current
    );

    if to_update.is_empty() && to_create.is_empty() {
        println!("  ✔  Everything is already up to date.");
        return Ok((0, 0, already_current, "Everything already up to date.".to_string()));
    }

    println!("  Pushing changes…\n");

    let mut updated = 0u32;
    let mut created = 0u32;
    let mut failed = 0u32;

    // One row per SKU: SKU | PRICE change | QTY change
    struct ChangeRow {
        sku: String,
        price_col: String,
        qty_col: String,
        failed: bool,
        error: String,
    }
    let mut change_rows: Vec<ChangeRow> = Vec::new();
    let mut new_item_lines: Vec<String> = Vec::new();
    let mut failed_lines: Vec<String> = Vec::new();

    for change in &to_update {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let cached = change.cached;
        let product = change.product;

        let price_col = if (cached.current_price - product.price).abs() > 0.001 {
            format!("${:.2} -> ${:.2}", cached.current_price, product.price)
        } else {
            "no change".to_string()
        };
        let qty_col = if cached.current_qty != product.inventory_quantity {
            format!("{} -> {}", cached.current_qty, product.inventory_quantity)
        } else {
            "no change".to_string()
        };

        match update_variant(client, base_url, product, cached).await {
            Ok(_) => {
                println!("  OK  {} -- {}", product.sku, change.change_desc);
                change_rows.push(ChangeRow {
                    sku: product.sku.clone(),
                    price_col,
                    qty_col,
                    failed: false,
                    error: String::new(),
                });
                updated += 1;
            }
            Err(e) => {
                error!("Failed to update SKU {}: {}", product.sku, e);
                println!("  XX  {} -- error: {}", product.sku, e);
                change_rows.push(ChangeRow {
                    sku: product.sku.clone(),
                    price_col,
                    qty_col,
                    failed: true,
                    error: e.to_string(),
                });
                failed_lines.push(format!("    {} -- {}", product.sku, e));
                failed += 1;
            }
        }
    }

    for product in &to_create {
        tokio::time::sleep(Duration::from_millis(600)).await;
        match create_product(client, base_url, product).await {
            Ok(_) => {
                println!("  +  {} -- created as DRAFT (price ${:.2}, qty {})", product.sku, product.price, product.inventory_quantity);
                new_item_lines.push(format!(
                    "    {:<22}  ${:.2}  qty {}  [DRAFT -- review before publishing]",
                    product.sku, product.price, product.inventory_quantity
                ));
                created += 1;
            }
            Err(e) => {
                error!("Failed to create SKU {}: {}", product.sku, e);
                failed_lines.push(format!("    {} -- CREATE FAILED: {}", product.sku, e));
                failed += 1;
            }
        }
    }

    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M");
    let mut report = format!(
        "Shopify Bulk Sync Report -- {}\n{}\n\n",
        timestamp, "=".repeat(65)
    );
    report.push_str(&format!(
        "  Updated : {}\n  Created : {} (as drafts)\n  Skipped : {} (no change)\n  Failed  : {}\n\n",
        updated, created, already_current, failed
    ));

    if !change_rows.is_empty() {
        let success_count = change_rows.iter().filter(|r| !r.failed).count();
        report.push_str(&format!("CHANGES ({}):\n", success_count));
        report.push_str(&format!(
            "    {:<22}  {:<24}  {}\n",
            "SKU", "PRICE", "QTY"
        ));
        report.push_str(&format!("    {}\n", "-".repeat(65)));
        for row in &change_rows {
            if !row.failed {
                report.push_str(&format!(
                    "    {:<22}  {:<24}  {}\n",
                    row.sku, row.price_col, row.qty_col
                ));
            }
        }
        report.push('\n');
    }

    if !new_item_lines.is_empty() {
        report.push_str(&format!("NEW ITEMS ({}) -- entered as drafts, action required:\n", new_item_lines.len()));
        report.push_str(&new_item_lines.join("\n"));
        report.push_str("\n\n");
    }

    if !failed_lines.is_empty() {
        report.push_str(&format!("FAILURES ({}):\n", failed_lines.len()));
        report.push_str(&failed_lines.join("\n"));
        report.push('\n');
    }

    // Save report as CSV to disk
    if updated > 0 || created > 0 || failed > 0 {
        let report_dir = std::path::Path::new("data/reports");
        if let Err(e) = std::fs::create_dir_all(report_dir) {
            warn!("Could not create reports directory: {}", e);
        } else {
            let filename = format!(
                "data/reports/sync_{}.csv",
                chrono::Local::now().format("%Y%m%d_%H%M%S")
            );
            match csv::Writer::from_path(&filename) {
                Ok(mut wtr) => {
                    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
                    wtr.write_record(&["Shopify Bulk Sync Report", &ts, "", ""]).ok();
                    wtr.write_record(&["", "", "", ""]).ok();
                    wtr.write_record(&["Updated", &updated.to_string(), "", ""]).ok();
                    wtr.write_record(&["Created (as drafts)", &created.to_string(), "", ""]).ok();
                    wtr.write_record(&["Skipped (no change)", &already_current.to_string(), "", ""]).ok();
                    wtr.write_record(&["Failed", &failed.to_string(), "", ""]).ok();
                    wtr.write_record(&["", "", "", ""]).ok();

                    if !change_rows.is_empty() {
                        wtr.write_record(&["SKU", "PRICE", "QTY", "STATUS"]).ok();
                        for row in &change_rows {
                            let status = if row.failed {
                                format!("FAILED: {}", row.error)
                            } else {
                                "Updated".to_string()
                            };
                            wtr.write_record(&[&row.sku, &row.price_col, &row.qty_col, &status]).ok();
                        }
                        wtr.write_record(&["", "", "", ""]).ok();
                    }

                    if !to_create.is_empty() {
                        wtr.write_record(&["NEW ITEMS", "PRICE", "QTY", "STATUS"]).ok();
                        for product in &to_create {
                            wtr.write_record(&[
                                &product.sku,
                                &format!("${:.2}", product.price),
                                &product.inventory_quantity.to_string(),
                                "DRAFT - review before publishing",
                            ]).ok();
                        }
                        wtr.write_record(&["", "", "", ""]).ok();
                    }

                    if change_rows.iter().any(|r| r.failed) {
                        wtr.write_record(&["FAILURES", "PRICE", "QTY", "ERROR"]).ok();
                        for row in change_rows.iter().filter(|r| r.failed) {
                            wtr.write_record(&[&row.sku, &row.price_col, &row.qty_col, &row.error]).ok();
                        }
                    }

                    wtr.flush().ok();
                    info!("Sync report saved to {}", filename);
                }
                Err(e) => warn!("Failed to save CSV report to {}: {}", filename, e),
            }
        }
    }

    Ok((updated, created, already_current, report))
}