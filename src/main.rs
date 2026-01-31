mod models;
mod services;

use anyhow::Result;
use clokwerk::{AsyncScheduler, TimeUnits};
use dotenv::dotenv;
use services::{
    csv_service::CsvService, 
    shopify_service::ShopifyService, 
    mock_shopify_service::MockShopifyService,
    file_watcher::FileWatcher,
    excel_converter::ExcelConverter,
};
use std::env;
use std::time::Duration;
use tracing::{info, error, warn, debug};
use std::path::PathBuf;

enum ShopifyClient {
    Real(ShopifyService),
    Mock(MockShopifyService),
}

impl ShopifyClient {
    async fn create_product(&self, product: &models::Product) -> Result<()> {
        match self {
            ShopifyClient::Real(s) => s.create_product(product).await,
            ShopifyClient::Mock(s) => s.update_product_on_shopify(product).await,
        }
    }

    async fn update_inventory(&mut self, product: &models::Product) -> Result<()> {
        match self {
            ShopifyClient::Real(s) => s.update_inventory(product).await,
            ShopifyClient::Mock(s) => s.update_product_on_shopify(product).await,
        }
    }

    async fn print_summary(&self) {
        if let ShopifyClient::Mock(s) = self {
            s.print_inventory_summary().await;
        }
    }

    async fn save_state(&self) -> Result<()> {
        if let ShopifyClient::Mock(s) = self {
            s.save_to_file("data/processed/mock_inventory.json").await?;
        }
        Ok(())
    }

    async fn refresh_token(&mut self) -> Result<()> {
        if let ShopifyClient::Real(s) = self {
            s.refresh_daily_token().await?;
        }
        Ok(())
    }
}

impl Clone for ShopifyClient {
    fn clone(&self) -> Self {
        match self {
            ShopifyClient::Real(s) => ShopifyClient::Real(s.clone()),
            ShopifyClient::Mock(s) => ShopifyClient::Mock(s.clone()),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    dotenv().ok();
    
    let store_url = env::var("SHOPIFY_STORE_URL").unwrap_or_else(|_| "test-store.myshopify.com".to_string());
    let access_token = env::var("ACCESS_TOKEN").or_else(|_| env::var("SHOPIFY_ACCESS_TOKEN")).unwrap_or_else(|_| "mock_token".to_string());
    let update_interval_hours: f64 = env::var("UPDATE_INTERVAL_HOURS").unwrap_or_else(|_| "24".to_string()).parse().expect("UPDATE_INTERVAL_HOURS must be a number");
    let watch_folder = env::var("WATCH_FOLDER").unwrap_or_else(|_| "data/incoming".to_string());
    let mock_mode = env::var("MOCK_MODE").unwrap_or_else(|_| "false".to_string()).to_lowercase() == "true";
    let service_mode = env::var("SERVICE_MODE").unwrap_or_else(|_| "false".to_string()).to_lowercase() == "true";

    info!("Starting Shopify Inventory Manager");

    let mut shopify = if mock_mode {
        let mock_service = MockShopifyService::new(store_url, access_token);
        let _ = mock_service.load_from_file("data/processed/mock_inventory.json").await;
        ShopifyClient::Mock(mock_service)
    } else {
        ShopifyClient::Real(ShopifyService::new(store_url, access_token))
    };

    if let ShopifyClient::Real(ref mut service) = shopify {
        let mut service_clone = service.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(86400)).await;
                let _ = service_clone.refresh_daily_token().await;
            }
        });
    }
    
    let csv_service = CsvService::new();
    let file_watcher = FileWatcher::new(&watch_folder);

    if service_mode {
        info!("Service Mode active: Starting continuous watch");
        run_continuous_watch(csv_service, shopify, file_watcher).await?;
        return Ok(());
    }

    loop {
        println!("\nShopify Inventory Manager");
        println!("1. Update inventory (one-time CSV)");
        println!("2. Bulk add new products (CSV, XLS, or XLSX)");
        println!("3. Start scheduled updates");
        println!("4. Watch folder continuously");
        println!("5. Refresh API Token Manually");
        println!("6. Exit");
        
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        
        match input.trim() {
            "1" => {
                let path_str = get_user_input("Enter CSV file path: ")?;
                let path = PathBuf::from(&path_str);
                if update_inventory_from_file(&csv_service, &mut shopify, &path_str).await.is_ok() {
                    let _ = shopify.save_state().await;
                    if let Ok(new_path) = file_watcher.move_to_processed(&path).await {
                        println!("File moved to: {:?}", new_path);
                    }
                }
            }
            "2" => {
                let path_str = get_user_input("Enter file path (CSV, XLS, or XLSX): ")?;
                let path = PathBuf::from(&path_str);
                let csv_path_str = if path_str.ends_with(".xls") || path_str.ends_with(".xlsx") {
                    let converter = ExcelConverter::new();
                    let output_path = format!("{}_converted.csv", path_str.trim_end_matches(".xls").trim_end_matches(".xlsx"));
                    match converter.convert_to_shopify_csv(&path_str, &output_path).await {
                        Ok(_) => output_path,
                        Err(e) => {
                            error!("Conversion failed: {}", e);
                            continue;
                        }
                    }
                } else {
                    path_str.clone()
                };

                if bulk_add_products_from_file(&csv_service, &mut shopify, &csv_path_str).await.is_ok() {
                    if let Ok(new_path) = file_watcher.move_to_processed(&path).await {
                        println!("Original file moved to: {:?}", new_path);
                    }
                }
            }
            "3" => run_scheduled_updates(csv_service.clone(), shopify.clone(), file_watcher.clone(), update_interval_hours).await?,
            "4" => run_continuous_watch(csv_service.clone(), shopify.clone(), file_watcher.clone()).await?,
            "5" => {
                let _ = shopify.refresh_token().await;
            }
            "6" => break,
            _ => println!("Invalid option"),
        }
    }
    Ok(())
}

