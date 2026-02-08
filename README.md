# Shopify Inventory Manager

```text
*******************************************************************************
                          SHOPIFY INVENTORY MANAGER
*******************************************************************************

-------------------------------------------------------------------------------
                              General Information
-------------------------------------------------------------------------------
Platform.............: Windows 10/11 x64
Lang.................: Rust / Stable
Burn/Install.........: Extract and Run
-------------------------------------------------------------------------------
                                 Release Notes
-------------------------------------------------------------------------------
Automated Shopify inventory sync via CSV/XLSX. Supports daily scheduling, 
STARTTLS email reporting, and local file state tracking. 

Built-in "Mock Mode" for testing without using your live API.

-------------------------------------------------------------------------------
                                System Workflow
-------------------------------------------------------------------------------

Inventory Source (Commander) -> Watch Folder -> Rust Engine -> Shopify API
                                     |
                                     └──> Archive (/data/processed)
                                     └──> Status Email (SMTP)

1. DATA SOURCE:  Inventory file is exported from Commander.
2. DETECTION:    System monitors the Watch Folder for new CSV/XLSX files.
3. PROCESSING:   Rust engine validates data and pushes updates to Shopify API.
4. ARCHIVING:    Processed files are moved to /data/processed with timestamps.
5. REPORTING:    A summary report is dispatched via SMTP email (if configured).

-------------------------------------------------------------------------------
                               Installation Notes
-------------------------------------------------------------------------------

1. CONFIGURATION:
   - Locate '.env' in the root folder.
   - Open with Notepad and update the following:
     * SHOPIFY_ACCESS_TOKEN: Your Admin API token (Pre-configured).
     * SMTP INFORMATION: Enter all SMTP details under # Email Alert Settings.
     * WATCH_FOLDER: Set to Commander export folder or 'data/incoming'.
     * SYNC_HOUR/MINUTE: Daily update time (Pre-set to 17:30 / 5:30 PM).
   - Save and exit.

2. DIRECTORIES:
   - Ensure /data/incoming and /data/processed folders exist in the root.
   - (Note: /data/incoming is not needed if pointing directly to Commander).
   - Place your CSV or XLSX files into the designated folder to begin.

3. EXECUTION:
   - START_SERVICE.bat: Launches the 24/7 background automation timer.
   - OPEN_MENU.bat:     Launches the manual interface for instant updates.

4. BACKGROUND PERSISTENCE:
   - To auto-start on Windows boot:
     a. Press Win+R, type 'shell:startup', hit Enter.
     b. Right-click 'START_SERVICE.bat' -> Create Shortcut.
     c. Move that shortcut into the startup folder.

-------------------------------------------------------------------------------
                                  Usage Tips
-------------------------------------------------------------------------------

- API REFRESH: The system handles API token rotation automatically. However, 
  if you encounter connectivity issues, you can force an update by using 
  'OPEN_MENU.bat' and selecting "5. Refresh API Token Manually".

- MOCK MODE: Set MOCK_MODE=true in .env to safely simulate updates. The app 
  will show you what it WOULD do without changing your live Shopify data.

- EMAIL: If you choose not to use email, leave the SMTP fields blank. The 
  system will still process all updates and move files normally.

- FILE HANDLING: Once a file is processed, it is moved to /data/processed 
  with a timestamp. If you need to re-run a file, move it back to /incoming.

- ERRORS: If the app window closes immediately, check your .env for typos 
  or missing folders. Every line in the .env must be correctly formatted.

-------------------------------------------------------------------------------
*******************************************************************************