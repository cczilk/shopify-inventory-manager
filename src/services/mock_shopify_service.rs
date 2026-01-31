use crate::models::*;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use serde::{Deserialize, Serialize}; 
use tokio::sync::Mutex;
use tracing::{info};

#[derive(Clone)]
pub struct MockShopifyService {
    inventory: Arc<Mutex<HashMap<String, MockProduct>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MockProduct {
    sku: String,
    title: String,
    price: f64,
    inventory_quantity: i32,
    barcode: String,
    weight: f64,
    description: String,
    cost: f64,
    compare_at_price: f64,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl MockShopifyService {
    pub fn new(_store_url: String, _access_token: String) -> Self {
        info!("MOCK MODE: Initialized mock Shopify service");
        info!("No real API calls will be made");
        
        Self {
            inventory: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn load_from_file(&self, path: &str) -> Result<()> {
        match tokio::fs::read_to_string(path).await {
            Ok(content) => {
                let loaded: HashMap<String, MockProduct> = serde_json::from_str(&content)?;
                let mut inventory = self.inventory.lock().await;
                *inventory = loaded;
                info!("MOCK: Loaded {} products from {}", inventory.len(), path);
                Ok(())
            }
            Err(_) => {
                info!("MOCK: No existing state file found at {}, starting fresh", path);
                Ok(())
            }
        }
    }

    pub async fn update_product_on_shopify(&self, product: &Product) -> Result<()> {
        let variant_payload = ShopifyVariant {
            id: Some(123456789), 
            sku: product.sku.clone(),
            price: format!("{:.2}", product.price),
            compare_at_price: Some(format!("{:.2}", product.compare_at_price)),
            inventory_quantity: Some(product.inventory_quantity),
            barcode: Some(product.barcode.clone()),
            weight: Some(product.weight),
            inventory_item_id: Some(999999999),
        };

        let inventory_payload = InventoryLevelSet {
            location_id: 88888888, 
            inventory_item_id: 999999999,
            available: product.inventory_quantity,
        };

        info!("MOCK API TRANSLATION:");
        info!("[Variant Update] Price: {}, Compare At: {}", variant_payload.price, variant_payload.compare_at_price.as_ref().unwrap());
        info!("[Inventory Set] Available: {} at Location: {}", inventory_payload.available, inventory_payload.location_id);

        self.create_product(product).await?;
        
        Ok(())
    }

    pub async fn create_product(&self, product: &Product) -> Result<()> {
        let mut inventory = self.inventory.lock().await;
        
        if inventory.contains_key(&product.sku) {
            return self.update_internal_state(inventory, product).await;
        }

        let now = chrono::Utc::now();
        let mock_product = MockProduct {
            sku: product.sku.clone(),
            title: product.title.clone(),
            price: product.price,
            inventory_quantity: product.inventory_quantity,
            barcode: product.barcode.clone(),
            weight: product.weight,
            description: product.description.clone(),
            cost: product.cost,
            compare_at_price: product.compare_at_price,
            created_at: now,
            updated_at: now,
        };

        inventory.insert(product.sku.clone(), mock_product);
        
        info!("MOCK: State Updated - SKU: {}", product.sku);
        Ok(())
    }

    async fn update_internal_state(&self, mut inventory: tokio::sync::MutexGuard<'_, HashMap<String, MockProduct>>, product: &Product) -> Result<()> {
        if let Some(existing) = inventory.get_mut(&product.sku) {
            existing.inventory_quantity = product.inventory_quantity;
            existing.price = product.price;
            existing.cost = product.cost;
            existing.compare_at_price = product.compare_at_price;
            existing.updated_at = chrono::Utc::now();
            info!("MOCK: State Updated - SKU: {}", product.sku);
        }
        Ok(())
    }

    pub async fn update_inventory(&mut self, product: &Product) -> Result<()> {
        self.update_product_on_shopify(product).await
    }

    pub async fn get_product_by_sku(&self, sku: &str) -> Result<Option<ShopifyProduct>> {
        let inventory = self.inventory.lock().await;
        
        if let Some(product) = inventory.get(sku) {
            let shopify_product = ShopifyProduct {
                id: Some(12345),
                title: product.title.clone(),
                body_html: Some(product.description.clone()),
                vendor: None,
                product_type: None,
                variants: vec![ShopifyVariant {
                    id: Some(67890),
                    sku: product.sku.clone(),
                    price: product.price.to_string(),
                    compare_at_price: Some(product.compare_at_price.to_string()),
                    inventory_quantity: Some(product.inventory_quantity),
                    barcode: Some(product.barcode.clone()),
                    weight: Some(product.weight),
                    inventory_item_id: Some(99999),
                }],
            };
            
            Ok(Some(shopify_product))
        } else {
            Ok(None)
        }
    }

    pub async fn get_product_count(&self) -> usize {
        let inventory = self.inventory.lock().await;
        inventory.len()
    }

    pub async fn print_inventory_summary(&self) {
        let inventory = self.inventory.lock().await;
        
        println!("\n{}", "=".repeat(60));
        println!("MOCK INVENTORY SUMMARY");
        println!("{}", "=".repeat(60));
        
        if inventory.is_empty() {
            println!("No products in inventory");
        } else {
            let mut items: Vec<_> = inventory.values().collect();
            items.sort_by(|a, b| a.sku.cmp(&b.sku));
            
            for product in items {
                println!("SKU: {}", product.sku);
                println!("  Title: {}", product.title);
                println!("  Sell: ${:.2} | List: ${:.2} | Cost: ${:.2}", product.price, product.compare_at_price, product.cost);
                println!("  Qty: {} | Updated: {}", product.inventory_quantity, product.updated_at.format("%Y-%m-%d %H:%M:%S"));
                println!();
            }
        }
        println!("{}\n", "=".repeat(60));
    }

    pub async fn save_to_file(&self, path: &str) -> Result<()> {
        let inventory = self.inventory.lock().await;
        let json = serde_json::to_string_pretty(&*inventory)?;
        tokio::fs::write(path, json).await?;
        info!("MOCK: Saved inventory state to {}", path);
        Ok(())
    }
}