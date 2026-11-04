use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use globset::Glob;
use walkdir::WalkDir;

use crate::core::cache::CacheManager;
use crate::core::db::Database;
use crate::core::error::{AppError, AppResult};
use crate::core::exif::probe_image_capture_time;
use crate::core::ffprobe::probe_media;
use crate::core::fingerprint::{needs_reindex, quick_fingerprint};
use crate::core::job_queue::{JobControl, ProgressUpdate};
use crate::core::thumbnail::ThumbnailService;
use crate::models::{Asset, AssetStatus, IndexProfile, Library, MediaType};

pub struct Scanner {
    db: Arc<Database>,
    cache: CacheManager,
    thumbnail: ThumbnailService,
}

impl Scanner {
    pub fn new(db: Arc<Database>, cache: Arc<CacheManager>) -> Self {
        let cache = CacheManager::new(cache.root());
        Self {
            db,
            thumbnail: ThumbnailService::new(cache.clone()),
            cache,
        }
    }

    pub fn scan_library(
        &self,
        library: &Library,
        control: &JobControl,
        progress: &dyn ProgressUpdate,
    ) -> AppResult<ScanSummary> {
        let root = PathBuf::from(&library.root_path);
        if !root.exists() || !root.is_dir() {
            return Err(AppError::InvalidPath(root));
        }

        let include_set = build_globset(&library.include_patterns);
        let exclude_set = build_globset(&library.exclude_patterns);

        let total = self.count_media_files(&root, &include_set, &exclude_set, control)?;
        progress.report_total(total as i64);

        let scan_marker = Utc::now().timestamp_millis();
        let mut processed = 0usize;
        let mut summary = ScanSummary::default();
        let mut traversal_failed = false;
        for entry in WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_hidden(e))
        {
            wait_for_scan_control(control)?;

            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    traversal_failed = true;
                    summary.errors += 1;
                    log::warn!("跳过无法访问的路径: {error}");
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let rel = path.strip_prefix(&root).unwrap_or(path);
            if !is_media_file(path) {
                continue;
            }
            if !matches_patterns(rel, &include_set, &exclude_set) {
                continue;
            }
            progress.report_step(format!(
                "扫描: {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));

            match self.process_file(library, path, scan_marker) {
                Ok(change) => {
                    if change {
                        summary.changed += 1;
                    } else {
                        summary.unchanged += 1;
                    }
                }
                Err(e) => {
                    summary.errors += 1;
                    log::error!("处理文件失败 {}: {:?}", path.display(), e);
                    self.db
                        .mark_asset_seen(&library.id, &normalize_path(path), scan_marker)?;
                }
            }

            processed += 1;
            progress.report_progress(
                if total == 0 {
                    1.0
                } else {
                    processed as f64 / total as f64
                },
                processed as i64,
                total as i64,
                summary.errors as i64,
            );
        }

        wait_for_scan_control(control)?;
        // 本次扫描中未出现的素材才标记为离线，不保存整库路径到内存。
        if !traversal_failed {
            summary.removed = self
                .db
                .mark_unseen_assets_offline(&library.id, scan_marker)?;
        }

        Ok(summary)
    }

    fn count_media_files(
        &self,
        root: &Path,
        include: &[globset::GlobMatcher],
        exclude: &[globset::GlobMatcher],
        control: &JobControl,
    ) -> AppResult<usize> {
        let mut total = 0usize;
        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| !is_hidden(entry))
        {
            wait_for_scan_control(control)?;
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    log::warn!("统计时跳过无法访问的路径: {error}");
                    continue;
                }
            };
            if !entry.file_type().is_file() || !is_media_file(entry.path()) {
                continue;
            }
            let relative_path = entry.path().strip_prefix(root).unwrap_or(entry.path());
            if matches_patterns(relative_path, include, exclude) {
                total += 1;
            }
        }
        Ok(total)
    }

    fn process_file(&self, library: &Library, path: &Path, scan_marker: i64) -> AppResult<bool> {
        let meta = fs::metadata(path)?;
        let modified = meta
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let size = meta.len();
        let normalized = normalize_path(path);
        let quick_fp = quick_fingerprint(&normalized, size, modified);

        let existing = self
            .db
            .find_asset_by_library_path(&library.id, &normalized)?;
        if let Some(existing) = existing.as_ref() {
            if !needs_reindex(
                &existing.quick_fingerprint,
                &quick_fp,
                existing.status.as_str(),
            ) {
                // 如果不在线则恢复为 indexed
                if existing.status == AssetStatus::Offline {
                    let mut updated = existing.clone();
                    updated.status = AssetStatus::Indexed;
                    updated.updated_at = Utc::now().timestamp_millis();
                    self.db.create_or_update_asset(&updated)?;
                    self.db
                        .mark_asset_seen(&library.id, &normalized, scan_marker)?;
                    return Ok(true);
                }
                self.db
                    .mark_asset_seen(&library.id, &normalized, scan_marker)?;
                return Ok(false);
            }
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let media_type = MediaType::from_extension(&ext);

        let mut asset = Asset {
            id: existing_id(&library.id, &normalized, &self.db)?,
            library_id: library.id.clone(),
            media_type,
            file_path: path.to_string_lossy().to_string(),
            normalized_path: normalized.clone(),
            file_name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            extension: ext,
            size_bytes: size as i64,
            modified_at: modified,
            quick_fingerprint: quick_fp,
            full_hash: None,
            duration_ms: None,
            width: None,
            height: None,
            fps: None,
            codec: None,
            capture_time: None,
            status: AssetStatus::Pending,
            index_level: 0,
            analysis_version: 1,
            created_at: Utc::now().timestamp_millis(),
            updated_at: Utc::now().timestamp_millis(),
            thumbnail_data_url: None,
        };

        if let Some(existing) = existing {
            asset.id = existing.id;
            asset.created_at = existing.created_at;
            self.db.invalidate_asset_content_derivatives(&asset.id)?;
            self.thumbnail.remove_derivatives_for_asset(&asset.id)?;
        }

        // ffprobe 元数据
        if media_type == MediaType::Video || media_type == MediaType::Audio {
            if let Ok(info) = probe_media(path) {
                asset.duration_ms = info.duration_ms;
                asset.width = info.width;
                asset.height = info.height;
                asset.fps = info.fps;
                asset.codec = info.codec;
                asset.capture_time = info.capture_time;
            }
        } else if media_type == MediaType::Image {
            if let Ok((w, h)) = image_dimensions(path) {
                asset.width = Some(w);
                asset.height = Some(h);
            }
            asset.capture_time = probe_image_capture_time(path);
        }

        asset.status = AssetStatus::Indexed;
        asset.index_level = 1;

        // 缩略图（失败不阻塞）
        let thumbnail = match self.thumbnail.generate_for_asset(&asset) {
            Ok(path) => path,
            Err(error) => {
                log::warn!("生成缩略图失败 {}: {error:?}", path.display());
                None
            }
        };

        self.db.create_or_update_asset(&asset)?;
        if media_type == MediaType::Video {
            self.index_video_derivatives(&asset, path, &library.index_profile);
        }
        let embedding_source = match media_type {
            MediaType::Image => Some(path.to_path_buf()),
            MediaType::Video => thumbnail,
            MediaType::Audio => None,
        };
        if let Some(embedding_source) = embedding_source {
            match crate::providers::visual_embedding::embed_image(&embedding_source) {
                Ok(vector) => {
                    self.db.upsert_asset_embedding(
                        &asset.id,
                        crate::providers::visual_embedding::PROVIDER_ID,
                        crate::providers::visual_embedding::MODEL_VERSION,
                        &vector,
                    )?;
                }
                Err(error) => log::warn!("建立本地视觉索引失败 {}: {error}", path.display()),
            }
        }
        self.db.sync_entity_asset_reference_embeddings(&asset.id)?;
        self.db
            .mark_asset_seen(&library.id, &normalized, scan_marker)?;
        Ok(true)
    }

    /// Scene boundaries and playable derivatives are part of a video's
    /// indexed representation, not a UI-only action. Failure remains
    /// non-fatal: the source asset and its metadata stay searchable, and the
    /// user can retry manual shot detection after fixing FFmpeg.
    fn index_video_derivatives(&self, asset: &Asset, source: &Path, profile: &IndexProfile) {
        let Some(duration_ms) = asset.duration_ms else {
            log::warn!("跳过视频片段派生：时长未知 {}", source.display());
            return;
        };
        let mut segments = match crate::core::scene_detect::detect_shots(
            &asset.id,
            source,
            duration_ms,
            profile,
        ) {
            Ok(segments) => segments,
            Err(error) => {
                log::warn!("视频镜头检测降级 {}: {error}", source.display());
                return;
            }
        };
        if let Err(error) = crate::core::video_derivatives::generate_for_segments(
            &self.cache,
            source,
            &asset.id,
            &mut segments,
            profile,
        ) {
            log::warn!("视频关键帧/预览派生降级 {}: {error}", source.display());
        }
        if let Err(error) = self.db.replace_segments(&asset.id, &segments) {
            log::warn!("持久化视频镜头片段失败 {}: {error}", source.display());
        }
    }
}

