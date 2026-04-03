use crate::services::excel_converter::ExcelConverter;
use crate::services::file_watcher::FileWatcher;
use anyhow::{anyhow, Result};
use reqwest::Client;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use tracing::{info, warn};

#[derive(Debug)]
struct ShopifyVariantRecord {
    variant_id: i64,
    sku: String,
    title: String,
}

#[derive(Debug)]
struct PriceFileRecord {
    sku: String,
    description: String,
}

#[derive(Debug)]
struct SkuCorrection {
    variant_id: i64,
    old_sku: String,
    old_description: String,
    new_sku: String,
    new_description: String,
}

pub async fn run_api_sku_fix(
    client: &Client,
    base_url: &str,
    price_file_path: &str,
) -> Result<()> {
    println!("\n{}", "=".repeat(79));
    println!("  API SKU NORMALIZER  —  Fix Missing Prefixes via Shopify API");
    println!("{}\n", "=".repeat(79));

    println!("[1/4] Fetching all variants from Shopify (this may take a moment)…");
    let shopify_variants = fetch_all_variants(client, base_url).await?;
    println!("      {} variants fetched.", shopify_variants.len());

    println!("[2/4] Loading price file:  {}", price_file_path);
    let csv_path = if FileWatcher::is_excel_file(std::path::Path::new(price_file_path)) {
        let out = std::path::Path::new(price_file_path).with_extension("csv");
        let out_str = out.to_string_lossy().to_string();
        info!("Excel file detected, converting to CSV…");
        ExcelConverter::new()
            .convert_to_shopify_csv(price_file_path, &out_str)
            .await?;
        out_str
    } else {
        price_file_path.to_string()
    };
    let price_records = load_price_file(&csv_path)?;
    println!("      {} SKUs loaded.", price_records.len());

    let suffix_map: HashMap<String, (String, String)> = price_records
        .iter()
        .filter_map(|r| {
            r.sku.find('-').map(|i| {
                let suffix = r.sku[i + 1..].to_uppercase();
                (suffix, (r.sku.clone(), r.description.clone()))
            })
        })
        .collect();

    let exact_set: HashSet<String> = price_records
        .iter()
        .map(|r| r.sku.to_uppercase())
        .collect();

    println!("[3/4] Scanning Shopify variants for mismatched SKUs…");
    let corrections = find_corrections(&shopify_variants, &suffix_map, &exact_set);

    if corrections.is_empty() {
        println!("  ✔  No mismatched SKUs found. Nothing to fix.");
        println!("{}\n", "=".repeat(79));
        return Ok(());
    }

    println!("\n  {:<6} {:<30} {}", "#", "CURRENT SKU", "CORRECTED SKU");
    println!("  {:<6} {:<30} {}", "", "Current Description", "Price File Description");
    println!("  {}", "-".repeat(80));
    for (i, c) in corrections.iter().enumerate() {
        println!("  {:<6} {:<30} {}", i + 1, c.old_sku, c.new_sku);
        println!("  {:<6} {:<30} {}", "", c.old_description, c.new_description);
        println!();
    }
    println!("  Total matches: {}", corrections.len());

    println!("\nEnter numbers to apply (e.g. 1-5, 7, 9-12), or 'all', or 'none' to abort:");
    print!("> ");
    std::io::stdout().flush()?;
    let mut selection = String::new();
    std::io::stdin().read_line(&mut selection)?;
    let selection = selection.trim();

    if selection.eq_ignore_ascii_case("none") || selection.is_empty() {
        println!("  Aborted — no changes made.");
        println!("{}\n", "=".repeat(79));
        return Ok(());
    }

    let selected_indices = if selection.eq_ignore_ascii_case("all") {
        (0..corrections.len()).collect::<Vec<_>>()
    } else {
        parse_selection(selection, corrections.len())?
    };

    if selected_indices.is_empty() {
        println!("  No valid selections — aborted.");
        println!("{}\n", "=".repeat(79));
        return Ok(());
    }

    println!("\n  Applying {} correction(s)…", selected_indices.len());
    let mut success = 0usize;
    let mut failed = 0usize;

    for idx in &selected_indices {
        let correction = &corrections[*idx];
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        match update_variant_sku(client, base_url, correction.variant_id, &correction.new_sku).await {
            Ok(_) => {
                info!("SKU corrected: {} → {}", correction.old_sku, correction.new_sku);
                println!("  ✔  {} → {}", correction.old_sku, correction.new_sku);
                success += 1;
            }
            Err(e) => {
                warn!("Failed to correct SKU {}: {}", correction.old_sku, e);
                println!("  ✗  {} — error: {}", correction.old_sku, e);
                failed += 1;
            }
        }
    }

    println!("\n  Done. {} corrected, {} failed.", success, failed);
    println!("{}\n", "=".repeat(79));
    Ok(())
}

