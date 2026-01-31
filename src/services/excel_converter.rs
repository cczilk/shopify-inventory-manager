use anyhow::{Context, Result, anyhow};
use calamine::{Reader, open_workbook_auto, Data, Range, Sheets};
use std::path::Path;
use tracing::info; 

pub struct ExcelConverter;

impl ExcelConverter {
    pub fn new() -> Self {
        Self
    }

    pub async fn convert_to_shopify_csv<P: AsRef<Path>>(
        &self,
        excel_path: P,
        output_csv_path: P,
    ) -> Result<usize> {
        let excel_path = excel_path.as_ref();
        let output_csv_path = output_csv_path.as_ref();

        info!("Converting Excel file: {:?}", excel_path);

        let mut workbook: Sheets<_> = open_workbook_auto(excel_path)
            .context("Failed to open Excel file. Make sure it's a valid .xls or .xlsx file")?;

        let sheet_names = workbook.sheet_names().to_vec();
        if sheet_names.is_empty() {
            return Err(anyhow!("No worksheets found in Excel file"));
        }
        
        let sheet_name = sheet_names[0].clone();
        info!("Reading worksheet: {}", sheet_name);
        
        let range: Range<Data> = workbook.worksheet_range(&sheet_name)
            .context(format!("Worksheet '{}' not found", sheet_name))?;

        let mut header_row_idx = 0;
        let mut headers: Vec<Data> = Vec::new();
        
        for (idx, row) in range.rows().enumerate() {
            if row.is_empty() { continue; }
            let first_cell = row[0].to_string().to_lowercase();
            if first_cell.contains("oem") || first_cell.contains("category") || first_cell.contains("item no") {
                headers = row.to_vec();
                header_row_idx = idx;
                info!("Found header row at index {}", idx);
                break;
            }
        }
        
        if headers.is_empty() {
            return Err(anyhow!("Could not find header row in Excel file"));
        }
        
        let item_no_idx = self.find_column_index(&headers, &["Item No.", "SKU", "Item No", "Item#"])?;
        let description_idx = self.find_column_index(&headers, &["Description", "Product Name", "Title"])?;
        let on_hand_idx = self.find_column_index(&headers, &["On Hand", "Quantity", "Qty"])?;
        let sell_idx = self.find_column_index(&headers, &["Sell", "Price", "Sell Price"])?;
        let list_idx = self.find_column_index(&headers, &["List", "List Price", "MSRP"]).ok();
        let cost_idx = self.find_column_index(&headers, &["Cost", "Unit Cost", "Wholesale"]).ok();

        let mut csv_records = Vec::new();
        csv_records.push(vec![
            "sku".to_string(), "title".to_string(), "price".to_string(),
            "inventory_quantity".to_string(), "barcode".to_string(),
            "weight".to_string(), "description".to_string(),
            "cost".to_string(), "compare_at_price".to_string(),
        ]);

        let mut product_count = 0;

        for (_row_num, row) in range.rows().enumerate().skip(header_row_idx + 1) {
            if row.is_empty() { continue; }
            
            if self.is_header_or_total_row(row, item_no_idx) {
                continue;
            }

            let sku = self.get_cell_value(row, item_no_idx).trim().to_string();
            if sku.is_empty() { continue; }

            let title = self.get_cell_value(row, description_idx).trim().to_string();
            let price_str = self.get_cell_value(row, sell_idx);
            let quantity_str = self.get_cell_value(row, on_hand_idx);
            
            let list_str = list_idx.map(|idx| self.get_cell_value(row, idx)).unwrap_or_default();
            let cost_str = cost_idx.map(|idx| self.get_cell_value(row, idx)).unwrap_or_default();

            let price = self.parse_price(&price_str).unwrap_or(0.0);
            let list_price = self.parse_price(&list_str).unwrap_or(0.0);
            let cost = self.parse_price(&cost_str).unwrap_or(0.0);
            let quantity = self.parse_quantity(&quantity_str).unwrap_or(0);

            csv_records.push(vec![
                sku.clone(),
                title.clone(),
                format!("{:.2}", price),
                quantity.to_string(),
                sku.clone(),
                "0".to_string(),
                title,
                format!("{:.2}", cost),
                format!("{:.2}", list_price),
            ]);

            product_count += 1;
        }

        let mut wtr = csv::Writer::from_path(output_csv_path).context("Failed to create CSV")?;
        for record in csv_records { wtr.write_record(&record)?; }
        wtr.flush()?;

        info!("Done: {} products converted.", product_count);
        Ok(product_count)
    }

    fn find_column_index(&self, headers: &[Data], possible_names: &[&str]) -> Result<usize> {
        for (idx, cell) in headers.iter().enumerate() {
            let cell_string = cell.to_string();
            let cell_str = cell_string.trim();
            for &name in possible_names {
                if cell_str.eq_ignore_ascii_case(name) {
                    return Ok(idx);
                }
            }
        }
        Err(anyhow!("Required column not found. Checked: {:?}", possible_names))
    }

    fn get_cell_value(&self, row: &[Data], idx: usize) -> String {
        row.get(idx).map(|d| d.to_string()).unwrap_or_default()
    }

    fn parse_price(&self, price_str: &str) -> Option<f64> {
        let cleaned = price_str.replace('$', "").replace(',', "").trim().to_string();
        cleaned.parse::<f64>().ok()
    }

    fn parse_quantity(&self, qty_str: &str) -> Option<i32> {
        let cleaned = qty_str.replace(',', "").trim().to_string();
        cleaned.parse::<f64>().ok().map(|f| f.floor() as i32)
    }

    fn is_header_or_total_row(&self, row: &[Data], item_no_idx: usize) -> bool {
        if row.is_empty() { return true; }
        
        let first_cell = row[0].to_string().trim().to_lowercase();
        
        if first_cell.is_empty() || first_cell.starts_with("total") {
            return true;
        }

        let sku_string = self.get_cell_value(row, item_no_idx);
        let sku_cell = sku_string.trim();
        if sku_cell.is_empty() {
            return true;
        }
        
        false
    }
}