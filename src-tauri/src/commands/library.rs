use chrono::Utc;
use tauri::State;

use crate::core::app_state::AppState;
use crate::core::error::AppResult;
use crate::core::scanner::{default_exclude_patterns, default_include_patterns, normalize_path};
use crate::models::{
    CreateLibraryRequest, IndexProfile, Job, JobStatus, Library, LibraryStatus,
    ReconnectLibraryRequest, ReconnectLibraryResult,
};

#[tauri::command]
pub async fn create_library(
    state: State<'_, AppState>,
    req: CreateLibraryRequest,
) -> AppResult<Library> {
    let requested_root = std::path::Path::new(&req.root_path);
    if !requested_root.is_absolute() || !requested_root.is_dir() {
        return Err(crate::core::error::AppError::InvalidPath(
            requested_root.to_path_buf(),
        ));
    }
    let root =
        normalize_path(&dunce::canonicalize(requested_root).map_err(|_| {
            crate::core::error::AppError::InvalidPath(requested_root.to_path_buf())
        })?);
    let now = Utc::now().timestamp_millis();
    let library = Library {
        id: uuid::Uuid::new_v4().to_string(),
        name: req.name,
        root_path: root,
        status: LibraryStatus::Idle,
        index_profile: req.index_profile.unwrap_or(IndexProfile::Balanced),
        include_patterns: req
            .include_patterns
            .unwrap_or_else(default_include_patterns),
        exclude_patterns: req
            .exclude_patterns
            .unwrap_or_else(default_exclude_patterns),
        watch_enabled: false,
        last_scan_at: None,
        created_at: now,
        updated_at: now,
    };
    state.db.create_library(&library)?;
    Ok(library)
}

#[tauri::command]
pub async fn list_libraries(state: State<'_, AppState>) -> AppResult<Vec<Library>> {
    state.db.list_libraries()
}

#[tauri::command]
pub async fn get_library(state: State<'_, AppState>, id: String) -> AppResult<Option<Library>> {
    state.db.get_library(&id)
}

#[tauri::command]
pub async fn delete_library(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.db.delete_library(&id)
}

#[tauri::command]
pub async fn start_scan(state: State<'_, AppState>, library_id: String) -> AppResult<Job> {
    enqueue_scan(&state, library_id)
}

#[tauri::command]
pub async fn set_library_watch_enabled(
    state: State<'_, AppState>,
    library_id: String,
    enabled: bool,
) -> AppResult<()> {
    state
        .job_queue
        .set_library_watch_enabled(&library_id, enabled)
}

#[tauri::command]
pub async fn reconnect_library(
    state: State<'_, AppState>,
    req: ReconnectLibraryRequest,
) -> AppResult<ReconnectLibraryResult> {
    let requested_root = std::path::Path::new(&req.root_path);
    if !requested_root.is_absolute() || !requested_root.is_dir() {
        return Err(crate::core::error::AppError::InvalidPath(
            requested_root.to_path_buf(),
        ));
    }
    let root = dunce::canonicalize(requested_root)
        .map_err(|_| crate::core::error::AppError::InvalidPath(requested_root.to_path_buf()))?;
    state.job_queue.reconnect_library(&req.library_id, &root)
}

fn enqueue_scan(state: &AppState, library_id: String) -> AppResult<Job> {
    let library = state
        .db
        .get_library(&library_id)?
        .ok_or_else(|| crate::core::error::AppError::LibraryNotFound(library_id.clone()))?;

    if library.status == LibraryStatus::Scanning {
        return Err(crate::core::error::AppError::Other(
            "素材库正在扫描中".to_string(),
        ));
    }
    if let Some(existing) = state.db.active_scan_job_for_library(&library_id)? {
        let action = if existing.status == JobStatus::Paused {
            "请恢复或取消该暂停任务"
        } else {
            "该任务已在队列中"
        };
        return Err(crate::core::error::AppError::Other(format!(
            "素材库已有扫描任务（{}），{}",
            existing.id, action
        )));
    }

    state
        .job_queue
        .submit(Job::scan(library_id, "等待扫描", 10))
}
