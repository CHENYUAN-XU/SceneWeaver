use std::path::Path;
use std::sync::Arc;

use tauri::State;

use crate::core::app_state::AppState;
use crate::core::error::{AppError, AppResult};
use crate::core::text_analysis;
use crate::models::{Asset, AssetStatus, AssetTextSource, TextCue};

#[tauri::command]
pub async fn list_asset_text(
    state: State<'_, AppState>,
    asset_id: String,
    source: AssetTextSource,
) -> AppResult<Vec<TextCue>> {
    state.db.list_asset_text(&asset_id, source)
}

#[tauri::command]
pub async fn delete_asset_text(
    state: State<'_, AppState>,
    asset_id: String,
    source: AssetTextSource,
) -> AppResult<()> {
    let _worker = state
        .text_analysis
        .try_lock()
        .map_err(|_| AppError::Other("正在分析本地文字，请稍后重试".into()))?;
    state.db.delete_asset_text(&asset_id, source)
}

#[tauri::command]
pub async fn analyze_asset_text(
    state: State<'_, AppState>,
    asset_id: String,
    source: AssetTextSource,
    language: Option<String>,
    model_path: Option<String>,
) -> AppResult<Vec<TextCue>> {
    let db = Arc::clone(&state.db);
    let cache = Arc::clone(&state.cache);
    let worker = Arc::clone(&state.text_analysis);
    tokio::task::spawn_blocking(move || {
        let _worker = worker
            .try_lock()
            .map_err(|_| AppError::Other("正在分析本地文字，请稍后重试".into()))?;
        let asset = db
            .get_asset(&asset_id)?
            .ok_or(AppError::AssetNotFound(asset_id))?;
        if asset.status != AssetStatus::Indexed {
            return Err(AppError::Other("请先扫描可访问的源素材".into()));
        }
        verify_source(&asset)?;
        let cues = match source {
            AssetTextSource::Ocr => text_analysis::recognize_text(
                &asset,
                &db.list_segments(&asset.id)?,
                &cache,
                language.as_deref().unwrap_or("eng"),
            )?,
            AssetTextSource::Speech => {
                let path = model_path
                    .as_deref()
                    .ok_or_else(|| AppError::Other("请选择本地 Whisper 模型文件".into()))?;
                text_analysis::transcribe(&asset, Path::new(path))?
            }
        };
        verify_source(&asset)?;
        db.store_asset_text(&asset, source, &cues)?;
        Ok(cues)
    })
    .await
    .map_err(|error| AppError::Other(format!("本地文字分析任务异常终止：{error}")))?
}

fn verify_source(asset: &Asset) -> AppResult<()> {
    let metadata = std::fs::metadata(&asset.file_path)?;
    let modified_at = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    if metadata.len() as i64 != asset.size_bytes || modified_at != asset.modified_at {
        return Err(AppError::Other("源文件已变更，请重新扫描后再分析".into()));
    }
    Ok(())
}
