use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::core::error::{AppError, AppResult};
use crate::core::fingerprint::quick_fingerprint;
use crate::core::scanner::normalize_path;
use crate::models::*;

const MATCH_ASSET_TERM: &str = "(file_name LIKE ? ESCAPE '\\' OR file_path LIKE ? ESCAPE '\\'
    OR EXISTS (SELECT 1 FROM tags WHERE scope_type = 'asset' AND scope_id = assets.id AND namespace = 'acg_creator' AND value LIKE ? ESCAPE '\\')
    OR EXISTS (SELECT 1 FROM asset_text WHERE asset_id = assets.id AND fingerprint = assets.quick_fingerprint AND body LIKE ? ESCAPE '\\'))";

#[derive(Clone)]
pub struct Database {
    path: PathBuf,
}

impl Database {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn open(&self) -> AppResult<Connection> {
        let conn = Connection::open(&self.path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA synchronous = NORMAL;",
        )?;
        Ok(conn)
    }

    pub fn init(&self) -> AppResult<()> {
        let conn = self.open()?;
        run_migrations(&conn)?;
        Ok(())
    }

    /// Produces a consistent SQLite snapshot without copying a live WAL file.
    pub fn export_snapshot(&self, output_path: &Path) -> AppResult<()> {
        if output_path.exists()
            || output_path
                .parent()
                .filter(|parent| parent.is_dir())
                .is_none()
        {
            return Err(AppError::InvalidPath(output_path.to_path_buf()));
        }
        let sql_path = output_path.to_string_lossy().replace('\'', "''");
        let conn = self.open()?;
        conn.execute_batch(&format!("VACUUM INTO '{sql_path}'"))?;
        Ok(())
    }

    // Libraries
    pub fn create_library(&self, library: &Library) -> AppResult<()> {
        let conn = self.open()?;
        conn.execute(
            "INSERT INTO libraries (
                id, name, root_path, status, index_profile,
                include_patterns, exclude_patterns, watch_enabled,
                last_scan_at, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                root_path = excluded.root_path,
                status = excluded.status,
                index_profile = excluded.index_profile,
                include_patterns = excluded.include_patterns,
                exclude_patterns = excluded.exclude_patterns,
                watch_enabled = excluded.watch_enabled,
                last_scan_at = excluded.last_scan_at,
                updated_at = excluded.updated_at",
            params![
                library.id,
                library.name,
                library.root_path,
                library.status.as_str(),
                library.index_profile.as_str(),
                serde_json::to_string(&library.include_patterns)?,
                serde_json::to_string(&library.exclude_patterns)?,
                library.watch_enabled as i32,
                library.last_scan_at,
                library.created_at,
                library.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_libraries(&self) -> AppResult<Vec<Library>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, root_path, status, index_profile,
                    include_patterns, exclude_patterns, watch_enabled,
                    last_scan_at, created_at, updated_at
             FROM libraries ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_library)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn get_library(&self, id: &str) -> AppResult<Option<Library>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, root_path, status, index_profile,
                    include_patterns, exclude_patterns, watch_enabled,
                    last_scan_at, created_at, updated_at
             FROM libraries WHERE id = ?1",
        )?;
        stmt.query_row([id], row_to_library)
            .optional()
            .map_err(AppError::from)
    }

    pub fn delete_library(&self, id: &str) -> AppResult<()> {
        let conn = self.open()?;
        conn.execute(
            "DELETE FROM tags WHERE scope_type = 'select_item' AND scope_id IN (SELECT item.id FROM selects_items item JOIN assets asset ON asset.id = item.asset_id WHERE asset.library_id = ?1)",
            [id],
        )?;
        conn.execute("DELETE FROM assets WHERE library_id = ?1", [id])?;
        conn.execute("DELETE FROM jobs WHERE library_id = ?1", [id])?;
        conn.execute("DELETE FROM libraries WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn set_library_watch_enabled(&self, id: &str, enabled: bool) -> AppResult<()> {
        let conn = self.open()?;
        let updated = conn.execute(
            "UPDATE libraries SET watch_enabled = ?1, updated_at = ?2 WHERE id = ?3",
            params![enabled, chrono::Utc::now().timestamp_millis(), id],
        )?;
        if updated == 0 {
            return Err(AppError::LibraryNotFound(id.to_owned()));
        }
        Ok(())
    }

    pub fn update_library_status(
        &self,
        id: &str,
        status: LibraryStatus,
        last_scan_at: Option<i64>,
    ) -> AppResult<()> {
        let conn = self.open()?;
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "UPDATE libraries SET status = ?1, last_scan_at = ?2, updated_at = ?3 WHERE id = ?4",
            params![status.as_str(), last_scan_at, now, id],
        )?;
        Ok(())
    }

    /// Switch a library to a new root without changing the identity of files that
    /// still exist at the same relative path. This preserves selects, embeddings,
    /// thumbnails and detected segments while making absent files explicitly offline.
    pub fn reconnect_library_root(
        &self,
        library_id: &str,
        new_root: &Path,
    ) -> AppResult<(Library, usize, usize)> {
        let mut library = self
            .get_library(library_id)?
            .ok_or_else(|| AppError::LibraryNotFound(library_id.to_string()))?;
        if library.status == LibraryStatus::Scanning {
            return Err(AppError::Other(
                "素材库正在扫描中，无法重新连接".to_string(),
            ));
        }

        let old_root = PathBuf::from(&library.root_path);
        let mut rebased = Vec::new();
        let mut normalized_paths = HashSet::new();
        for asset in self.list_assets(library_id)? {
            let Ok(relative_path) = Path::new(&asset.file_path).strip_prefix(&old_root) else {
                continue;
            };
            let candidate = new_root.join(relative_path);
            if !candidate.is_file() {
                continue;
            }
            let canonical = dunce::canonicalize(&candidate)
                .map_err(|_| AppError::InvalidPath(candidate.clone()))?;
            let normalized_path = normalize_path(&canonical);
            if !normalized_paths.insert(normalized_path.clone()) {
                return Err(AppError::Other(
                    "新素材库中存在无法安全区分的重复路径".to_string(),
                ));
            }
            let metadata = std::fs::metadata(&canonical)?;
            let modified_at = metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            rebased.push((
                asset,
                canonical,
                normalized_path,
                metadata.len() as i64,
                modified_at,
            ));
        }

        let now = chrono::Utc::now().timestamp_millis();
        let mut conn = self.open()?;
        let transaction = conn.transaction()?;
        transaction.execute(
            "UPDATE libraries SET root_path = ?1, status = 'idle', updated_at = ?2 WHERE id = ?3",
            params![normalize_path(new_root), now, library_id],
        )?;
        transaction.execute(
            "UPDATE assets SET status = 'offline', updated_at = ?1 WHERE library_id = ?2 AND status != 'offline'",
            params![now, library_id],
        )?;
        for (asset, canonical, normalized_path, size_bytes, modified_at) in &rebased {
            let changed = asset.size_bytes != *size_bytes || asset.modified_at != *modified_at;
            let status = if changed {
                AssetStatus::Pending
            } else if asset.status == AssetStatus::Offline {
                AssetStatus::Indexed
            } else {
                asset.status.clone()
            };
            let rebased_fingerprint =
                quick_fingerprint(normalized_path, asset.size_bytes as u64, asset.modified_at);
            if !changed {
                transaction.execute(
                    "UPDATE asset_text SET fingerprint = ?1 WHERE asset_id = ?2 AND fingerprint = ?3",
                    params![rebased_fingerprint, asset.id, asset.quick_fingerprint],
                )?;
            }
            transaction.execute(
                "UPDATE assets SET file_path = ?1, normalized_path = ?2, file_name = ?3,
                    quick_fingerprint = ?4, status = ?5, updated_at = ?6 WHERE id = ?7",
                params![
                    canonical.to_string_lossy().to_string(),
                    normalized_path,
                    canonical
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    rebased_fingerprint,
                    status.as_str(),
                    now,
                    asset.id,
                ],
            )?;
        }
        transaction.commit()?;

        let offline_assets = self
            .list_assets(library_id)?
            .iter()
            .filter(|asset| asset.status == AssetStatus::Offline)
            .count();
        library.root_path = normalize_path(new_root);
        library.status = LibraryStatus::Idle;
        library.updated_at = now;
        Ok((library, rebased.len(), offline_assets))
    }

    // Assets
    pub fn create_or_update_asset(&self, asset: &Asset) -> AppResult<()> {
        let conn = self.open()?;
        conn.execute(
            "INSERT INTO assets (
                id, library_id, media_type, file_path, normalized_path, file_name,
                extension, size_bytes, modified_at, quick_fingerprint, full_hash,
                duration_ms, width, height, fps, codec, capture_time, status,
                index_level, analysis_version, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)
            ON CONFLICT(id) DO UPDATE SET
                library_id = excluded.library_id,
                media_type = excluded.media_type,
                file_path = excluded.file_path,
                normalized_path = excluded.normalized_path,
                file_name = excluded.file_name,
                extension = excluded.extension,
                size_bytes = excluded.size_bytes,
                modified_at = excluded.modified_at,
                quick_fingerprint = excluded.quick_fingerprint,
                full_hash = excluded.full_hash,
                duration_ms = excluded.duration_ms,
                width = excluded.width,
                height = excluded.height,
                fps = excluded.fps,
                codec = excluded.codec,
                capture_time = excluded.capture_time,
                status = excluded.status,
                index_level = excluded.index_level,
                analysis_version = excluded.analysis_version,
                updated_at = excluded.updated_at",
            params![
                asset.id,
                asset.library_id,
                asset.media_type.as_str(),
                asset.file_path,
                asset.normalized_path,
                asset.file_name,
                asset.extension,
                asset.size_bytes,
                asset.modified_at,
                asset.quick_fingerprint,
                asset.full_hash,
                asset.duration_ms,
                asset.width,
                asset.height,
                asset.fps,
                asset.codec,
                asset.capture_time,
                asset.status.as_str(),
                asset.index_level,
                asset.analysis_version,
                asset.created_at,
                asset.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn find_asset_by_library_path(
        &self,
        library_id: &str,
        normalized_path: &str,
    ) -> AppResult<Option<Asset>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id, library_id, media_type, file_path, normalized_path, file_name,
                    extension, size_bytes, modified_at, quick_fingerprint, full_hash,
                    duration_ms, width, height, fps, codec, capture_time, status,
                    index_level, analysis_version, created_at, updated_at
             FROM assets WHERE library_id = ?1 AND normalized_path = ?2",
        )?;
        stmt.query_row([library_id, normalized_path], row_to_asset)
            .optional()
            .map_err(AppError::from)
    }

    pub fn mark_asset_seen(
        &self,
        library_id: &str,
        normalized_path: &str,
        scan_marker: i64,
    ) -> AppResult<()> {
        let conn = self.open()?;
        conn.execute(
            "UPDATE assets SET last_seen_scan_at = ?1 WHERE library_id = ?2 AND normalized_path = ?3",
            params![scan_marker, library_id, normalized_path],
        )?;
        Ok(())
    }