fn parse_selection(input: &str, max_len: usize) -> Result<Vec<usize>> {
    let mut indices = Vec::new();

    for part in input.split(',') {
        let part = part.trim();
        if part.contains('-') {
            let mut sides = part.splitn(2, '-');
            let start: usize = sides.next().unwrap_or("").trim().parse()
                .map_err(|_| anyhow!("Invalid range: '{}'", part))?;
            let end: usize = sides.next().unwrap_or("").trim().parse()
                .map_err(|_| anyhow!("Invalid range: '{}'", part))?;
            if start < 1 || end < start {
                return Err(anyhow!("Invalid range: '{}'", part));
            }
            for n in start..=end {
                if n <= max_len {
                    indices.push(n - 1);
                }
            }
        } else {
            let n: usize = part.parse()
                .map_err(|_| anyhow!("Invalid number: '{}'", part))?;
            if n >= 1 && n <= max_len {
                indices.push(n - 1);
            }
        }
    }

    let mut seen = HashSet::new();
    indices.retain(|i| seen.insert(*i));

    Ok(indices)
}

fn find_corrections(
    shopify: &[ShopifyVariantRecord],
    suffix_map: &HashMap<String, (String, String)>,
    exact_set: &HashSet<String>,
) -> Vec<SkuCorrection> {
    let mut results = Vec::new();

    for variant in shopify {
        if exact_set.contains(&variant.sku.to_uppercase()) {
            continue;
        }

        let shopify_suffix = match variant.sku.find('-') {
            None => variant.sku.to_uppercase(),
            Some(p) if p == 3 => variant.sku[p + 1..].to_uppercase(),
            _ => continue,
        };

        if let Some((correct_sku, correct_desc)) = suffix_map.get(&shopify_suffix) {
            if correct_sku.to_uppercase() != variant.sku.to_uppercase() {
                results.push(SkuCorrection {
                    variant_id: variant.variant_id,
                    old_sku: variant.sku.clone(),
                    old_description: variant.title.clone(),
                    new_sku: correct_sku.clone(),
                    new_description: correct_desc.clone(),
                });
            }
        }
    }

    results
}

async fn fetch_all_variants(client: &Client, base_url: &str) -> Result<Vec<ShopifyVariantRecord>> {
    let url = format!("{}/graphql.json", base_url);
    let mut variants = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
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
                            product {{ title }}
                        }}
                    }}
                }}
            }}"#,
            after_clause
        );

        let body = json!({ "query": query });
        let response = client.post(&url).json(&body).send().await?;
        let data: serde_json::Value = response.json().await?;

        let edges = data["data"]["productVariants"]["edges"]
            .as_array()
            .ok_or_else(|| anyhow!("Unexpected GraphQL response shape"))?;

        for edge in edges {
            let node = &edge["node"];
            let id_raw = node["id"].as_str().unwrap_or("");
            let variant_id = id_raw.split('/').last().unwrap_or("0").parse::<i64>().unwrap_or(0);
            let sku = node["sku"].as_str().unwrap_or("").trim().to_string();
            let title = node["product"]["title"].as_str().unwrap_or("").trim().to_string();
            if !sku.is_empty() {
                variants.push(ShopifyVariantRecord { variant_id, sku, title });
            }
        }

        let page_info = &data["data"]["productVariants"]["pageInfo"];
        if page_info["hasNextPage"].as_bool().unwrap_or(false) {
            cursor = page_info["endCursor"].as_str().map(String::from);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        } else {
            break;
        }
    }

    Ok(variants)
}

async fn update_variant_sku(
    client: &Client,
    base_url: &str,
    variant_id: i64,
    new_sku: &str,
) -> Result<()> {
    let url = format!("{}/variants/{}.json", base_url, variant_id);
    let body = json!({ "variant": { "id": variant_id, "sku": new_sku } });

    let response = client.put(&url).json(&body).send().await?;
    if response.status().is_success() {
        return Ok(());
    }

    let text = response.text().await?;
    if text.contains("Exceeded 2 calls per second") {
        warn!("Rate limited updating variant {}, retrying after 4s…", variant_id);
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        let retry = client.put(&url).json(&body).send().await?;
        if retry.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!("SKU update failed after retry: {}", retry.text().await?))
        }
    } else {
        Err(anyhow!("SKU update failed: {}", text))
    }
}

fn load_price_file(path: &str) -> Result<Vec<PriceFileRecord>> {
    let content = std::fs::read_to_string(path)?;
    let clean = content.trim_start_matches('\u{feff}');

    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .trim(csv::Trim::All)
        .from_reader(clean.as_bytes());

    let headers: Vec<String> = rdr.headers()?.iter().map(String::from).collect();

    let sku_idx = headers
        .iter()
        .position(|h| h.eq_ignore_ascii_case("Item No.") || h.eq_ignore_ascii_case("SKU"))
        .ok_or_else(|| anyhow!("'Item No.' column not found in {}", path))?;

    let desc_idx = headers
        .iter()
        .position(|h| {
            h.eq_ignore_ascii_case("Description")
                || h.eq_ignore_ascii_case("Extended Sell")
                || h.eq_ignore_ascii_case("Title")
        })
        .unwrap_or(sku_idx);

    let mut records = Vec::new();
    for result in rdr.records() {
        let rec = result?;
        let sku = rec.get(sku_idx).unwrap_or("").trim().to_string();
        if !sku.is_empty() {
            let description = rec.get(desc_idx).unwrap_or("").trim().to_string();
            records.push(PriceFileRecord { sku, description });
        }
    }

    Ok(records)
}