fn get_user_input(prompt: &str) -> Result<String> {
    use std::io::{stdout, Write};
    print!("{}", prompt);
    stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

async fn update_inventory_from_file(csv_service: &CsvService, shopify: &mut ShopifyClient, file_path: &str) -> Result<()> {
    let products = csv_service.read_csv(file_path).await?;
    println!("Found {} products in {}", products.len(), file_path);
    let mut success_count = 0;
    for product in &products { 
        match shopify.update_inventory(product).await {
            Ok(_) => {
                success_count += 1;
                println!("Synced: {}", product.sku);
            }
            Err(e) => println!("Failed: {} - Error: {}", product.sku, e),
        }
    }
    println!("Sync complete. {}/{} successfully processed.", success_count, products.len());
    Ok(())
}

async fn bulk_add_products_from_file(csv_service: &CsvService, shopify: &mut ShopifyClient, file_path: &str) -> Result<()> {
    let products = csv_service.read_csv(file_path).await?;
    println!("Starting bulk add for {} products...", products.len());
    let mut success_count = 0;
    for product in &products {
        match shopify.create_product(product).await {
            Ok(_) => {
                success_count += 1;
                println!("Created: {}", product.sku);
            }
            Err(e) => println!("Failed to create: {} - Error: {}", product.sku, e),
        }
    }
    println!("Bulk add complete. {} products added.", success_count);
    Ok(())
}

async fn run_scheduled_updates(csv_service: CsvService, shopify: ShopifyClient, file_watcher: FileWatcher, interval_hours: f64) -> Result<()> {
    let interval_seconds = (interval_hours * 3600.0) as u32;
    let mut scheduler = AsyncScheduler::new();
    let csv_s = csv_service.clone();
    let shop_s = shopify.clone();
    let watch_path = file_watcher.watch_path.clone();

    scheduler.every(interval_seconds.seconds()).run(move || {
        let csv = csv_s.clone();
        let mut shop = shop_s.clone();
        let watcher = FileWatcher::new(watch_path.clone());
        async move {
            if let Ok(Some(path)) = watcher.scan_for_latest().await {
                if update_inventory_from_file(&csv, &mut shop, &path.to_string_lossy()).await.is_ok() {
                    let _ = watcher.move_to_processed(&path).await;
                }
            }
        }
    });

    loop {
        scheduler.run_pending().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn run_continuous_watch(csv_service: CsvService, mut shopify: ShopifyClient, file_watcher: FileWatcher) -> Result<()> {
    let _watcher = file_watcher.start_watching().await?;
    let converter = ExcelConverter::new();
    loop {
        if let Some(file_path) = file_watcher.get_latest_file().await {
            let csv_path = if FileWatcher::is_excel_file(&file_path) {
                let out = file_path.with_extension("csv");
                match converter.convert_to_shopify_csv(&file_path, &out).await {
                    Ok(_) => out,
                    Err(_) => {
                        file_watcher.clear_latest().await;
                        continue;
                    }
                }
            } else { 
                file_path.clone() 
            };

            if update_inventory_from_file(&csv_service, &mut shopify, &csv_path.to_string_lossy()).await.is_ok() {
                file_watcher.mark_processed(&file_path).await;
            }
            file_watcher.clear_latest().await;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}