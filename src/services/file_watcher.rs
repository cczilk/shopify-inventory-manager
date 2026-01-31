use anyhow::{Result, anyhow};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher, Event};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info};

#[derive(Clone)] 
pub struct FileWatcher {
    pub watch_path: PathBuf,
    latest_file: Arc<Mutex<Option<PathBuf>>>,
    processed_files: Arc<Mutex<std::collections::HashSet<PathBuf>>>,
}
impl FileWatcher {
    pub fn new<P: AsRef<Path>>(watch_path: P) -> Self {
        Self {
            watch_path: watch_path.as_ref().to_path_buf(),
            latest_file: Arc::new(Mutex::new(None)),
            processed_files: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    pub async fn move_to_processed(&self, path: &PathBuf) -> Result<PathBuf> {
        let file_name = path.file_name().ok_or_else(|| anyhow!("Invalid filename"))?;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        
        let processed_dir = Path::new("data/processed");
        tokio::fs::create_dir_all(processed_dir).await?;
        
        let new_path = processed_dir.join(format!("{}_{}", timestamp, file_name.to_string_lossy()));
        
        tokio::fs::rename(path, &new_path).await?;
        info!("Successfully moved processed file to: {:?}", new_path);
        Ok(new_path)
    }

    pub fn is_excel_file(path: &Path) -> bool {
        if let Some(ext) = path.extension() {
            let ext_str = ext.to_string_lossy().to_lowercase();
            ext_str == "xls" || ext_str == "xlsx"
        } else { false }
    }

    pub async fn start_watching(&self) -> Result<RecommendedWatcher> {
        let latest_file = self.latest_file.clone();
        let watch_path = self.watch_path.clone();
        let canonical_watch = std::fs::canonicalize(&watch_path).unwrap_or_else(|_| watch_path.clone());

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                let latest_file = latest_file.clone();
                let canonical_watch = canonical_watch.clone();
                if let Ok(event) = res {
                    for path in event.paths {
                        let is_valid = path.extension().map_or(false, |ext| {
                            let s = ext.to_string_lossy().to_lowercase();
                            s == "csv" || s == "xls" || s == "xlsx"
                        });
                        
                        if is_valid {
                            if let Some(parent) = path.parent() {
                                let canonical_parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
                                if canonical_parent == canonical_watch {
                                    let mut latest = latest_file.blocking_lock();
                                    *latest = Some(path);
                                }
                            }
                        }
                    }
                }
            },
            Config::default(),
        )?;

        watcher.watch(&self.watch_path, RecursiveMode::NonRecursive)?;
        Ok(watcher)
    }

    pub async fn get_latest_file(&self) -> Option<PathBuf> {
        let latest = self.latest_file.lock().await;
        latest.clone()
    }

    pub async fn scan_for_latest(&self) -> Result<Option<PathBuf>> {
        let mut entries = tokio::fs::read_dir(&self.watch_path).await?;
        let mut latest_file: Option<(PathBuf, std::time::SystemTime)> = None;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("csv") {
                let metadata = tokio::fs::metadata(&path).await?;
                let modified = metadata.modified()?;
                match latest_file {
                    None => latest_file = Some((path.clone(), modified)),
                    Some((_, latest_time)) if modified > latest_time => latest_file = Some((path.clone(), modified)),
                    _ => {}
                }
            }
        }
        Ok(latest_file.map(|(p, _)| p))
    }

    pub async fn mark_processed(&self, path: &PathBuf) {
        let mut processed = self.processed_files.lock().await;
        processed.insert(path.clone());
    }

    pub async fn clear_latest(&self) {
        let mut latest = self.latest_file.lock().await;
        *latest = None;
    }
}