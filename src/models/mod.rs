use serde::{Deserialize, Deserializer, Serialize};

fn deserialize_sku<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let raw = String::deserialize(d)?;
    Ok(raw.trim().to_string())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Product {
    #[serde(rename = "Item No.", deserialize_with = "deserialize_sku")]
    pub sku: String,
    #[serde(rename = "Description")]
    pub title: String,
    #[serde(default, rename = "Category")]
    pub handle: String,
    #[serde(rename = "Sell")]
    pub price: f64,
    #[serde(rename = "On Hand")]
    pub inventory_quantity: i32,
    #[serde(default)]
    pub barcode: String,
    #[serde(default)]
    pub weight: f64,
    #[serde(default, rename = "Extended Sell")]
    pub description: String,
    #[serde(rename = "Cost")]
    pub cost: f64,
    #[serde(default, rename = "List")]
    pub compare_at_price: f64,
    #[serde(default, rename = "OEM")]
    pub oem: String,
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