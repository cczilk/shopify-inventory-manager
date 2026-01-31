use crate::models::Product;
use anyhow::{Context, Result};
use csv::ReaderBuilder;
use std::path::Path;

#[derive(Clone)]
pub struct CsvService;

impl CsvService {
    pub fn new() -> Self {
        Self
    }

    pub async fn read_csv<P: AsRef<Path>>(&self, path: P) -> Result<Vec<Product>> {
        let path = path.as_ref();
        
        let content = tokio::fs::read_to_string(path)
            .await
            .context(format!("Failed to read CSV file: {:?}", path))?;

        let mut reader = ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .from_reader(content.as_bytes());

        let mut products = Vec::new();

        for result in reader.deserialize() {
            let product: Product = result.context("Failed to parse CSV row")?;
            products.push(product);
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