use crate::models::Product;
use anyhow::{Context, Result};
use csv::ReaderBuilder;
use std::path::Path;
use tracing::error;

#[derive(Clone)]
pub struct CsvService;

impl CsvService {
    pub fn new() -> Self {
        Self
    }

pub async fn read_csv(&self, file_path: &str) -> Result<Vec<Product>> {
    let content = std::fs::read_to_string(file_path)?;
    
    
    let clean_content = content.trim_start_matches('\u{feff}');

    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .trim(csv::Trim::All) 
        .from_reader(clean_content.as_bytes());

    let mut products = Vec::new();
    for result in rdr.deserialize() {
        match result {
            Ok(product) => products.push(product),
            Err(e) => {
                error!("CSV Parse Error on row: {}", e);
                return Err(anyhow::anyhow!("Failed to parse CSV row: {}", e));
            }
        }
    }
    Ok(products)
}

    pub async fn create_template<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let template = r#"sku,title,price,inventory_quantity,barcode,weight,description
SAMPLE-001,Sample Product 1,29.99,100,1234567890123,1.5,This is a sample product
SAMPLE-002,Sample Product 2,39.99,50,9876543210987,2.0,Another sample product
"#;

        tokio::fs::write(path.as_ref(), template)
            .await
            .context("Failed to write template CSV")?;

        Ok(())
    }
}