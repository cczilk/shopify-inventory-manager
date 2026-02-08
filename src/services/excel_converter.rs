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
        let oem_idx = self.find_column_index(&headers, &["OEM", "Brand", "Manufacturer"]).ok();
        let cat_idx = self.find_column_index(&headers, &["Category", "Type", "Dept"]).ok();

        let mut csv_records = Vec::new();
        
        
        csv_records.push(vec![
            "Item No.".to_string(),
            "Description".to_string(),
            "Sell".to_string(),
            "On Hand".to_string(),
            "Cost".to_string(),
            "List".to_string(),
            "OEM".to_string(),
            "Category".to_string(),
            "Extended Sell".to_string(), 
        ]);

        let mut product_count = 0;

        for (_row_num, row) in range.rows().enumerate().skip(header_row_idx + 1) {
            if row.is_empty() || self.is_header_or_total_row(row, item_no_idx) { continue; }

            let sku = self.get_cell_value(row, item_no_idx).trim().to_string();
            if sku.is_empty() { continue; }

            let title = self.get_cell_value(row, description_idx).trim().to_string();
            let price_str = self.get_cell_value(row, sell_idx);
            let quantity_str = self.get_cell_value(row, on_hand_idx);
            
            let list_str = list_idx.map(|idx| self.get_cell_value(row, idx)).unwrap_or_default();
            let cost_str = cost_idx.map(|idx| self.get_cell_value(row, idx)).unwrap_or_default();
            let oem_str = oem_idx.map(|idx| self.get_cell_value(row, idx)).unwrap_or_else(|| "AMS".to_string());
            let cat_str = cat_idx.map(|idx| self.get_cell_value(row, idx)).unwrap_or_else(|| "Parts".to_string());

            let price = self.parse_price(&price_str).unwrap_or(0.0);
            let list_price = self.parse_price(&list_str).unwrap_or(0.0);
            let cost = self.parse_price(&cost_str).unwrap_or(0.0);
            let quantity = self.parse_quantity(&quantity_str).unwrap_or(0);

            
            csv_records.push(vec![
                sku,
                title.clone(),
                format!("{:.2}", price),
                quantity.to_string(),
                format!("{:.2}", cost),
                format!("{:.2}", list_price),
                oem_str,
                cat_str,
                title, 
            ]);

            product_count += 1;
        }

        let mut wtr = csv::Writer::from_path(output_csv_path).context("Failed to create CSV")?;
        for record in csv_records { wtr.write_record(&record)?; }
        wtr.flush()?;

        info!("Successfully converted {} products.", product_count);
        Ok(product_count)
    }

    fn find_column_index(&self, headers: &[Data], possible_names: &[&str]) -> Result<usize> {
        for (idx, cell) in headers.iter().enumerate() {
            let cell_str = cell.to_string().trim().to_lowercase();
            for &name in possible_names {
                if cell_str == name.to_lowercase() {
                    return Ok(idx);
                }
            }
        }
        Err(anyhow!("Required column not found among: {:?}", possible_names))
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
        let first_cell = row[0].to_string().trim().to_lowercase();
        if first_cell.is_empty() || first_cell.contains("total") || first_cell.contains("inventory") {
            return true;
        }
        let sku = self.get_cell_value(row, item_no_idx);
        sku.trim().is_empty()
    }
}