pub(super) fn media_snapshot(library: &Library) -> AppResult<MediaSnapshot> {
    let root = Path::new(&library.root_path);
    let include = build_globset(&library.include_patterns);
    let exclude = build_globset(&library.exclude_patterns);
    let mut files = HashMap::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !is_hidden(entry))
    {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(path);
        if entry.file_type().is_file()
            && is_media_file(path)
            && matches_patterns(relative, &include, &exclude)
        {
            let metadata = entry.metadata()?;
            files.insert(
                relative.to_path_buf(),
                (metadata.len(), metadata.modified()?),
            );
        }
    }
    Ok(files)
}

pub(super) type MediaSnapshot = HashMap<PathBuf, (u64, std::time::SystemTime)>;

fn wait_for_scan_control(control: &JobControl) -> AppResult<()> {
    while control.is_paused() && !control.is_cancelled() {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    if control.is_cancelled() {
        return Err(AppError::Cancelled);
    }
    Ok(())
}

fn existing_id(library_id: &str, normalized_path: &str, db: &Database) -> AppResult<String> {
    Ok(db
        .find_asset_by_library_path(library_id, normalized_path)?
        .map(|a| a.id)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()))
}

fn image_dimensions(path: &Path) -> AppResult<(i32, i32)> {
    let reader = image::ImageReader::open(path)?;
    let (w, h) = reader.into_dimensions()?;
    Ok((w as i32, h as i32))
}

