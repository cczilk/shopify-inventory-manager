mod models;
mod services;

use anyhow::Result;
use clokwerk::{AsyncScheduler, TimeUnits, Job}; 
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
use std::path::PathBuf;
use tracing::{info, error, debug};
use notify::{Watcher, PollWatcher, Config, RecursiveMode};

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
    dotenv().ok();
    tracing_subscriber::fmt::init();
    
    let store_url = env::var("SHOPIFY_STORE_URL").unwrap_or_default();
    let access_token = env::var("SHOPIFY_ACCESS_TOKEN").unwrap_or_default();
    let watch_folder = env::var("WATCH_FOLDER").unwrap_or_else(|_| "data/incoming".to_string());
    let mock_mode = env::var("MOCK_MODE").unwrap_or_else(|_| "false".to_string()).to_lowercase() == "true";

    info!("Starting Shopify Inventory Manager");

    let mut shopify = if mock_mode {
        ShopifyClient::Mock(MockShopifyService::new(store_url.clone(), access_token.clone()))
    } else {
        ShopifyClient::Real(ShopifyService::new(store_url.clone(), access_token.clone()))
    };

    
    let mut scheduler = AsyncScheduler::new();
    let shopify_for_sched = shopify.clone();
    scheduler.every(1.day()).at("03:00").run(move || {
        let mut shop = shopify_for_sched.clone();
        async move {
            let _ = shop.refresh_token().await;
            info!("Background Task: Daily token refresh complete.");
        }
    });

    tokio::spawn(async move {
        loop {
            scheduler.run_pending().await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
    
    let csv_service = CsvService::new();
    let file_watcher = FileWatcher::new(&watch_folder);

    loop {
        println!("\nShopify Inventory Manager");
        println!("1. Update inventory (one-time CSV)");
        println!("2. Bulk add new products (CSV, XLS, or XLSX)");
        println!("3. Start scheduled updates (Stay in menu)");
        println!("4. Watch folder continuously (Full Automation)");
        println!("5. Refresh API Token Manually");
        println!("6. Check System Status");
        println!("7. Exit");
        print!("Select an option: ");
        
        use std::io::Write;
        std::io::stdout().flush()?;
        
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        
        match input.trim() {
            "1" => {
                print!("Enter CSV path: ");
                std::io::stdout().flush()?;
                let mut path_str = String::new();
                std::io::stdin().read_line(&mut path_str)?;
                let path = PathBuf::from(path_str.trim());
                
                if update_inventory_from_file(&csv_service, &mut shopify, path_str.trim()).await.is_ok() {
                    let _ = shopify.save_state().await;
                    let _ = file_watcher.move_to_processed(&path).await;
                    println!("Sync complete. File moved to processed.");
                }
            }
            "2" => {
                print!("Enter file path (CSV/XLSX): ");
                std::io::stdout().flush()?;
                let mut path_str = String::new();
                std::io::stdin().read_line(&mut path_str)?;
                let path = PathBuf::from(path_str.trim());

                
                let target_path = if FileWatcher::is_excel_file(&path) {
                    let out = path.with_extension("csv");
                    let converter = ExcelConverter::new();
                    match converter.convert_to_shopify_csv(path.to_str().unwrap(), out.to_str().unwrap()).await {
                        Ok(_) => out.to_string_lossy().to_string(),
                        Err(e) => { error!("Conversion failed: {}", e); continue; }
                    }
                } else {
                    path_str.trim().to_string()
                };

                if bulk_add_products_from_file(&csv_service, &mut shopify, &target_path).await.is_ok() {
                    let _ = shopify.save_state().await;
                    let _ = file_watcher.move_to_processed(&path).await;
                    println!("Bulk add complete. Original file moved to processed.");
                }
            }
            "3" => {
                println!("Scheduled updates active in background. Watching folder at intervals...");
            }
            "4" => {
                println!("Starting Continuous Watch. Press Ctrl+C to return to menu.");
                let _ = run_continuous_watch(csv_service.clone(), shopify.clone(), file_watcher.clone()).await;
            }
            "5" => {
                let _ = shopify.refresh_token().await;
                println!("Token manually refreshed.");
            }
            "6" => {
                println!("\n--- SYSTEM STATUS ---");
                println!("Store URL:   {}", store_url);
                println!("Watch Path:  {}", watch_folder);
                println!("Mode:        {}", if mock_mode { "MOCK (Simulation)" } else { "REAL (Production)" });
            }
            "7" => break,
            _ => println!("Invalid selection."),
        }
    }
    
    Ok(())
}

async fn bulk_add_products_from_file(csv_service: &CsvService, shopify: &mut ShopifyClient, file_path: &str) -> Result<()> {
    let products = csv_service.read_csv(file_path).await?;
    println!("Starting bulk add for {} products...", products.len());
    let mut success_count = 0;
    for product in &products {
        match shopify.create_product(product).await {
            Ok(_) => { success_count += 1; println!("Created: {}", product.sku); }
            Err(e) => println!("Failed to create: {} - Error: {}", product.sku, e),
        }
    }
    println!("Bulk add complete. {} products added.", success_count);
    Ok(())
}

async fn run_continuous_watch(csv_service: CsvService, mut shopify: ShopifyClient, file_watcher: FileWatcher) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(10);
    let config = Config::default().with_poll_interval(Duration::from_secs(2));
    let mut watcher = PollWatcher::new(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            if event.kind.is_create() || event.kind.is_modify() {
                for path in event.paths { let _ = tx.blocking_send(path); }
            }
        }
    }, config)?;

    watcher.watch(std::path::Path::new(&file_watcher.watch_path), RecursiveMode::Recursive)?;
    info!("Watcher active. Monitoring {}...", file_watcher.watch_path.display());

    let converter = ExcelConverter::new();
    
    while let Some(path) = rx.recv().await {
        if path.is_dir() || path.extension().map_or(true, |ext| ext == "tmp") { continue; }
        let csv_path = if FileWatcher::is_excel_file(&path) {
            let out = path.with_extension("csv");
            if converter.convert_to_shopify_csv(path.to_str().unwrap(), out.to_str().unwrap()).await.is_ok() { out } else { continue }
        } else { path.clone() };

        if update_inventory_from_file(&csv_service, &mut shopify, &csv_path.to_string_lossy()).await.is_ok() {
            let _ = file_watcher.mark_processed(&path).await;
        }
    }
    Ok(())
}

async fn update_inventory_from_file(csv_service: &CsvService, shopify: &mut ShopifyClient, file_path: &str) -> Result<u32> {
    let products = csv_service.read_csv(file_path).await?;
    let mut count = 0;
    for product in &products { 
        if shopify.update_inventory(product).await.is_ok() { 
            count += 1; 
            println!("Synced: {}", product.sku);
        }
    }
    info!("Processed {} items.", count);
    Ok(count)
}