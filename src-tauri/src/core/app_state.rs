use std::path::Path;
#[cfg(feature = "webdriver-e2e")]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tauri::Manager;

use crate::core::cache::CacheManager;
use crate::core::db::Database;
use crate::core::error::AppResult;
use crate::core::job_queue::JobQueue;

pub struct AppState {
    pub db: Arc<Database>,
    pub cache: Arc<CacheManager>,
    pub job_queue: Arc<JobQueue>,
    pub text_analysis: Arc<Mutex<()>>,
}

impl AppState {
    pub fn new(app_data_dir: &Path, app_handle: tauri::AppHandle) -> AppResult<Self> {
        std::fs::create_dir_all(app_data_dir)?;
        let db_path = app_data_dir.join("sceneweaver.db");
        let db = Arc::new(Database::new(db_path));
        db.init()?;

        let cache_root = app_data_dir.join("cache");
        let cache = Arc::new(CacheManager::new(cache_root));
        cache.ensure_dirs()?;

        let job_queue = Arc::new(JobQueue::new(
            Arc::clone(&db),
            Arc::clone(&cache),
            app_handle,
        ));
        job_queue.recover()?;
        super::library_watch::start(Arc::clone(&db), Arc::downgrade(&job_queue));

        Ok(Self {
            db,
            cache,
            job_queue,
            text_analysis: Arc::new(Mutex::new(())),
        })
    }
}

pub fn setup_app_state(app: &mut tauri::App) -> AppResult<()> {
    let default_app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| crate::core::error::AppError::Other(format!("获取数据目录失败: {}", e)))?;
    // The override is compiled only into the explicit desktop E2E binary. Tauri
    // does not interpret arbitrary data-directory environment variables itself.
    #[cfg(feature = "webdriver-e2e")]
    let app_data_dir = std::env::var_os("SCENEWEAVER_E2E_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or(default_app_data_dir);
    #[cfg(not(feature = "webdriver-e2e"))]
    let app_data_dir = default_app_data_dir;
    let app_handle = app.app_handle().clone();
    let state = AppState::new(&app_data_dir, app_handle)?;
    app.manage(state);
    Ok(())
}
