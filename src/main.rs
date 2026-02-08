mod models;
mod services;

use anyhow::Result;
use clokwerk::{AsyncScheduler, TimeUnits, Job}; 
use std::io::Write; // Required for .flush()
use services::{
    csv_service::CsvService, 
    shopify_service::ShopifyService, 
    mock_shopify_service::MockShopifyService,
    file_watcher::FileWatcher,
    excel_converter::ExcelConverter,
    email_service::EmailService, 
};
use std::env;
use std::time::Duration;
use std::path::PathBuf;
use tracing::{info, error, debug};
use notify::{Watcher, PollWatcher, Config, RecursiveMode};
use chrono::{Local, Timelike, Duration as ChronoDuration};

enum ShopifyClient {
    Real(ShopifyService),
    Mock(MockShopifyService),
}

impl ShopifyClient {
    async fn create_product(&mut self, product: &models::Product) -> Result<()> {
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
    // --- FORCE LOAD ENV FROM EXE DIRECTORY ---
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let env_path = exe_dir.join(".env");
            dotenv::from_path(&env_path).ok();
        }
    }
    dotenv::dotenv().ok();
    
    tracing_subscriber::fmt::init();
    
    // --- CRITICAL DEBUG CHECK ---
    let check_hour = env::var("SYNC_HOUR").unwrap_or_else(|_| "NOT_SET".to_string());
    let check_user = env::var("SMTP_USER").unwrap_or_else(|_| "NOT_SET".to_string());
    info!("Starting Shopify Inventory Manager");
    info!("ENV LOAD CHECK -> SYNC_HOUR: {}, SMTP_USER: {}", check_hour, check_user);

    let args: Vec<String> = env::args().collect();
    let store_url = env::var("SHOPIFY_STORE_URL").unwrap_or_default();
    let access_token = env::var("SHOPIFY_ACCESS_TOKEN").unwrap_or_default();
    let watch_folder = env::var("WATCH_FOLDER").unwrap_or_else(|_| "data/incoming".to_string());
    let mock_mode = env::var("MOCK_MODE").unwrap_or_else(|_| "false".to_string()).to_lowercase() == "true";
    let is_service = args.contains(&"--service".to_string()) || env::var("SERVICE_MODE").unwrap_or_else(|_| "false".to_string()).to_lowercase() == "true";

    let mut shopify = if mock_mode {
        ShopifyClient::Mock(MockShopifyService::new(store_url.clone(), access_token.clone()))
    } else {
        ShopifyClient::Real(ShopifyService::new(store_url.clone(), access_token.clone()))
    };

    let email_service = EmailService::new();

    let mut scheduler = AsyncScheduler::new();
    let shopify_for_sched = shopify.clone();
    let email_for_sched = email_service.clone();

    scheduler.every(1.day()).at("03:00").run(move || {
        let mut shop = shopify_for_sched.clone();
        let mail = email_for_sched.clone();
        async move {
            match shop.refresh_token().await {
                Ok(_) => info!("Background Task: Daily token refresh complete."),
                Err(e) => {
                    error!("Token refresh FAILED: {}", e);
                    mail.send_error_alert(&format!("System failed to refresh Shopify Token: {}", e));
                }
            }
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

    if is_service {
        info!("Service mode: Initializing daily sync timer...");
        loop {
            // Re-read env inside loop to stay fresh
            dotenv::dotenv().ok();
            let sync_hour: u32 = env::var("SYNC_HOUR").unwrap_or_else(|_| "0".to_string()).trim().parse().unwrap_or(0);
            let sync_min: u32 = env::var("SYNC_MINUTE").unwrap_or_else(|_| "0".to_string()).trim().parse().unwrap_or(0);

            let now = Local::now();
            let mut next_run = now.date_naive()
                .and_hms_opt(sync_hour, sync_min, 0)
                .expect("Invalid time")
                .and_local_timezone(Local)
                .unwrap();

            if now >= next_run {
                next_run = next_run + ChronoDuration::days(1);
            }

            let wait_duration = next_run - now;
            info!("Current local time: {}", now.format("%H:%M:%S"));
            info!("Next sync scheduled for: {}", next_run.format("%Y-%m-%d %H:%M:%S"));
            info!("Sleeping for {} minutes...", wait_duration.num_minutes());
            
            // Sleep until target reached
            while Local::now() < next_run {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }

            info!("Sync time reached! Processing files...");

            let mut report_body = String::new();
            let mut total_synced = 0;
            let mut total_failed = 0;

            if let Ok(entries) = std::fs::read_dir(&watch_folder) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        info!("Processing: {:?}", path.file_name());
                        match process_single_file(&path, &csv_service, &mut shopify, &file_watcher).await {
                            Ok(count) => {
                                total_synced += count;
                                report_body.push_str(&format!("✅ {:?} ({} items)\n", path.file_name(), count));
                            },
                            Err(e) => {
                                total_failed += 1;
                                report_body.push_str(&format!("❌ {:?} - {}\n", path.file_name(), e));
                            }
                        }
                    }
                }
            }

            if total_synced > 0 || total_failed > 0 {
                let subject = format!("Shopify Daily Sync Report - {}", Local::now().format("%Y-%m-%d"));
                let final_body = format!(
                    "Daily Inventory Summary\n------------------------\nTotal Items Updated: {}\nTotal Files Failed: {}\n\nDetails:\n{}", 
                    total_synced, total_failed, report_body
                );
                email_service.send_report(&subject, &final_body);
                info!("Daily report email sent.");
            }

            // Small sleep to move past the current minute
            tokio::time::sleep(Duration::from_secs(61)).await;
        }
    } else {
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
                    if let Ok(count) = update_inventory_from_file(&csv_service, &mut shopify, path_str.trim()).await {
                        let _ = shopify.save_state().await;
                        let _ = file_watcher.move_to_processed(&path).await;
                        println!("Sync complete ({} items).", count);
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
                    }
                }
                "3" => println!("Running in background..."),
                "4" => {
                    let _ = run_continuous_watch(csv_service.clone(), shopify.clone(), file_watcher.clone()).await;
                }
                "5" => {
                    let _ = shopify.refresh_token().await;
                    println!("Token refreshed.");
                }
                "6" => {
                    println!("\n--- SYSTEM STATUS ---");
                    println!("Store: {}", store_url);
                    println!("Email: {}", check_user);
                    println!("Sync:  {}:{}", check_hour, env::var("SYNC_MINUTE").unwrap_or_else(|_| "0".to_string()));
                }
                "7" => break,
                _ => println!("Invalid selection."),
            }
        }
    }
    Ok(())
}

