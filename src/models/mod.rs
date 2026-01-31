use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Product {
    pub sku: String,
    pub title: String,
    pub price: f64,
    pub inventory_quantity: i32,
    #[serde(default)]
    pub barcode: String,
    #[serde(default)]
    pub weight: f64,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub compare_at_price: f64, 
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ShopifyProduct {
    pub id: Option<i64>,
    pub title: String,
    pub body_html: Option<String>,
    pub vendor: Option<String>,
    pub product_type: Option<String>,
    pub variants: Vec<ShopifyVariant>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ShopifyVariant {
    pub id: Option<i64>,
    pub sku: String,
    pub price: String,
    pub compare_at_price: Option<String>,
    pub inventory_quantity: Option<i32>,
    pub barcode: Option<String>,
    pub weight: Option<f64>,
    pub inventory_item_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ShopifyProductWrapper {
    pub product: ShopifyProduct,
}

#[derive(Debug, Serialize)]
pub struct InventoryLevelSet {
    pub location_id: i64,
    pub inventory_item_id: i64,
    pub available: i32,
}

#[derive(Debug, Deserialize)]
pub struct ShopifyResponse<T> {
    pub product: Option<T>,
    pub products: Option<Vec<T>>,
}

#[derive(Debug, Deserialize)]
pub struct ShopifyLocation {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct ShopifyLocationsResponse {
    pub locations: Vec<ShopifyLocation>,
}