fn is_media_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        ext.as_str(),
        "jpg"
            | "jpeg"
            | "png"
            | "gif"
            | "webp"
            | "bmp"
            | "tiff"
            | "tif"
            | "heic"
            | "heif"
            | "avif"
            | "jxl"
            | "mp4"
            | "mov"
            | "mkv"
            | "avi"
            | "wmv"
            | "flv"
            | "webm"
            | "m4v"
            | "mpg"
            | "mpeg"
            | "mts"
            | "m2ts"
            | "ts"
            | "mp3"
            | "wav"
            | "aac"
            | "flac"
            | "m4a"
            | "ogg"
    )
}

fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.') && s != ".")
        .unwrap_or(false)
}

fn build_globset(patterns: &[String]) -> Vec<globset::GlobMatcher> {
    patterns
        .iter()
        .filter_map(|p| Glob::new(p).ok().map(|g| g.compile_matcher()))
        .collect()
}

fn matches_patterns(
    rel: &Path,
    include: &[globset::GlobMatcher],
    exclude: &[globset::GlobMatcher],
) -> bool {
    if !include.is_empty() && !include.iter().any(|g| g.is_match(rel)) {
        return false;
    }
    if exclude.iter().any(|g| g.is_match(rel)) {
        return false;
    }
    true
}

pub fn normalize_path(path: &Path) -> String {
    dunce::simplified(path).to_string_lossy().replace('/', "\\")
}

#[derive(Debug, Default)]
pub struct ScanSummary {
    pub changed: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub errors: usize,
}

pub fn default_include_patterns() -> Vec<String> {
    vec!["**/*".to_string()]
}

pub fn default_exclude_patterns() -> Vec<String> {
    vec![
        "**/.*".to_string(),
        "**/Thumbs.db".to_string(),
        "**/.DS_Store".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{is_media_file, normalize_path};

    #[test]
    fn recognises_supported_media_extensions_case_insensitively() {
        assert!(is_media_file(Path::new("C:/素材/夜景.MP4")));
        assert!(is_media_file(Path::new("C:/素材/角色.png")));
        assert!(!is_media_file(Path::new("C:/素材/notes.txt")));
    }

    #[test]
    fn normalises_a_chinese_windows_path() {
        let path = normalize_path(Path::new(r"E:\媒体 库\角色\雨夜.png"));
        assert!(path.contains("媒体 库\\角色\\雨夜.png"));
        assert!(!path.contains('/'));
    }
}
