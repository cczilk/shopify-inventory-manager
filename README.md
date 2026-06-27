# Shopify Inventory Manager


tool that automates inventory syncing from Commander exports to Shopify. CSV or XLSX file is dropped into the watch folder and it handles the rest,
updating prices, quantities, and creating new products as drafts for review. Includes a daily scheduler, email reports, and a mock mode for safe testing.
Features
```text
Auto-syncs inventory from Commander CSV/XLSX exports
New products enter Shopify as drafts — publish when ready
Daily scheduled sync with email summary reports
Bulk sync mode with local report saving
Mock mode for testing without touching live data
Office 365 and IP relay SMTP support

Built with Rust · Shopify Admin REST API · Windows 10/11