    pub fn mark_unseen_assets_offline(
        &self,
        library_id: &str,
        scan_marker: i64,
    ) -> AppResult<usize> {
        let conn = self.open()?;
        let now = chrono::Utc::now().timestamp_millis();
        let changed = conn.execute(
            "UPDATE assets
             SET status = 'offline', updated_at = ?1
             WHERE library_id = ?2 AND last_seen_scan_at != ?3 AND status != 'offline'",
            params![now, library_id, scan_marker],
        )?;
        Ok(changed)
    }

    pub fn list_assets(&self, library_id: &str) -> AppResult<Vec<Asset>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id, library_id, media_type, file_path, normalized_path, file_name,
                    extension, size_bytes, modified_at, quick_fingerprint, full_hash,
                    duration_ms, width, height, fps, codec, capture_time, status,
                    index_level, analysis_version, created_at, updated_at
             FROM assets WHERE library_id = ?1 ORDER BY file_name",
        )?;
        let rows = stmt.query_map([library_id], row_to_asset)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn list_indexed_visual_assets(&self) -> AppResult<Vec<Asset>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id, library_id, media_type, file_path, normalized_path, file_name,
                    extension, size_bytes, modified_at, quick_fingerprint, full_hash,
                    duration_ms, width, height, fps, codec, capture_time, status,
                    index_level, analysis_version, created_at, updated_at
             FROM assets WHERE status = 'indexed' AND media_type IN ('image', 'video')",
        )?;
        let rows = stmt.query_map([], row_to_asset)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn get_asset(&self, id: &str) -> AppResult<Option<Asset>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id, library_id, media_type, file_path, normalized_path, file_name,
                    extension, size_bytes, modified_at, quick_fingerprint, full_hash,
                    duration_ms, width, height, fps, codec, capture_time, status,
                    index_level, analysis_version, created_at, updated_at
             FROM assets WHERE id = ?1",
        )?;
        stmt.query_row([id], row_to_asset)
            .optional()
            .map_err(AppError::from)
    }

    pub fn add_asset_acg_tag(&self, asset_id: &str, value: &str) -> AppResult<Vec<String>> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 80 {
            return Err(AppError::Other("标签不能为空且最长 80 个字符".to_string()));
        }
        let conn = self.open()?;
        let asset_exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM assets WHERE id = ?1)",
            [asset_id],
            |row| row.get(0),
        )?;
        if !asset_exists {
            return Err(AppError::AssetNotFound(asset_id.to_string()));
        }
        let duplicate: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tags WHERE scope_type = 'asset' AND scope_id = ?1 AND namespace = 'acg_creator' AND value = ?2 COLLATE NOCASE)",
            params![asset_id, value],
            |row| row.get(0),
        )?;
        if !duplicate {
            let now = chrono::Utc::now().timestamp_millis();
            conn.execute(
                "INSERT INTO tags (id, scope_type, scope_id, namespace, key, value, source, user_confirmed, created_at, updated_at) VALUES (?1, 'asset', ?2, 'acg_creator', 'label', ?3, 'user', 1, ?4, ?4)",
                params![uuid::Uuid::new_v4().to_string(), asset_id, value, now],
            )?;
        }
        self.asset_acg_tags(asset_id)
    }

    pub fn asset_acg_tags(&self, asset_id: &str) -> AppResult<Vec<String>> {
        let conn = self.open()?;
        let mut statement = conn.prepare(
            "SELECT value FROM tags WHERE scope_type = 'asset' AND scope_id = ?1 AND namespace = 'acg_creator' ORDER BY created_at ASC",
        )?;
        let rows = statement.query_map([asset_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn remove_asset_acg_tag(&self, asset_id: &str, value: &str) -> AppResult<()> {
        let conn = self.open()?;
        let deleted = conn.execute(
            "DELETE FROM tags WHERE scope_type = 'asset' AND scope_id = ?1 AND namespace = 'acg_creator' AND value = ?2 COLLATE NOCASE",
            params![asset_id, value.trim()],
        )?;
        if deleted == 0 {
            return Err(AppError::Other("ACG 标签不存在或不属于该素材".to_string()));
        }
        Ok(())
    }

    pub fn store_asset_text(
        &self,
        asset: &Asset,
        source: AssetTextSource,
        cues: &[TextCue],
    ) -> AppResult<()> {
        let conn = self.open()?;
        let body = cues
            .iter()
            .map(|cue| cue.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let updated = conn.execute(
            "INSERT INTO asset_text (asset_id, source, fingerprint, body, cues_json)
             SELECT id, ?2, quick_fingerprint, ?4, ?5 FROM assets
             WHERE id = ?1 AND quick_fingerprint = ?3 AND status = 'indexed'
             ON CONFLICT(asset_id, source) DO UPDATE SET fingerprint = excluded.fingerprint,
               body = excluded.body, cues_json = excluded.cues_json",
            params![
                asset.id,
                source.as_str(),
                asset.quick_fingerprint,
                body,
                serde_json::to_string(cues)?
            ],
        )?;
        if updated == 0 {
            return Err(AppError::Other("素材已变更，请重新扫描后再分析".into()));
        }
        Ok(())
    }

    pub fn list_asset_text(
        &self,
        asset_id: &str,
        source: AssetTextSource,
    ) -> AppResult<Vec<TextCue>> {
        let conn = self.open()?;
        let json = conn.query_row(
            "SELECT text.cues_json FROM asset_text text JOIN assets ON assets.id = text.asset_id
             WHERE text.asset_id = ?1 AND text.source = ?2 AND text.fingerprint = assets.quick_fingerprint",
            params![asset_id, source.as_str()],
            |row| row.get::<_, String>(0),
        ).optional()?;
        match json {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(Vec::new()),
        }
    }

    pub fn delete_asset_text(&self, asset_id: &str, source: AssetTextSource) -> AppResult<()> {
        self.open()?.execute(
            "DELETE FROM asset_text WHERE asset_id = ?1 AND source = ?2",
            params![asset_id, source.as_str()],
        )?;
        Ok(())
    }

    pub fn search_assets(&self, query: &str, limit: usize) -> AppResult<Vec<Asset>> {
        let normalized_query = query.trim();
        if normalized_query.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.open()?;
        let pattern = format!(
            "%{}%",
            normalized_query.replace('%', "\\%").replace('_', "\\_")
        );
        let mut stmt = conn.prepare(
            "SELECT id, library_id, media_type, file_path, normalized_path, file_name,
                    extension, size_bytes, modified_at, quick_fingerprint, full_hash,
                    duration_ms, width, height, fps, codec, capture_time, status,
                    index_level, analysis_version, created_at, updated_at
             FROM assets
             WHERE status = 'indexed' AND (file_name LIKE ?1 ESCAPE '\\' OR file_path LIKE ?1 ESCAPE '\\')
             ORDER BY modified_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], row_to_asset)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn search_assets_with_conditions(
        &self,
        request: &SearchRequest,
        limit: usize,
    ) -> AppResult<Vec<Asset>> {
        let mut must = request.must.clone();
        if must.is_empty() && request.must_not.is_empty() && !request.raw_query.trim().is_empty() {
            must.push(request.raw_query.trim().to_string());
        }
        let must_segment_labels = must
            .iter()
            .filter_map(|term| segment_label_for_term(term))
            .collect::<Vec<_>>();
        must.retain(|term| segment_label_for_term(term).is_none());
        let must_not_segment_labels = request
            .must_not
            .iter()
            .filter_map(|term| segment_label_for_term(term))
            .collect::<Vec<_>>();
        if must.is_empty()
            && must_segment_labels.is_empty()
            && request.should.is_empty()
            && request.must_not.is_empty()
        {
            return Ok(Vec::new());
        }
        let mut score_sql = String::from("0");
        let mut score_values = Vec::<rusqlite::types::Value>::new();
        for term in &request.should {
            if let Some(label) = segment_label_for_term(term) {
                score_sql.push_str(" + CASE WHEN ");
                score_sql.push_str(label.sql_predicate(true));
                score_sql.push_str(" THEN 1 ELSE 0 END");
                continue;
            }
            score_sql.push_str(" + CASE WHEN ");
            append_asset_term(&mut score_sql, &mut score_values, term);
            score_sql.push_str(" THEN 1 ELSE 0 END");
        }
        let mut sql = format!("SELECT id, library_id, media_type, file_path, normalized_path, file_name, extension, size_bytes, modified_at, quick_fingerprint, full_hash, duration_ms, width, height, fps, codec, capture_time, status, index_level, analysis_version, created_at, updated_at, ({score_sql}) AS relevance FROM assets WHERE status = 'indexed'");
        let mut values = Vec::<rusqlite::types::Value>::new();
        if !request.media_types.is_empty() {
            sql.push_str(" AND media_type IN (");
            sql.push_str(
                &std::iter::repeat_n("?", request.media_types.len())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            sql.push(')');
            values.extend(request.media_types.iter().cloned().map(Into::into));
        }
        if let Some(min_quality_score) = request.min_quality_score {
            sql.push_str(" AND (media_type = 'image' OR EXISTS (SELECT 1 FROM segments WHERE segments.asset_id = assets.id AND quality_score >= ?))");
            values.push(min_quality_score.into());
        }
        if let Some(date_start_ms) = request.date_start_ms {
            sql.push_str(" AND COALESCE(capture_time, modified_at) >= ?");
            values.push(date_start_ms.into());
        }
        if let Some(date_end_ms) = request.date_end_ms {
            sql.push_str(" AND COALESCE(capture_time, modified_at) <= ?");
            values.push(date_end_ms.into());
        }
        if let Some(predicate) =
            segment_labels_predicate(&must_segment_labels, &must_not_segment_labels)
        {
            sql.push_str(" AND ");
            sql.push_str(&predicate);
        }
        for term in must {
            sql.push_str(" AND ");
            append_asset_term(&mut sql, &mut values, &term);
        }
        for term in &request.must_not {
            if segment_label_for_term(term).is_some() {
                continue;
            }
            sql.push_str(" AND NOT ");
            append_asset_term(&mut sql, &mut values, term);
        }
        sql.push_str(" ORDER BY relevance DESC, modified_at DESC LIMIT ?");
        score_values.append(&mut values);
        score_values.push((limit as i64).into());
        let conn = self.open()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(score_values), row_to_asset)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    /// Applies the non-text filters used by semantic retrieval. Textual Must
    /// terms intentionally remain available to the embedding query, while
    /// structured segment labels, Must Not, type and quality filters remain
    /// hard SQLite constraints.
    pub fn assets_matching_nonsemantic_filters(
        &self,
        request: &SearchRequest,
        limit: usize,
    ) -> AppResult<Vec<Asset>> {
        let mut sql = String::from(
            "SELECT id, library_id, media_type, file_path, normalized_path, file_name, extension, size_bytes, modified_at, quick_fingerprint, full_hash, duration_ms, width, height, fps, codec, capture_time, status, index_level, analysis_version, created_at, updated_at FROM assets WHERE status = 'indexed'",
        );
        let mut values = Vec::<rusqlite::types::Value>::new();
        if !request.media_types.is_empty() {
            sql.push_str(" AND media_type IN (");
            sql.push_str(
                &std::iter::repeat_n("?", request.media_types.len())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            sql.push(')');
            values.extend(request.media_types.iter().cloned().map(Into::into));
        }
        if let Some(min_quality_score) = request.min_quality_score {
            sql.push_str(" AND (media_type = 'image' OR EXISTS (SELECT 1 FROM segments WHERE segments.asset_id = assets.id AND quality_score >= ?))");
            values.push(min_quality_score.into());
        }
        if let Some(date_start_ms) = request.date_start_ms {
            sql.push_str(" AND COALESCE(capture_time, modified_at) >= ?");
            values.push(date_start_ms.into());
        }
        if let Some(date_end_ms) = request.date_end_ms {
            sql.push_str(" AND COALESCE(capture_time, modified_at) <= ?");
            values.push(date_end_ms.into());
        }
        let must_segment_labels = request
            .must
            .iter()
            .filter_map(|term| segment_label_for_term(term))
            .collect::<Vec<_>>();
        let must_not_segment_labels = request
            .must_not
            .iter()
            .filter_map(|term| segment_label_for_term(term))
            .collect::<Vec<_>>();
        if let Some(predicate) =
            segment_labels_predicate(&must_segment_labels, &must_not_segment_labels)
        {
            sql.push_str(" AND ");
            sql.push_str(&predicate);
        }
        for term in &request.must_not {
            if segment_label_for_term(term).is_some() {
                continue;
            }
            sql.push_str(" AND NOT ");
            append_asset_term(&mut sql, &mut values, term);
        }
        sql.push_str(" ORDER BY modified_at DESC LIMIT ?");
        values.push((limit as i64).into());
        let conn = self.open()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(values), row_to_asset)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn upsert_asset_embedding(
        &self,
        asset_id: &str,
        provider_id: &str,
        model_version: &str,
        vector: &[f32],
    ) -> AppResult<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.open()?;
        conn.execute(
            "INSERT INTO asset_embeddings (asset_id, provider_id, model_version, vector_json, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5) ON CONFLICT(asset_id, provider_id) DO UPDATE SET model_version = excluded.model_version, vector_json = excluded.vector_json, updated_at = excluded.updated_at",
            params![asset_id, provider_id, model_version, serde_json::to_string(vector)?, now],
        )?;
        Ok(())
    }

    /// Invalidates every derived signal for a source file whose bytes changed.
    ///
    /// `asset_id` stays stable across an incremental re-scan, so keeping its
    /// vectors would make a semantic search return the *previous* image or
    /// video. Segment-level selects are also tied to old timing and must not
    /// survive a replacement. Asset-level selects, user tags and favourites
    /// deliberately remain: they are choices about the library item itself.
    pub fn invalidate_asset_content_derivatives(&self, asset_id: &str) -> AppResult<()> {
        let mut conn = self.open()?;
        let transaction = conn.transaction()?;
        transaction.execute(
            "DELETE FROM asset_embeddings WHERE asset_id = ?1",
            [asset_id],
        )?;
        transaction.execute("DELETE FROM asset_text WHERE asset_id = ?1", [asset_id])?;
        transaction.execute(
            "DELETE FROM entity_reference_embeddings WHERE reference_id IN (SELECT id FROM entity_references WHERE asset_id = ?1)",
            [asset_id],
        )?;
        transaction.execute(
            "DELETE FROM tags WHERE scope_type = 'select_item' AND scope_id IN (SELECT id FROM selects_items WHERE segment_id IN (SELECT id FROM segments WHERE asset_id = ?1))",
            [asset_id],
        )?;
        transaction.execute(
            "DELETE FROM selects_items WHERE segment_id IN (SELECT id FROM segments WHERE asset_id = ?1)",
            [asset_id],
        )?;
        transaction.execute("DELETE FROM segments WHERE asset_id = ?1", [asset_id])?;
        transaction.commit()?;
        Ok(())
    }

    /// Rebuilds provider-scoped entity feedback vectors from the current asset
    /// vectors. This keeps an entity reference useful after an incremental
    /// source update without preserving a stale CLIP vector that requires an
    /// explicit semantic re-index.
    pub fn sync_entity_asset_reference_embeddings(&self, asset_id: &str) -> AppResult<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let mut conn = self.open()?;
        let transaction = conn.transaction()?;
        transaction.execute(
            "DELETE FROM entity_reference_embeddings WHERE reference_id IN (SELECT id FROM entity_references WHERE asset_id = ?1)",
            [asset_id],
        )?;
        transaction.execute(
            "INSERT INTO entity_reference_embeddings (reference_id, provider_id, model_version, vector_json, created_at, updated_at) SELECT r.id, e.provider_id, e.model_version, e.vector_json, ?2, ?2 FROM entity_references r JOIN asset_embeddings e ON e.asset_id = r.asset_id WHERE r.asset_id = ?1",
            params![asset_id, now],
        )?;
        transaction.execute(
            "UPDATE entity_references SET embedding_ref = (SELECT vector_json FROM asset_embeddings WHERE asset_id = ?1 AND provider_id = ?2) WHERE asset_id = ?1",
            params![asset_id, crate::providers::visual_embedding::PROVIDER_ID],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn similar_assets(&self, reference_asset_id: &str, limit: usize) -> AppResult<Vec<Asset>> {
        self.similar_assets_for_provider(
            reference_asset_id,
            crate::providers::visual_embedding::PROVIDER_ID,
            limit,
        )
    }

    pub fn similar_assets_for_provider(
        &self,
        reference_asset_id: &str,
        provider_id: &str,
        limit: usize,
    ) -> AppResult<Vec<Asset>> {
        let conn = self.open()?;
        let reference: String = conn
            .query_row(
                "SELECT vector_json FROM asset_embeddings WHERE asset_id = ?1 AND provider_id = ?2",
                params![reference_asset_id, provider_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| AppError::Other("该素材尚未建立本地视觉索引".to_string()))?;
        let reference: Vec<f32> = serde_json::from_str(&reference)?;
        let mut statement = conn.prepare(
            "SELECT a.id, a.library_id, a.media_type, a.file_path, a.normalized_path, a.file_name, a.extension, a.size_bytes, a.modified_at, a.quick_fingerprint, a.full_hash, a.duration_ms, a.width, a.height, a.fps, a.codec, a.capture_time, a.status, a.index_level, a.analysis_version, a.created_at, a.updated_at, e.vector_json FROM assets a JOIN asset_embeddings e ON e.asset_id = a.id WHERE a.status = 'indexed' AND a.id != ?1 AND e.provider_id = ?2",
        )?;
        let rows = statement.query_map(params![reference_asset_id, provider_id], |row| {
            Ok((row_to_asset_at(row, 0)?, row.get::<_, String>(22)?))
        })?;
        let mut ranked = rows
            .filter_map(Result::ok)
            .filter_map(|(asset, serialized)| {
                serde_json::from_str::<Vec<f32>>(&serialized)
                    .ok()
                    .map(|vector| {
                        (
                            asset,
                            crate::providers::visual_embedding::cosine_similarity(
                                &reference, &vector,
                            ),
                        )
                    })
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| right.1.total_cmp(&left.1));
        Ok(ranked
            .into_iter()
            .take(limit)
            .map(|(asset, _)| asset)
            .collect())
    }

    pub fn assets_for_visual_query(&self, query: &[f32], limit: usize) -> AppResult<Vec<Asset>> {
        self.assets_for_embedding_provider(
            crate::providers::visual_embedding::PROVIDER_ID,
            query,
            limit,
        )
    }

    pub fn assets_for_embedding_provider(
        &self,
        provider_id: &str,
        query: &[f32],
        limit: usize,
    ) -> AppResult<Vec<Asset>> {
        let conn = self.open()?;
        let mut statement = conn.prepare(
            "SELECT a.id, a.library_id, a.media_type, a.file_path, a.normalized_path, a.file_name, a.extension, a.size_bytes, a.modified_at, a.quick_fingerprint, a.full_hash, a.duration_ms, a.width, a.height, a.fps, a.codec, a.capture_time, a.status, a.index_level, a.analysis_version, a.created_at, a.updated_at, e.vector_json FROM assets a JOIN asset_embeddings e ON e.asset_id = a.id WHERE a.status = 'indexed' AND e.provider_id = ?1",
        )?;
        let rows = statement.query_map([provider_id], |row| {
            Ok((row_to_asset_at(row, 0)?, row.get::<_, String>(22)?))
        })?;
        let mut ranked = rows
            .filter_map(Result::ok)
            .filter_map(|(asset, serialized)| {
                serde_json::from_str::<Vec<f32>>(&serialized)
                    .ok()
                    .map(|vector| {
                        (
                            asset,
                            crate::providers::visual_embedding::cosine_similarity(query, &vector),
                        )
                    })
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| right.1.total_cmp(&left.1));
        Ok(ranked
            .into_iter()
            .take(limit)
            .map(|(asset, _)| asset)
            .collect())
    }

    pub fn record_search(
        &self,
        request: &SearchRequest,
        result_count: usize,
        latency_ms: i64,
    ) -> AppResult<()> {
        let conn = self.open()?;
        conn.execute("INSERT INTO searches (id, raw_query, parsed_query_json, result_count, latency_ms, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![uuid::Uuid::new_v4().to_string(), request.raw_query, serde_json::to_string(request)?, result_count as i64, latency_ms, chrono::Utc::now().timestamp_millis()])?;
        Ok(())
    }

    pub fn recent_searches(&self) -> AppResult<Vec<String>> {
        let conn = self.open()?;
        let mut statement = conn.prepare("SELECT raw_query FROM searches GROUP BY raw_query ORDER BY MAX(created_at) DESC LIMIT 8")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn delete_asset_by_path(&self, normalized_path: &str) -> AppResult<()> {
        let conn = self.open()?;
        conn.execute(
            "DELETE FROM tags WHERE scope_type = 'select_item' AND scope_id IN (SELECT item.id FROM selects_items item JOIN assets asset ON asset.id = item.asset_id WHERE asset.normalized_path = ?1)",
            [normalized_path],
        )?;
        conn.execute(
            "DELETE FROM assets WHERE normalized_path = ?1",
            [normalized_path],
        )?;
        Ok(())
    }

    // Segments
    pub fn list_segments(&self, asset_id: &str) -> AppResult<Vec<Segment>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id, asset_id, segment_type, segment_index, start_ms, end_ms, duration_ms,
                    representative_frame_path, thumbnail_path, preview_path, quality_score,
                    subtitle_present, game_ui, black_frame_score, blur_score, embedding_ref,
                    created_at, updated_at
             FROM segments WHERE asset_id = ?1 ORDER BY segment_index",
        )?;
        let rows = stmt.query_map([asset_id], row_to_segment)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn replace_segments(&self, asset_id: &str, segments: &[Segment]) -> AppResult<()> {
        let mut conn = self.open()?;
        let transaction = conn.transaction()?;
        // Detected segment ids are regenerated on every detection pass. Remove their
        // selects entries in the same transaction so a user never sees a stale range.
        transaction.execute(
            "DELETE FROM tags WHERE scope_type = 'select_item' AND scope_id IN (SELECT id FROM selects_items WHERE segment_id IN (SELECT id FROM segments WHERE asset_id = ?1))",
            [asset_id],
        )?;
        transaction.execute(
            "DELETE FROM selects_items WHERE segment_id IN (SELECT id FROM segments WHERE asset_id = ?1)",
            [asset_id],
        )?;
        transaction.execute("DELETE FROM segments WHERE asset_id = ?1", [asset_id])?;
        for segment in segments {
            transaction.execute(
                "INSERT INTO segments (
                    id, asset_id, segment_type, segment_index, start_ms, end_ms, duration_ms,
                    representative_frame_path, thumbnail_path, preview_path, quality_score,
                    subtitle_present, game_ui, black_frame_score, blur_score, embedding_ref,
                    created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                params![
                    segment.id, segment.asset_id, segment.segment_type, segment.segment_index,
                    segment.start_ms, segment.end_ms, segment.duration_ms, segment.representative_frame_path,
                    segment.thumbnail_path, segment.preview_path, segment.quality_score,
                    segment.subtitle_present.map(|value| if value { 1 } else { 0 }),
                    segment.game_ui.map(|value| if value { 1 } else { 0 }),
                    segment.black_frame_score, segment.blur_score, segment.embedding_ref,
                    segment.created_at, segment.updated_at,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn toggle_favorite(&self, asset_id: &str) -> AppResult<bool> {
        let conn = self.open()?;
        let existing: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM favorites WHERE asset_id = ?1",
                [asset_id],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            conn.execute("DELETE FROM favorites WHERE asset_id = ?1", [asset_id])?;
            Ok(false)
        } else {
            conn.execute(
                "INSERT INTO favorites (asset_id, created_at) VALUES (?1, ?2)",
                params![asset_id, chrono::Utc::now().timestamp_millis()],
            )?;
            Ok(true)
        }
    }

    pub fn favorite_asset_ids(&self) -> AppResult<Vec<String>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare("SELECT asset_id FROM favorites ORDER BY created_at DESC")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn set_asset_hidden(&self, asset_id: &str, hidden: bool) -> AppResult<()> {
        let conn = self.open()?;
        if hidden {
            let exists: Option<i64> = conn
                .query_row("SELECT 1 FROM assets WHERE id = ?1", [asset_id], |row| {
                    row.get(0)
                })
                .optional()?;
            if exists.is_none() {
                return Err(AppError::AssetNotFound(asset_id.to_string()));
            }
            conn.execute(
                "INSERT OR IGNORE INTO hidden_assets (asset_id, created_at) VALUES (?1, ?2)",
                params![asset_id, chrono::Utc::now().timestamp_millis()],
            )?;
        } else {
            conn.execute("DELETE FROM hidden_assets WHERE asset_id = ?1", [asset_id])?;
        }
        Ok(())
    }

    pub fn hidden_asset_ids(&self) -> AppResult<Vec<String>> {
        let conn = self.open()?;
        let mut stmt =
            conn.prepare("SELECT asset_id FROM hidden_assets ORDER BY created_at DESC")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn add_to_default_selects(&self, asset_id: &str) -> AppResult<()> {
        let conn = self.open()?;
        let collection_id = Self::default_select_collection_id(&conn)?;
        Self::add_asset_to_select_collection_conn(&conn, &collection_id, asset_id)
    }

    pub fn add_assets_to_default_selects(&self, asset_ids: &[String]) -> AppResult<usize> {
        let mut conn = self.open()?;
        let collection_id = Self::default_select_collection_id(&conn)?;
        let transaction = conn.transaction()?;
        let added =
            Self::add_assets_to_select_collection_conn(&transaction, &collection_id, asset_ids)?;
        transaction.commit()?;
        Ok(added)
    }

    pub fn add_assets_to_select_collection(
        &self,
        collection_id: &str,
        asset_ids: &[String],
    ) -> AppResult<usize> {
        let mut conn = self.open()?;
        let collection_exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM selects_collections WHERE id = ?1",
                [collection_id],
                |row| row.get(0),
            )
            .optional()?;
        if collection_exists.is_none() {
            return Err(AppError::Other("选片集合不存在或已被删除".to_string()));
        }
        let transaction = conn.transaction()?;
        let added =
            Self::add_assets_to_select_collection_conn(&transaction, collection_id, asset_ids)?;
        transaction.commit()?;
        Ok(added)
    }

    fn add_assets_to_select_collection_conn(
        conn: &Connection,
        collection_id: &str,
        asset_ids: &[String],
    ) -> AppResult<usize> {
        let mut added = 0;
        for asset_id in asset_ids {
            let exists: Option<i64> = conn.query_row(
                "SELECT 1 FROM selects_items WHERE collection_id = ?1 AND asset_id = ?2 AND segment_id IS NULL",
                params![collection_id, asset_id], |row| row.get(0),
            ).optional()?;
            if exists.is_none() {
                Self::add_asset_to_select_collection_conn(&conn, &collection_id, asset_id)?;
                added += 1;
            }
        }
        Ok(added)
    }

    pub fn add_segment_to_default_selects(
        &self,
        asset_id: &str,
        segment_id: &str,
    ) -> AppResult<()> {
        let conn = self.open()?;
        let collection_id = Self::default_select_collection_id(&conn)?;
        Self::add_segment_to_select_collection_conn(&conn, &collection_id, asset_id, segment_id)
    }

    pub fn add_segment_to_select_collection(
        &self,
        collection_id: &str,
        asset_id: &str,
        segment_id: &str,
    ) -> AppResult<()> {
        let conn = self.open()?;
        let collection_exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM selects_collections WHERE id = ?1",
                [collection_id],
                |row| row.get(0),
            )
            .optional()?;
        if collection_exists.is_none() {
            return Err(AppError::Other("选片集合不存在或已被删除".to_string()));
        }
        Self::add_segment_to_select_collection_conn(&conn, collection_id, asset_id, segment_id)
    }

    fn default_select_collection_id(conn: &Connection) -> AppResult<String> {
        let existing_collection: Option<String> = conn.query_row(
            "SELECT id FROM selects_collections WHERE name = '我的选片' ORDER BY created_at LIMIT 1",
            [],
            |row| row.get(0),
        ).optional()?;
        Ok(if let Some(id) = existing_collection {
            id
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().timestamp_millis();
            conn.execute(
                "INSERT INTO selects_collections (id, name, description, created_at, updated_at) VALUES (?1, '我的选片', '默认选片集合', ?2, ?2)",
                params![id, now],
            )?;
            id
        })
    }

    pub fn add_asset_to_select_collection(
        &self,
        collection_id: &str,
        asset_id: &str,
    ) -> AppResult<()> {
        let conn = self.open()?;
        let collection_exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM selects_collections WHERE id = ?1",
                [collection_id],
                |row| row.get(0),
            )
            .optional()?;
        if collection_exists.is_none() {
            return Err(AppError::Other("选片集合不存在或已被删除".to_string()));
        }
        Self::add_asset_to_select_collection_conn(&conn, collection_id, asset_id)
    }

    fn add_asset_to_select_collection_conn(
        conn: &Connection,
        collection_id: &str,
        asset_id: &str,
    ) -> AppResult<()> {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM selects_items WHERE collection_id = ?1 AND asset_id = ?2 AND segment_id IS NULL",
                params![collection_id, asset_id],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            let position: i64 = conn.query_row(
                "SELECT COUNT(*) FROM selects_items WHERE collection_id = ?1",
                [&collection_id],
                |row| row.get(0),
            )?;
            let now = chrono::Utc::now().timestamp_millis();
            conn.execute(
                "INSERT INTO selects_items (id, collection_id, asset_id, position, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![uuid::Uuid::new_v4().to_string(), collection_id, asset_id, position, now],
            )?;
        }
        Ok(())
    }

    fn add_segment_to_select_collection_conn(
        conn: &Connection,
        collection_id: &str,
        asset_id: &str,
        segment_id: &str,
    ) -> AppResult<()> {
        let valid_segment: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM segments WHERE id = ?1 AND asset_id = ?2",
                params![segment_id, asset_id],
                |row| row.get(0),
            )
            .optional()?;
        if valid_segment.is_none() {
            return Err(AppError::Other("镜头片段不存在或不属于该素材".to_string()));
        }
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM selects_items WHERE collection_id = ?1 AND asset_id = ?2 AND segment_id = ?3",
                params![collection_id, asset_id, segment_id],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            let position: i64 = conn.query_row(
                "SELECT COUNT(*) FROM selects_items WHERE collection_id = ?1",
                [collection_id],
                |row| row.get(0),
            )?;
            let now = chrono::Utc::now().timestamp_millis();
            conn.execute(
                "INSERT INTO selects_items (id, collection_id, asset_id, segment_id, position, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![uuid::Uuid::new_v4().to_string(), collection_id, asset_id, segment_id, position, now],
            )?;
        }
        Ok(())
    }

    pub fn default_select_assets(&self) -> AppResult<Vec<Asset>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT a.id, a.library_id, a.media_type, a.file_path, a.normalized_path, a.file_name,
                    a.extension, a.size_bytes, a.modified_at, a.quick_fingerprint, a.full_hash,
                    a.duration_ms, a.width, a.height, a.fps, a.codec, a.capture_time, a.status,
                    a.index_level, a.analysis_version, a.created_at, a.updated_at
             FROM selects_items item JOIN selects_collections collection ON item.collection_id = collection.id
             JOIN assets a ON item.asset_id = a.id
             WHERE collection.name = '我的选片' ORDER BY item.position",
        )?;
        let rows = stmt.query_map([], row_to_asset)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn create_select_collection(&self, collection: &SelectCollection) -> AppResult<()> {
        let conn = self.open()?;
        conn.execute(
            "INSERT INTO selects_collections (id, name, description, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![collection.id, collection.name, collection.description, collection.created_at, collection.updated_at],
        )?;
        Ok(())
    }

    pub fn list_select_collections(&self) -> AppResult<Vec<SelectCollection>> {
        let conn = self.open()?;
        let mut statement = conn.prepare(
            "SELECT id, name, description, created_at, updated_at FROM selects_collections ORDER BY updated_at DESC, name COLLATE NOCASE",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(SelectCollection {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn default_select_items(&self) -> AppResult<Vec<SelectItem>> {
        let conn = self.open()?;
        let collection_id = Self::default_select_collection_id(&conn)?;
        drop(conn);
        self.list_select_items(&collection_id)
    }

    pub fn list_select_items(&self, collection_id: &str) -> AppResult<Vec<SelectItem>> {
        let conn = self.open()?;
        let mut items = {
            let mut statement = conn.prepare(
                "SELECT item.id, item.collection_id, item.asset_id, item.segment_id, item.position, item.rating, item.note,
                    item.recommended_in_ms, item.recommended_out_ms, item.created_at, item.updated_at,
                    a.id, a.library_id, a.media_type, a.file_path, a.normalized_path, a.file_name, a.extension,
                    a.size_bytes, a.modified_at, a.quick_fingerprint, a.full_hash, a.duration_ms, a.width, a.height,
                    a.fps, a.codec, a.capture_time, a.status, a.index_level, a.analysis_version, a.created_at, a.updated_at,
                    segment.id, segment.asset_id, segment.segment_type, segment.segment_index, segment.start_ms, segment.end_ms, segment.duration_ms,
                    segment.representative_frame_path, segment.thumbnail_path, segment.preview_path, segment.quality_score,
                    segment.subtitle_present, segment.game_ui, segment.black_frame_score, segment.blur_score, segment.embedding_ref,
                    segment.created_at, segment.updated_at
             FROM selects_items item JOIN assets a ON a.id = item.asset_id
             LEFT JOIN segments segment ON segment.id = item.segment_id AND segment.asset_id = item.asset_id
             WHERE item.collection_id = ?1 ORDER BY item.position, item.created_at",
                )?;
            let rows = statement.query_map([collection_id], |row| {
                let segment_id: Option<String> = row.get(33)?;
                let segment = if let Some(id) = segment_id {
                    Some(Segment {
                        id,
                        asset_id: row.get(34)?,
                        segment_type: row.get(35)?,
                        segment_index: row.get(36)?,
                        start_ms: row.get(37)?,
                        end_ms: row.get(38)?,
                        duration_ms: row.get(39)?,
                        representative_frame_path: row.get(40)?,
                        thumbnail_path: row.get(41)?,
                        thumbnail_data_url: None,
                        preview_path: row.get(42)?,
                        quality_score: row.get(43)?,
                        subtitle_present: row.get::<_, Option<i64>>(44)?.map(|value| value != 0),
                        game_ui: row.get::<_, Option<i64>>(45)?.map(|value| value != 0),
                        black_frame_score: row.get(46)?,
                        blur_score: row.get(47)?,
                        embedding_ref: row.get(48)?,
                        created_at: row.get(49)?,
                        updated_at: row.get(50)?,
                    })
                } else {
                    None
                };
                Ok(SelectItem {
                    id: row.get(0)?,
                    collection_id: row.get(1)?,
                    asset_id: row.get(2)?,
                    segment_id: row.get(3)?,
                    position: row.get(4)?,
                    rating: row.get(5)?,
                    note: row.get(6)?,
                    recommended_in_ms: row.get(7)?,
                    recommended_out_ms: row.get(8)?,
                    tags: Vec::new(),
                    created_at: row.get(9)?,
                    updated_at: row.get(10)?,
                    asset: row_to_asset_at(row, 11)?,
                    segment,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(AppError::from)?
        };
        for item in &mut items {
            item.tags = Self::select_item_tags_conn(&conn, &item.id)?;
        }
        Ok(items)
    }

    pub fn add_select_item_tag(&self, item_id: &str, value: &str) -> AppResult<Vec<String>> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 80 {
            return Err(AppError::Other("标签不能为空且最长 80 个字符".to_string()));
        }
        let conn = self.open()?;
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM selects_items WHERE id = ?1",
                [item_id],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(AppError::Other("选片项不存在或已被删除".to_string()));
        }
        let duplicate: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tags WHERE scope_type = 'select_item' AND scope_id = ?1 AND namespace = 'selects' AND value = ?2 COLLATE NOCASE)",
            params![item_id, value],
            |row| row.get(0),
        )?;
        if !duplicate {
            let now = chrono::Utc::now().timestamp_millis();
            conn.execute(
                "INSERT INTO tags (id, scope_type, scope_id, namespace, key, value, source, user_confirmed, created_at, updated_at) VALUES (?1, 'select_item', ?2, 'selects', 'label', ?3, 'user', 1, ?4, ?4)",
                params![uuid::Uuid::new_v4().to_string(), item_id, value, now],
            )?;
        }
        Self::select_item_tags_conn(&conn, item_id)
    }

    pub fn add_select_item_tag_batch(&self, item_ids: &[String], value: &str) -> AppResult<usize> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 80 {
            return Err(AppError::Other("标签不能为空且最长 80 个字符".to_string()));
        }
        let mut conn = self.open()?;
        let transaction = conn.transaction()?;
        let mut added = 0;
        for item_id in item_ids {
            let exists: Option<i64> = transaction
                .query_row(
                    "SELECT 1 FROM selects_items WHERE id = ?1",
                    [item_id],
                    |row| row.get(0),
                )
                .optional()?;
            if exists.is_none() {
                return Err(AppError::Other("选片项不存在或已被删除".to_string()));
            }
            let duplicate: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM tags WHERE scope_type = 'select_item' AND scope_id = ?1 AND namespace = 'selects' AND value = ?2 COLLATE NOCASE)",
                params![item_id, value],
                |row| row.get(0),
            )?;
            if !duplicate {
                let now = chrono::Utc::now().timestamp_millis();
                transaction.execute(
                    "INSERT INTO tags (id, scope_type, scope_id, namespace, key, value, source, user_confirmed, created_at, updated_at) VALUES (?1, 'select_item', ?2, 'selects', 'label', ?3, 'user', 1, ?4, ?4)",
                    params![uuid::Uuid::new_v4().to_string(), item_id, value, now],
                )?;
                added += 1;
            }
        }
        transaction.commit()?;
        Ok(added)
    }

    pub fn remove_select_item_tag(&self, item_id: &str, value: &str) -> AppResult<()> {
        let conn = self.open()?;
        let deleted = conn.execute(
            "DELETE FROM tags WHERE scope_type = 'select_item' AND scope_id = ?1 AND namespace = 'selects' AND value = ?2 COLLATE NOCASE",
            params![item_id, value.trim()],
        )?;
        if deleted == 0 {
            return Err(AppError::Other("选片标签不存在或已被删除".to_string()));
        }
        Ok(())
    }

    fn select_item_tags_conn(conn: &Connection, item_id: &str) -> AppResult<Vec<String>> {
        let mut statement = conn.prepare(
            "SELECT value FROM tags WHERE scope_type = 'select_item' AND scope_id = ?1 AND namespace = 'selects' ORDER BY created_at ASC, value COLLATE NOCASE",
        )?;
        let rows = statement.query_map([item_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn update_select_item(
        &self,
        item_id: &str,
        request: &UpdateSelectItemRequest,
    ) -> AppResult<SelectItem> {
        let conn = self.open()?;
        let changed = conn.execute(
            "UPDATE selects_items SET rating = ?2, note = ?3, recommended_in_ms = ?4, recommended_out_ms = ?5, updated_at = ?6 WHERE id = ?1",
            params![item_id, request.rating, request.note, request.recommended_in_ms, request.recommended_out_ms, chrono::Utc::now().timestamp_millis()],
        )?;
        if changed == 0 {
            return Err(AppError::Other("选片项不存在或已被删除".to_string()));
        }
        let collection_id: String = conn.query_row(
            "SELECT collection_id FROM selects_items WHERE id = ?1",
            [item_id],
            |row| row.get(0),
        )?;
        drop(conn);
        self.list_select_items(&collection_id)?
            .into_iter()
            .find(|item| item.id == item_id)
            .ok_or_else(|| AppError::Other("选片项不存在或已被删除".to_string()))
    }

    pub fn remove_select_item(&self, item_id: &str) -> AppResult<()> {
        let mut conn = self.open()?;
        let transaction = conn.transaction()?;
        transaction.execute(
            "DELETE FROM tags WHERE scope_type = 'select_item' AND scope_id = ?1",
            [item_id],
        )?;
        transaction.execute("DELETE FROM selects_items WHERE id = ?1", [item_id])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn move_select_item(&self, item_id: &str, collection_id: &str) -> AppResult<()> {
        let conn = self.open()?;
        let target_exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM selects_collections WHERE id = ?1",
                [collection_id],
                |row| row.get(0),
            )
            .optional()?;
        if target_exists.is_none() {
            return Err(AppError::Other("目标选片集合不存在或已被删除".to_string()));
        }
        let position: i64 = conn.query_row(
            "SELECT COUNT(*) FROM selects_items WHERE collection_id = ?1",
            [collection_id],
            |row| row.get(0),
        )?;
        let changed = conn.execute(
            "UPDATE selects_items SET collection_id = ?2, position = ?3, updated_at = ?4 WHERE id = ?1",
            params![item_id, collection_id, position, chrono::Utc::now().timestamp_millis()],
        )?;
        if changed == 0 {
            return Err(AppError::Other("选片项不存在或已被删除".to_string()));
        }
        Ok(())
    }

    pub fn reorder_select_item(&self, item_id: &str, target_position: i64) -> AppResult<()> {
        let mut conn = self.open()?;
        let transaction = conn.transaction()?;
        let (collection_id, old_position): (String, i64) = transaction.query_row(
            "SELECT collection_id, position FROM selects_items WHERE id = ?1",
            [item_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let max_position: i64 = transaction.query_row(
            "SELECT COUNT(*) - 1 FROM selects_items WHERE collection_id = ?1",
            [&collection_id],
            |row| row.get(0),
        )?;
        let target = target_position.clamp(0, max_position.max(0));
        if target < old_position {
            transaction.execute("UPDATE selects_items SET position = position + 1 WHERE collection_id = ?1 AND position >= ?2 AND position < ?3", params![collection_id, target, old_position])?;
        }
        if target > old_position {
            transaction.execute("UPDATE selects_items SET position = position - 1 WHERE collection_id = ?1 AND position > ?2 AND position <= ?3", params![collection_id, old_position, target])?;
        }
        transaction.execute(
            "UPDATE selects_items SET position = ?2, updated_at = ?3 WHERE id = ?1",
            params![item_id, target, chrono::Utc::now().timestamp_millis()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn create_entity(&self, entity: &Entity) -> AppResult<()> {
        let conn = self.open()?;
        conn.execute("INSERT INTO entities (id, entity_type, name, description, aliases_json, pack_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![entity.id, entity.entity_type, entity.name, entity.description, serde_json::to_string(&entity.aliases)?, entity.pack_id, entity.created_at, entity.updated_at])?;
        Ok(())
    }

    pub fn list_entities(&self) -> AppResult<Vec<Entity>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare("SELECT id, entity_type, name, description, aliases_json, pack_id, created_at, updated_at FROM entities ORDER BY created_at DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok(Entity {
                id: row.get(0)?,
                entity_type: row.get(1)?,
                name: row.get(2)?,
                description: row.get(3)?,
                aliases: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(4)?)
                    .unwrap_or_default(),
                pack_id: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    /// Resolves an Entity mentioned in a parsed natural-language search. Must
    /// Not terms are intentionally excluded: asking for "不要角色 A" must not
    /// turn A's positive reference images into candidates.
    pub fn entities_matching_search_request(
        &self,
        request: &SearchRequest,
    ) -> AppResult<Vec<Entity>> {
        let positive_terms = request
            .must
            .iter()
            .chain(&request.should)
            .map(|term| term.trim().to_lowercase())
            .filter(|term| !term.is_empty())
            .collect::<Vec<_>>();
        let raw_query = request.raw_query.trim().to_lowercase();
        if positive_terms.is_empty() && (!request.must_not.is_empty() || raw_query.is_empty()) {
            return Ok(Vec::new());
        }
        Ok(self
            .list_entities()?
            .into_iter()
            .filter(|entity| {
                std::iter::once(&entity.name)
                    .chain(entity.aliases.iter())
                    .map(|name| name.trim().to_lowercase())
                    .filter(|name| !name.is_empty())
                    .any(|name| {
                        positive_terms.iter().any(|term| term == &name)
                            || (name.chars().count() >= 2 && raw_query.contains(&name))
                    })
            })
            .collect())
    }

    pub fn add_entity_reference(
        &self,
        reference: &crate::models::EntityReference,
        vector: &[f32],
    ) -> AppResult<()> {
        let conn = self.open()?;
        conn.execute("INSERT INTO entity_references (id, entity_id, asset_id, image_path, embedding_ref, is_positive, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![reference.id, reference.entity_id, reference.asset_id, reference.image_path, serde_json::to_string(vector)?, if reference.is_positive { 1 } else { 0 }, reference.created_at])?;
        self.upsert_entity_reference_embedding(
            &reference.id,
            crate::providers::visual_embedding::PROVIDER_ID,
            crate::providers::visual_embedding::MODEL_VERSION,
            vector,
        )?;
        Ok(())
    }

    pub fn upsert_entity_reference_embedding(
        &self,
        reference_id: &str,
        provider_id: &str,
        model_version: &str,
        vector: &[f32],
    ) -> AppResult<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.open()?;
        conn.execute(
            "INSERT INTO entity_reference_embeddings (reference_id, provider_id, model_version, vector_json, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5) ON CONFLICT(reference_id, provider_id) DO UPDATE SET model_version = excluded.model_version, vector_json = excluded.vector_json, updated_at = excluded.updated_at",
            params![reference_id, provider_id, model_version, serde_json::to_string(vector)?, now],
        )?;
        Ok(())
    }

    pub fn list_entity_references(
        &self,
        entity_id: &str,
    ) -> AppResult<Vec<crate::models::EntityReference>> {
        let conn = self.open()?;
        let mut statement = conn.prepare("SELECT id, entity_id, asset_id, image_path, is_positive, created_at FROM entity_references WHERE entity_id = ?1 ORDER BY created_at DESC")?;
        let rows = statement.query_map([entity_id], |row| {
            Ok(crate::models::EntityReference {
                id: row.get(0)?,
                entity_id: row.get(1)?,
                asset_id: row.get(2)?,
                image_path: row.get(3)?,
                is_positive: row.get::<_, i64>(4)? != 0,
                created_at: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn remove_entity_reference(&self, entity_id: &str, reference_id: &str) -> AppResult<()> {
        let conn = self.open()?;
        let deleted = conn.execute(
            "DELETE FROM entity_references WHERE id = ?1 AND entity_id = ?2",
            params![reference_id, entity_id],
        )?;
        if deleted == 0 {
            return Err(AppError::Other(
                "实体参考图不存在或不属于该实体".to_string(),
            ));
        }
        Ok(())
    }

    pub fn set_entity_asset_feedback(
        &self,
        entity_id: &str,
        asset_id: &str,
        is_positive: bool,
    ) -> AppResult<()> {
        let mut conn = self.open()?;
        let mut embedding_statement = conn.prepare(
            "SELECT provider_id, model_version, vector_json FROM asset_embeddings WHERE asset_id = ?1",
        )?;
        let embeddings = embedding_statement
            .query_map([asset_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let colour_vector = embeddings
            .iter()
            .find(|(provider, _, _)| provider == crate::providers::visual_embedding::PROVIDER_ID)
            .map(|(_, _, vector)| vector.clone())
            .ok_or_else(|| {
                AppError::Other("该素材尚未完成本地视觉索引，无法作为实体反馈".to_string())
            })?;
        drop(embedding_statement);
        let entity_exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM entities WHERE id = ?1)",
            [entity_id],
            |row| row.get(0),
        )?;
        if !entity_exists {
            return Err(AppError::Other("实体不存在".to_string()));
        }
        let transaction = conn.transaction()?;
        transaction.execute(
            "DELETE FROM entity_references WHERE entity_id = ?1 AND asset_id = ?2",
            params![entity_id, asset_id],
        )?;
        let reference_id = uuid::Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO entity_references (id, entity_id, asset_id, embedding_ref, is_positive, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![reference_id, entity_id, asset_id, colour_vector, if is_positive { 1 } else { 0 }, chrono::Utc::now().timestamp_millis()],
        )?;
        let now = chrono::Utc::now().timestamp_millis();
        for (provider_id, model_version, vector) in embeddings {
            transaction.execute(
                "INSERT INTO entity_reference_embeddings (reference_id, provider_id, model_version, vector_json, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![reference_id, provider_id, model_version, vector, now],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn similar_assets_for_entity(
        &self,
        entity_id: &str,
        limit: usize,
    ) -> AppResult<Vec<Asset>> {
        self.similar_assets_for_entity_provider(
            entity_id,
            crate::providers::visual_embedding::PROVIDER_ID,
            limit,
        )
    }

    /// Merges explicit name/alias hits with every available local reference
    /// provider for one entity. The caller remains responsible for applying
    /// request-specific hard filters and attaching presentation data.
    pub fn entity_candidate_assets(
        &self,
        entity_id: &str,
        include_semantic: bool,
        limit: usize,
    ) -> AppResult<Vec<Asset>> {
        let mut assets = self.assets_matching_entity_terms(entity_id, limit)?;
        if let Ok(visual_matches) = self.similar_assets_for_entity(entity_id, limit) {
            merge_unique_assets(&mut assets, visual_matches);
        }
        if include_semantic {
            if let Ok(semantic_matches) = self.similar_assets_for_entity_provider(
                entity_id,
                crate::providers::semantic_clip::PROVIDER_ID,
                limit,
            ) {
                merge_unique_assets(&mut assets, semantic_matches);
            }
        }
        Ok(assets)
    }

    pub fn similar_assets_for_entity_provider(
        &self,
        entity_id: &str,
        provider_id: &str,
        limit: usize,
    ) -> AppResult<Vec<Asset>> {
        let conn = self.open()?;
        let mut statement = conn.prepare(
            "SELECT e.vector_json, r.is_positive FROM entity_references r JOIN entity_reference_embeddings e ON e.reference_id = r.id WHERE r.entity_id = ?1 AND e.provider_id = ?2",
        )?;
        let rows = statement.query_map(params![entity_id, provider_id], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)? != 0))
        })?;
        let mut positive = Vec::new();
        let mut negative = Vec::new();
        for (serialized, is_positive) in rows.filter_map(Result::ok) {
            if let Some(vector) =
                serialized.and_then(|value| serde_json::from_str::<Vec<f32>>(&value).ok())
            {
                if is_positive {
                    positive.push(vector);
                } else {
                    negative.push(vector);
                }
            }
        }
        if positive.is_empty() {
            return Err(AppError::Other(
                "请先为实体添加至少一张正参考图".to_string(),
            ));
        }
        let mean = |vectors: &[Vec<f32>]| {
            let dimension = vectors[0].len();
            let mut output = vec![0.0_f32; dimension];
            let mut count = 0usize;
            for vector in vectors {
                if vector.len() == dimension {
                    for (index, value) in vector.iter().enumerate() {
                        output[index] += value;
                    }
                    count += 1;
                }
            }
            for value in &mut output {
                *value /= count.max(1) as f32;
            }
            output
        };
        let positive_mean = mean(&positive);
        let negative_mean = (!negative.is_empty()).then(|| mean(&negative));
        drop(statement);
        let mut statement = conn.prepare("SELECT a.id, a.library_id, a.media_type, a.file_path, a.normalized_path, a.file_name, a.extension, a.size_bytes, a.modified_at, a.quick_fingerprint, a.full_hash, a.duration_ms, a.width, a.height, a.fps, a.codec, a.capture_time, a.status, a.index_level, a.analysis_version, a.created_at, a.updated_at, e.vector_json FROM assets a JOIN asset_embeddings e ON e.asset_id = a.id WHERE a.status = 'indexed' AND e.provider_id = ?1")?;
        let rows = statement.query_map([provider_id], |row| {
            Ok((row_to_asset_at(row, 0)?, row.get::<_, String>(22)?))
        })?;
        let mut ranked = rows
            .filter_map(Result::ok)
            .filter_map(|(asset, serialized)| {
                serde_json::from_str::<Vec<f32>>(&serialized)
                    .ok()
                    .map(|vector| {
                        let positive_score = crate::providers::visual_embedding::cosine_similarity(
                            &positive_mean,
                            &vector,
                        );
                        let negative_penalty = negative_mean
                            .as_ref()
                            .map(|mean| {
                                crate::providers::visual_embedding::cosine_similarity(mean, &vector)
                                    * 0.35
                            })
                            .unwrap_or(0.0);
                        (asset, positive_score - negative_penalty)
                    })
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| right.1.total_cmp(&left.1));
        Ok(ranked
            .into_iter()
            .take(limit)
            .map(|(asset, _)| asset)
            .collect())
    }

    pub fn assets_matching_entity_terms(
        &self,
        entity_id: &str,
        limit: usize,
    ) -> AppResult<Vec<Asset>> {
        let conn = self.open()?;
        let (name, aliases_json): (String, String) = conn.query_row(
            "SELECT name, aliases_json FROM entities WHERE id = ?1",
            [entity_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut terms = vec![name.to_lowercase()];
        terms.extend(
            serde_json::from_str::<Vec<String>>(&aliases_json)
                .unwrap_or_default()
                .into_iter()
                .map(|term| term.to_lowercase()),
        );
        terms.retain(|term| !term.trim().is_empty());
        let mut statement = conn.prepare("SELECT id, library_id, media_type, file_path, normalized_path, file_name, extension, size_bytes, modified_at, quick_fingerprint, full_hash, duration_ms, width, height, fps, codec, capture_time, status, index_level, analysis_version, created_at, updated_at FROM assets WHERE status = 'indexed'")?;
        let rows = statement.query_map([], |row| row_to_asset_at(row, 0))?;
        let mut matches = rows
            .filter_map(Result::ok)
            .filter_map(|asset| {
                let haystack = format!("{} {}", asset.file_name, asset.file_path).to_lowercase();
                let score = terms
                    .iter()
                    .filter(|term| haystack.contains(term.as_str()))
                    .count();
                (score > 0).then_some((asset, score))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| left.0.file_name.cmp(&right.0.file_name))
        });
        Ok(matches
            .into_iter()
            .take(limit)
            .map(|(asset, _)| asset)
            .collect())
    }

    // Jobs
    pub fn create_job(&self, job: &Job) -> AppResult<()> {
        let conn = self.open()?;
        conn.execute(
            "INSERT INTO jobs (
                id, job_type, library_id, asset_id, status, priority, progress,
                current_step, checkpoint_json, error_code, error_message,
                started_at, finished_at, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            ON CONFLICT(id) DO UPDATE SET
                job_type = excluded.job_type,
                library_id = excluded.library_id,
                asset_id = excluded.asset_id,
                status = excluded.status,
                priority = excluded.priority,
                progress = excluded.progress,
                current_step = excluded.current_step,
                checkpoint_json = excluded.checkpoint_json,
                error_code = excluded.error_code,
                error_message = excluded.error_message,
                started_at = excluded.started_at,
                finished_at = excluded.finished_at,
                updated_at = excluded.updated_at",
            params![
                job.id,
                job.job_type.as_str(),
                job.library_id,
                job.asset_id,
                job.status.as_str(),
                job.priority,
                job.progress,
                job.current_step,
                job.checkpoint_json,
                job.error_code,
                job.error_message,
                job.started_at,
                job.finished_at,
                job.created_at,
                job.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_job(&self, id: &str) -> AppResult<Option<Job>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id, job_type, library_id, asset_id, status, priority, progress,
                    current_step, checkpoint_json, error_code, error_message,
                    started_at, finished_at, created_at, updated_at
             FROM jobs WHERE id = ?1",
        )?;
        stmt.query_row([id], row_to_job)
            .optional()
            .map_err(AppError::from)
    }

    pub fn list_jobs(&self) -> AppResult<Vec<Job>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id, job_type, library_id, asset_id, status, priority, progress,
                    current_step, checkpoint_json, error_code, error_message,
                    started_at, finished_at, created_at, updated_at
             FROM jobs ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_job)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn list_active_jobs(&self) -> AppResult<Vec<Job>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id, job_type, library_id, asset_id, status, priority, progress,
                    current_step, checkpoint_json, error_code, error_message,
                    started_at, finished_at, created_at, updated_at
             FROM jobs WHERE status = 'pending'
             ORDER BY priority DESC, created_at ASC",
        )?;
        let rows = stmt.query_map([], row_to_job)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    /// A library can have at most one scan in flight. A paused scan still owns
    /// the library's checkpoint and must be resumed or cancelled, not duplicated.
    pub fn active_scan_job_for_library(&self, library_id: &str) -> AppResult<Option<Job>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id, job_type, library_id, asset_id, status, priority, progress,
                    current_step, checkpoint_json, error_code, error_message,
                    started_at, finished_at, created_at, updated_at
             FROM jobs
             WHERE job_type = 'scan' AND library_id = ?1
               AND status IN ('pending', 'running', 'paused')
             ORDER BY CASE status WHEN 'running' THEN 0 WHEN 'pending' THEN 1 ELSE 2 END,
                      created_at ASC
             LIMIT 1",
        )?;
        stmt.query_row([library_id], row_to_job)
            .optional()
            .map_err(AppError::from)
    }

    /// Converts jobs that were running when the process stopped into pending
    /// work. Paused jobs are intentionally untouched: pausing is an explicit
    /// user decision and must survive an application restart.
    pub fn recover_interrupted_jobs(&self) -> AppResult<Vec<Job>> {
        let mut conn = self.open()?;
        let transaction = conn.transaction()?;
        let mut statement = transaction.prepare(
            "SELECT id, job_type, library_id, asset_id, status, priority, progress,
                    current_step, checkpoint_json, error_code, error_message,
                    started_at, finished_at, created_at, updated_at
             FROM jobs WHERE status = 'running' ORDER BY created_at ASC",
        )?;
        let rows = statement.query_map([], row_to_job)?;
        let mut recovered = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let now = chrono::Utc::now().timestamp_millis();
        for job in &mut recovered {
            job.status = JobStatus::Pending;
            job.current_step = "等待恢复（上次运行中断）".to_string();
            job.updated_at = now;
            transaction.execute(
                "UPDATE jobs SET status = ?1, current_step = ?2, updated_at = ?3 WHERE id = ?4",
                params![
                    job.status.as_str(),
                    job.current_step,
                    job.updated_at,
                    job.id
                ],
            )?;
        }
        transaction.commit()?;
        Ok(recovered)
    }

    pub fn update_job(&self, job: &Job) -> AppResult<()> {
        self.create_job(job)
    }

    // Stats
    pub fn get_app_stats(&self) -> AppResult<AppStats> {
        let conn = self.open()?;
        let library_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM libraries", [], |r| r.get(0))?;
        let asset_count: i64 = conn.query_row("SELECT COUNT(*) FROM assets", [], |r| r.get(0))?;
        let video_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM assets WHERE media_type = 'video'",
            [],
            |r| r.get(0),
        )?;
        let image_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM assets WHERE media_type = 'image'",
            [],
            |r| r.get(0),
        )?;
        let job_count: i64 = conn.query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))?;
        Ok(AppStats {
            library_count,
            asset_count,
            video_count,
            image_count,
            job_count,
        })
    }

    pub fn acg_creator_pack_enabled(&self) -> AppResult<bool> {
        let conn = self.open()?;
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM app_settings WHERE key = 'acg_creator_pack_enabled'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.as_deref() == Some("true"))
    }

    pub fn set_acg_creator_pack_enabled(&self, enabled: bool) -> AppResult<()> {
        let conn = self.open()?;
        conn.execute(
            "INSERT INTO app_settings (key, value, updated_at) VALUES ('acg_creator_pack_enabled', ?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![enabled.to_string(), chrono::Utc::now().timestamp_millis()],
        )?;
        Ok(())
    }
}

fn segment_labels_predicate(
    positive: &[SegmentLabel],
    negative: &[SegmentLabel],
) -> Option<String> {
    if positive.is_empty() && negative.is_empty() {
        return None;
    }
    let mut predicates = positive
        .iter()
        .map(|label| label.segment_predicate(true))
        .collect::<Vec<_>>();
    predicates.extend(negative.iter().map(|label| label.segment_predicate(false)));
    Some(format!(
        "EXISTS (SELECT 1 FROM segments WHERE segments.asset_id = assets.id AND {})",
        predicates.join(" AND ")
    ))
}

fn append_asset_term(sql: &mut String, values: &mut Vec<rusqlite::types::Value>, term: &str) {
    sql.push_str(MATCH_ASSET_TERM);
    let pattern = rusqlite::types::Value::from(search_pattern(term));
    values.extend(std::iter::repeat_n(pattern, 4));
}

fn search_pattern(term: &str) -> String {
    format!(
        "%{}%",
        term.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

fn row_to_library(row: &rusqlite::Row) -> rusqlite::Result<Library> {
    let status: String = row.get(3)?;
    let profile: String = row.get(4)?;
    let include_json: String = row.get(5)?;
    let exclude_json: String = row.get(6)?;
    Ok(Library {
        id: row.get(0)?,
        name: row.get(1)?,
        root_path: row.get(2)?,
        status: LibraryStatus::try_from(status.as_str()).unwrap_or(LibraryStatus::Idle),
        index_profile: IndexProfile::try_from(profile.as_str()).unwrap_or(IndexProfile::Balanced),
        include_patterns: serde_json::from_str(&include_json).unwrap_or_default(),
        exclude_patterns: serde_json::from_str(&exclude_json).unwrap_or_default(),
        watch_enabled: row.get::<_, i32>(7)? != 0,
        last_scan_at: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn row_to_asset(row: &rusqlite::Row) -> rusqlite::Result<Asset> {
    row_to_asset_at(row, 0)
}

fn row_to_asset_at(row: &rusqlite::Row, offset: usize) -> rusqlite::Result<Asset> {
    let media_type: String = row.get(offset + 2)?;
    let status: String = row.get(offset + 17)?;
    Ok(Asset {
        id: row.get(offset)?,
        library_id: row.get(offset + 1)?,
        media_type: MediaType::try_from(media_type.as_str()).unwrap_or(MediaType::Audio),
        file_path: row.get(offset + 3)?,
        normalized_path: row.get(offset + 4)?,
        file_name: row.get(offset + 5)?,
        extension: row.get(offset + 6)?,
        size_bytes: row.get(offset + 7)?,
        modified_at: row.get(offset + 8)?,
        quick_fingerprint: row.get(offset + 9)?,
        full_hash: row.get(offset + 10)?,
        duration_ms: row.get(offset + 11)?,
        width: row.get(offset + 12)?,
        height: row.get(offset + 13)?,
        fps: row.get(offset + 14)?,
        codec: row.get(offset + 15)?,
        capture_time: row.get(offset + 16)?,
        status: AssetStatus::try_from(status.as_str()).unwrap_or(AssetStatus::Pending),
        index_level: row.get(offset + 18)?,
        analysis_version: row.get(offset + 19)?,
        created_at: row.get(offset + 20)?,
        updated_at: row.get(offset + 21)?,
        thumbnail_data_url: None,
    })
}

fn merge_unique_assets(target: &mut Vec<Asset>, candidates: Vec<Asset>) {
    for candidate in candidates {
        if !target.iter().any(|asset| asset.id == candidate.id) {
            target.push(candidate);
        }
    }
}

fn row_to_job(row: &rusqlite::Row) -> rusqlite::Result<Job> {
    let job_type: String = row.get(1)?;
    let status: String = row.get(4)?;
    Ok(Job {
        id: row.get(0)?,
        job_type: JobType::try_from(job_type.as_str()).unwrap_or(JobType::Scan),
        library_id: row.get(2)?,
        asset_id: row.get(3)?,
        status: JobStatus::try_from(status.as_str()).unwrap_or(JobStatus::Pending),
        priority: row.get(5)?,
        progress: row.get(6)?,
        current_step: row.get(7)?,
        checkpoint_json: row.get(8)?,
        error_code: row.get(9)?,
        error_message: row.get(10)?,
        started_at: row.get(11)?,
        finished_at: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

fn row_to_segment(row: &rusqlite::Row) -> rusqlite::Result<Segment> {
    Ok(Segment {
        id: row.get(0)?,
        asset_id: row.get(1)?,
        segment_type: row.get(2)?,
        segment_index: row.get(3)?,
        start_ms: row.get(4)?,
        end_ms: row.get(5)?,
        duration_ms: row.get(6)?,
        representative_frame_path: row.get(7)?,
        thumbnail_path: row.get(8)?,
        thumbnail_data_url: None,
        preview_path: row.get(9)?,
        quality_score: row.get(10)?,
        subtitle_present: row.get::<_, Option<i32>>(11)?.map(|v| v != 0),
        game_ui: row.get::<_, Option<i32>>(12)?.map(|v| v != 0),
        black_frame_score: row.get(13)?,
        blur_score: row.get(14)?,
        embedding_ref: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
    })
}

fn run_migrations(conn: &Connection) -> AppResult<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 0 {
        create_schema(conn)?;
        return migrate_asset_text(conn);
    }

    if version < 2 {
        migrate_assets_to_library_scoped_paths(conn)?;
    }
    if version < 3 {
        conn.execute_batch(
            "ALTER TABLE assets ADD COLUMN last_seen_scan_at INTEGER NOT NULL DEFAULT 0;
             PRAGMA user_version = 3;",
        )?;
    }
    if version < 4 {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS favorites (asset_id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE); PRAGMA user_version = 4;")?;
    }
    if version < 5 {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL); PRAGMA user_version = 5;")?;
    }
    if version < 6 {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS asset_embeddings (asset_id TEXT PRIMARY KEY, provider_id TEXT NOT NULL, model_version TEXT NOT NULL, vector_json TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE); CREATE INDEX IF NOT EXISTS idx_asset_embeddings_provider ON asset_embeddings(provider_id, model_version); PRAGMA user_version = 6;")?;
    }
    if version < 7 {
        conn.execute_batch("BEGIN; ALTER TABLE asset_embeddings RENAME TO asset_embeddings_legacy; CREATE TABLE asset_embeddings (asset_id TEXT NOT NULL, provider_id TEXT NOT NULL, model_version TEXT NOT NULL, vector_json TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY (asset_id, provider_id), FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE); INSERT INTO asset_embeddings (asset_id, provider_id, model_version, vector_json, created_at, updated_at) SELECT asset_id, provider_id, model_version, vector_json, created_at, updated_at FROM asset_embeddings_legacy; DROP TABLE asset_embeddings_legacy; CREATE INDEX idx_asset_embeddings_provider ON asset_embeddings(provider_id, model_version); COMMIT; PRAGMA user_version = 7;")?;
    }
    if version < 8 {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS entity_reference_embeddings (reference_id TEXT NOT NULL, provider_id TEXT NOT NULL, model_version TEXT NOT NULL, vector_json TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY (reference_id, provider_id), FOREIGN KEY (reference_id) REFERENCES entity_references(id) ON DELETE CASCADE); CREATE INDEX IF NOT EXISTS idx_entity_reference_embeddings_provider ON entity_reference_embeddings(provider_id, model_version); INSERT OR IGNORE INTO entity_reference_embeddings (reference_id, provider_id, model_version, vector_json, created_at, updated_at) SELECT id, 'local-color-histogram', 'v1', embedding_ref, created_at, created_at FROM entity_references WHERE embedding_ref IS NOT NULL; PRAGMA user_version = 8;")?;
    }
    if version < 9 {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS hidden_assets (asset_id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE); PRAGMA user_version = 9;")?;
    }
    if version < 10 {
        migrate_asset_text(conn)?;
    }
    Ok(())
}

fn migrate_asset_text(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE IF NOT EXISTS asset_text (
            asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            source TEXT NOT NULL,
            fingerprint TEXT NOT NULL,
            body TEXT NOT NULL,
            cues_json TEXT NOT NULL,
            PRIMARY KEY (asset_id, source)
         );
         PRAGMA user_version = 10;
         COMMIT;",
    )?;
    Ok(())
}

fn create_schema(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS libraries (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            root_path TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'idle',
            index_profile TEXT NOT NULL DEFAULT 'balanced',
            include_patterns TEXT NOT NULL DEFAULT '[]',
            exclude_patterns TEXT NOT NULL DEFAULT '[]',
            watch_enabled INTEGER NOT NULL DEFAULT 0,
            last_scan_at INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS assets (
            id TEXT PRIMARY KEY,
            library_id TEXT NOT NULL,
            media_type TEXT NOT NULL,
            file_path TEXT NOT NULL,
            normalized_path TEXT NOT NULL,
            file_name TEXT NOT NULL,
            extension TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            modified_at INTEGER NOT NULL,
            quick_fingerprint TEXT NOT NULL,
            full_hash TEXT,
            duration_ms INTEGER,
            width INTEGER,
            height INTEGER,
            fps REAL,
            codec TEXT,
            capture_time INTEGER,
            status TEXT NOT NULL DEFAULT 'pending',
            index_level INTEGER NOT NULL DEFAULT 0,
            analysis_version INTEGER NOT NULL DEFAULT 0,
            last_seen_scan_at INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            FOREIGN KEY (library_id) REFERENCES libraries(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_assets_library ON assets(library_id, status);
        CREATE INDEX IF NOT EXISTS idx_assets_normpath ON assets(normalized_path);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_assets_library_normalized_path
            ON assets(library_id, normalized_path);
        CREATE INDEX IF NOT EXISTS idx_assets_fingerprint ON assets(quick_fingerprint);

        CREATE TABLE IF NOT EXISTS asset_embeddings (
            asset_id TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            model_version TEXT NOT NULL,
            vector_json TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (asset_id, provider_id),
            FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_asset_embeddings_provider ON asset_embeddings(provider_id, model_version);

        CREATE TABLE IF NOT EXISTS jobs (
            id TEXT PRIMARY KEY,
            job_type TEXT NOT NULL,
            library_id TEXT,
            asset_id TEXT,
            status TEXT NOT NULL DEFAULT 'pending',
            priority INTEGER NOT NULL DEFAULT 0,
            progress REAL NOT NULL DEFAULT 0,
            current_step TEXT NOT NULL DEFAULT '',
            checkpoint_json TEXT,
            error_code TEXT,
            error_message TEXT,
            started_at INTEGER,
            finished_at INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status, job_type);
        CREATE INDEX IF NOT EXISTS idx_jobs_library ON jobs(library_id, status);

        CREATE TABLE IF NOT EXISTS segments (
            id TEXT PRIMARY KEY,
            asset_id TEXT NOT NULL,
            segment_type TEXT NOT NULL,
            segment_index INTEGER NOT NULL,
            start_ms INTEGER NOT NULL,
            end_ms INTEGER NOT NULL,
            duration_ms INTEGER NOT NULL,
            representative_frame_path TEXT,
            thumbnail_path TEXT,
            preview_path TEXT,
            quality_score REAL,
            subtitle_present INTEGER,
            game_ui INTEGER,
            black_frame_score REAL,
            blur_score REAL,
            embedding_ref TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_segments_asset ON segments(asset_id);

        CREATE TABLE IF NOT EXISTS tags (
            id TEXT PRIMARY KEY,
            scope_type TEXT NOT NULL,
            scope_id TEXT NOT NULL,
            namespace TEXT NOT NULL,
            key TEXT NOT NULL,
            value TEXT NOT NULL,
            confidence REAL,
            source TEXT,
            pack_id TEXT,
            user_confirmed INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_tags_scope ON tags(scope_type, scope_id);

        CREATE TABLE IF NOT EXISTS entities (
            id TEXT PRIMARY KEY,
            entity_type TEXT NOT NULL,
            name TEXT NOT NULL,
            description TEXT,
            aliases_json TEXT NOT NULL DEFAULT '[]',
            pack_id TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS entity_references (
            id TEXT PRIMARY KEY,
            entity_id TEXT NOT NULL,
            asset_id TEXT,
            image_path TEXT,
            embedding_ref TEXT,
            is_positive INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            FOREIGN KEY (entity_id) REFERENCES entities(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_entity_refs ON entity_references(entity_id);

        CREATE TABLE IF NOT EXISTS entity_reference_embeddings (
            reference_id TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            model_version TEXT NOT NULL,
            vector_json TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (reference_id, provider_id),
            FOREIGN KEY (reference_id) REFERENCES entity_references(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_entity_reference_embeddings_provider
            ON entity_reference_embeddings(provider_id, model_version);

        CREATE TABLE IF NOT EXISTS searches (
            id TEXT PRIMARY KEY,
            raw_query TEXT NOT NULL,
            parsed_query_json TEXT,
            result_count INTEGER,
            latency_ms INTEGER,
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS selects_collections (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS selects_items (
            id TEXT PRIMARY KEY,
            collection_id TEXT NOT NULL,
            asset_id TEXT NOT NULL,
            segment_id TEXT,
            position INTEGER NOT NULL,
            rating INTEGER,
            note TEXT,
            recommended_in_ms INTEGER,
            recommended_out_ms INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            FOREIGN KEY (collection_id) REFERENCES selects_collections(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_selects_collection ON selects_items(collection_id);",
    )?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS favorites (asset_id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE);")?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS hidden_assets (asset_id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE);")?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL);")?;
    Ok(())
}

fn migrate_assets_to_library_scoped_paths(conn: &Connection) -> AppResult<()> {
    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    let result = conn.execute_batch(
        "BEGIN;
         ALTER TABLE segments RENAME TO segments_legacy;
         ALTER TABLE assets RENAME TO assets_legacy;
         CREATE TABLE assets (
             id TEXT PRIMARY KEY,
             library_id TEXT NOT NULL,
             media_type TEXT NOT NULL,
             file_path TEXT NOT NULL,
             normalized_path TEXT NOT NULL,
             file_name TEXT NOT NULL,
             extension TEXT NOT NULL,
             size_bytes INTEGER NOT NULL,
             modified_at INTEGER NOT NULL,
             quick_fingerprint TEXT NOT NULL,
             full_hash TEXT,
             duration_ms INTEGER,
             width INTEGER,
             height INTEGER,
             fps REAL,
             codec TEXT,
             capture_time INTEGER,
             status TEXT NOT NULL DEFAULT 'pending',
             index_level INTEGER NOT NULL DEFAULT 0,
             analysis_version INTEGER NOT NULL DEFAULT 0,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL,
             FOREIGN KEY (library_id) REFERENCES libraries(id) ON DELETE CASCADE
         );
         INSERT INTO assets (
             id, library_id, media_type, file_path, normalized_path, file_name, extension,
             size_bytes, modified_at, quick_fingerprint, full_hash, duration_ms, width, height,
             fps, codec, capture_time, status, index_level, analysis_version, created_at, updated_at
         ) SELECT
             id, library_id, media_type, file_path, normalized_path, file_name, extension,
             size_bytes, modified_at, quick_fingerprint, full_hash, duration_ms, width, height,
             fps, codec, capture_time, status, index_level, analysis_version, created_at, updated_at
         FROM assets_legacy;
         CREATE TABLE segments (
             id TEXT PRIMARY KEY,
             asset_id TEXT NOT NULL,
             segment_type TEXT NOT NULL,
             segment_index INTEGER NOT NULL,
             start_ms INTEGER NOT NULL,
             end_ms INTEGER NOT NULL,
             duration_ms INTEGER NOT NULL,
             representative_frame_path TEXT,
             thumbnail_path TEXT,
             preview_path TEXT,
             quality_score REAL,
             subtitle_present INTEGER,
             game_ui INTEGER,
             black_frame_score REAL,
             blur_score REAL,
             embedding_ref TEXT,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL,
             FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE
         );
         INSERT INTO segments SELECT * FROM segments_legacy;
         DROP TABLE segments_legacy;
         DROP TABLE assets_legacy;
         CREATE INDEX idx_assets_library ON assets(library_id, status);
         CREATE INDEX idx_assets_normpath ON assets(normalized_path);
         CREATE UNIQUE INDEX idx_assets_library_normalized_path ON assets(library_id, normalized_path);
         CREATE INDEX idx_assets_fingerprint ON assets(quick_fingerprint);
         CREATE INDEX idx_segments_asset ON segments(asset_id);
         COMMIT;
         PRAGMA user_version = 2;",
    );
    let enable_foreign_keys = conn.execute_batch("PRAGMA foreign_keys = ON;");
    result?;
    enable_foreign_keys?;
    Ok(())
}