async fn process_single_file(path: &PathBuf, csv_service: &CsvService, shopify: &mut ShopifyClient, file_watcher: &FileWatcher) -> Result<u32> {
    let converter = ExcelConverter::new();
    let csv_path = if FileWatcher::is_excel_file(path) {
        let out = path.with_extension("csv");
        converter.convert_to_shopify_csv(path.to_str().unwrap(), out.to_str().unwrap()).await?;
        out
    } else {
        path.clone()
    };
    match update_inventory_from_file(csv_service, shopify, &csv_path.to_string_lossy()).await {
        Ok(count) => {
            let _ = file_watcher.move_to_processed(path).await;
            Ok(count)
        }
        Err(e) => Err(e)
    }
}

async fn bulk_add_products_from_file(csv_service: &CsvService, shopify: &mut ShopifyClient, file_path: &str) -> Result<()> {
    let products = csv_service.read_csv(file_path).await?;
    let mut success_count = 0;
    for product in &products {
        match shopify.create_product(product).await {
            Ok(_) => { success_count += 1; }
            Err(e) => println!("Error {}: {}", product.sku, e),
        }
    }
    println!("Bulk add complete. {} added.", success_count);
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
    while let Some(path) = rx.recv().await {
        if path.is_dir() || path.extension().map_or(true, |ext| ext == "tmp") { continue; }
        let _ = process_single_file(&path, &csv_service, &mut shopify, &file_watcher).await;
    }
    Ok(())
}

async fn update_inventory_from_file(csv_service: &CsvService, shopify: &mut ShopifyClient, file_path: &str) -> Result<u32> {
    let products = csv_service.read_csv(file_path).await?;
    let mut count = 0;
    for product in &products { 
        if shopify.update_inventory(product).await.is_ok() { 
            count += 1; 
        }
    }
    Ok(count)
}