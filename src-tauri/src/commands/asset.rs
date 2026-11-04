use std::collections::{HashMap, HashSet};

use tauri::State;

use crate::core::app_state::AppState;
use crate::core::error::AppResult;
use crate::models::{Asset, Segment};

#[tauri::command]
pub async fn list_assets(state: State<'_, AppState>, library_id: String) -> AppResult<Vec<Asset>> {
    let mut assets = state.db.list_assets(&library_id)?;
    attach_thumbnail_data(&state, &mut assets);
    Ok(assets)
}

#[tauri::command]
pub async fn get_asset(state: State<'_, AppState>, id: String) -> AppResult<Option<Asset>> {
    let mut asset = state.db.get_asset(&id)?;
    if let Some(asset) = asset.as_mut() {
        attach_thumbnail_data_url(&state, asset);
    }
    Ok(asset)
}

#[tauri::command]
pub async fn list_segments(
    state: State<'_, AppState>,
    asset_id: String,
) -> AppResult<Vec<crate::models::Segment>> {
    let mut segments = state.db.list_segments(&asset_id)?;
    for segment in &mut segments {
        attach_segment_thumbnail_data_url(&state, segment);
    }
    Ok(segments)
}

#[tauri::command]
pub async fn detect_asset_shots(
    state: State<'_, AppState>,
    asset_id: String,
) -> AppResult<Vec<crate::models::Segment>> {
    let asset = state
        .db
        .get_asset(&asset_id)?
        .ok_or_else(|| crate::core::error::AppError::AssetNotFound(asset_id.clone()))?;
    if asset.media_type != crate::models::MediaType::Video {
        return Err(crate::core::error::AppError::Other(
            "只能对视频进行镜头检测".to_string(),
        ));
    }
    let duration = asset.duration_ms.ok_or_else(|| {
        crate::core::error::AppError::Other("视频时长未知，无法切分镜头".to_string())
    })?;
    let asset_for_worker = asset.clone();
    let profile = state
        .db
        .get_library(&asset.library_id)?
        .map(|library| library.index_profile)
        .unwrap_or(crate::models::IndexProfile::Balanced);
    let cache = std::sync::Arc::clone(&state.cache);
    let segments = tokio::task::spawn_blocking(move || {
        let mut segments = crate::core::scene_detect::detect_shots(
            &asset_for_worker.id,
            std::path::Path::new(&asset_for_worker.file_path),
            duration,
            &profile,
        )?;
        crate::core::video_derivatives::generate_for_segments(
            &cache,
            std::path::Path::new(&asset_for_worker.file_path),
            &asset_for_worker.id,
            &mut segments,
            &profile,
        )?;
        Ok::<_, crate::core::error::AppError>(segments)
    })
    .await
    .map_err(|error| {
        crate::core::error::AppError::Other(format!("视频处理任务异常终止: {error}"))
    })??;
    state.db.replace_segments(&asset.id, &segments)?;
    Ok(segments)
}

#[tauri::command]
pub async fn segment_preview_data_url(
    state: State<'_, AppState>,
    asset_id: String,
    segment_id: String,
) -> AppResult<Option<String>> {
    let segment = state
        .db
        .list_segments(&asset_id)?
        .into_iter()
        .find(|segment| segment.id == segment_id)
        .ok_or_else(|| {
            crate::core::error::AppError::Other("镜头片段不存在或不属于此素材".to_string())
        })?;
    let Some(preview_path) = segment.preview_path else {
        return Ok(None);
    };
    let cache_root = std::fs::canonicalize(state.cache.root())?;
    let preview_path = std::fs::canonicalize(preview_path)?;
    if !preview_path.starts_with(&cache_root)
        || preview_path.extension().and_then(|ext| ext.to_str()) != Some("mp4")
    {
        return Err(crate::core::error::AppError::InvalidPath(preview_path));
    }
    let bytes = std::fs::read(&preview_path)?;
    const MAX_PREVIEW_RESPONSE_BYTES: usize = 6 * 1024 * 1024;
    if bytes.len() > MAX_PREVIEW_RESPONSE_BYTES {
        return Err(crate::core::error::AppError::Other(
            "预览缓存过大，无法安全载入；请重新生成预览".to_string(),
        ));
    }
    Ok(Some(format!(
        "data:video/mp4;base64,{}",
        base64_encode(&bytes)
    )))
}

#[tauri::command]
pub async fn add_segment_to_default_selects(
    state: State<'_, AppState>,
    asset_id: String,
    segment_id: String,
) -> AppResult<()> {
    state
        .db
        .add_segment_to_default_selects(&asset_id, &segment_id)
}

#[tauri::command]
pub async fn add_asset_acg_tag(
    state: State<'_, AppState>,
    asset_id: String,
    value: String,
) -> AppResult<Vec<String>> {
    state.db.add_asset_acg_tag(&asset_id, &value)
}

#[tauri::command]
pub async fn list_asset_acg_tags(
    state: State<'_, AppState>,
    asset_id: String,
) -> AppResult<Vec<String>> {
    state.db.asset_acg_tags(&asset_id)
}

#[tauri::command]
pub async fn remove_asset_acg_tag(
    state: State<'_, AppState>,
    asset_id: String,
    value: String,
) -> AppResult<()> {
    state.db.remove_asset_acg_tag(&asset_id, &value)
}

#[tauri::command]
pub async fn search_assets(
    state: State<'_, AppState>,
    request: crate::models::SearchRequest,
) -> AppResult<Vec<crate::models::SearchResult>> {
    let started = std::time::Instant::now();
    let mut assets = state.db.search_assets_with_conditions(&request, 100)?;
    let mut semantic_ids = HashSet::new();
    let mut entity_match_names = HashMap::<String, Vec<String>>::new();
    let semantic_status = crate::providers::semantic_clip::status(
        &state.cache.models_path(),
        &crate::providers::semantic_clip::default_runtime_path(),
    );
    if semantic_status.ready {
        if let Some(prompt) = crate::providers::semantic_clip::semantic_query_prompt_for_conditions(
            &request.must,
            &request.should,
            &request.raw_query,
        ) {
            if let Ok(query_vector) = crate::providers::semantic_clip::embed_text(
                &state.cache.models_path(),
                &crate::providers::semantic_clip::default_runtime_path(),
                &prompt,
            ) {
                let allowed_ids = state
                    .db
                    .assets_matching_nonsemantic_filters(&request, 500)?
                    .into_iter()
                    .map(|asset| asset.id)
                    .collect::<HashSet<_>>();
                for semantic_asset in state.db.assets_for_embedding_provider(
                    crate::providers::semantic_clip::PROVIDER_ID,
                    &query_vector,
                    100,
                )? {
                    if allowed_ids.contains(&semantic_asset.id) {
                        semantic_ids.insert(semantic_asset.id.clone());
                        if !assets.iter().any(|asset| asset.id == semantic_asset.id) {
                            assets.push(semantic_asset);
                        }
                    }
                }
            }
        }
    }
    let allowed_ids = state
        .db
        .assets_matching_nonsemantic_filters(&request, 500)?
        .into_iter()
        .map(|asset| asset.id)
        .collect::<HashSet<_>>();
    for entity in state.db.entities_matching_search_request(&request)? {
        for candidate in state
            .db
            .entity_candidate_assets(&entity.id, semantic_status.ready, 100)?
        {
            if !allowed_ids.contains(&candidate.id) {
                continue;
            }
            let asset_id = candidate.id.clone();
            if !assets.iter().any(|asset| asset.id == asset_id) {
                assets.push(candidate);
            }
            entity_match_names
                .entry(asset_id)
                .or_default()
                .push(entity.name.clone());
        }
    }
    let mut visual_fallback: Option<&str> = None;
    if assets.is_empty() {
        if let Some(query_vector) =
            crate::providers::visual_embedding::embed_color_query(&request.raw_query)
        {
            assets = state
                .db
                .assets_for_visual_query(&query_vector, 100)?
                .into_iter()
                .filter(|asset| allowed_ids.contains(&asset.id))
                .collect();
            visual_fallback = (!assets.is_empty()).then_some("命中本地颜色视觉特征");
        }
    }
    state
        .db
        .record_search(&request, assets.len(), started.elapsed().as_millis() as i64)?;
    attach_thumbnail_data(&state, &mut assets);
    let mut results = assets
        .into_iter()
        .map(|asset| {
            let visual_reason = if semantic_ids.contains(&asset.id) {
                Some("命中本地 CLIP 图文语义向量")
            } else {
                visual_fallback
            };
            let mut result = explain_search_result(asset, &request, visual_reason);
            if let Some(names) = entity_match_names.get(&result.asset.id) {
                let names = names
                    .iter()
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>();
                result.match_reasons.push(format!(
                    "命中实体参考：{}",
                    names.into_iter().collect::<Vec<_>>().join("、")
                ));
                result.score += 1.0;
            }
            result
        })
        .collect::<Vec<_>>();
    for result in &mut results {
        let acg_tags = state.db.asset_acg_tags(&result.asset.id)?;
        apply_acg_tag_explanations(result, &request, &acg_tags);
        apply_text_explanations(&state, result, &request)?;
        let segments = state.db.list_segments(&result.asset.id)?;
        apply_segment_label_explanations(result, &request, &segments);
        result.matching_segment_ids = matching_segment_ids(&request, &segments);
        if !result.matching_segment_ids.is_empty() {
            result.match_reasons.push(format!(
                "已定位 {} 个满足片段条件的镜头",
                result.matching_segment_ids.len()
            ));
        }
    }
    // Keyword, CLIP and entity candidates are retrieved through separate
    // paths. Rank only after they have been merged so a later semantic or
    // entity candidate cannot be displayed below unrelated SQL candidates.
    results.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.asset.modified_at.cmp(&left.asset.modified_at))
            .then_with(|| left.asset.id.cmp(&right.asset.id))
    });
    Ok(diversify_equal_score_results(results))
}

/// Keeps ranking quality intact while avoiding a same-score block from one
/// library. Scores remain strict priority tiers; only candidates that are
/// otherwise tied are interleaved in a stable round-robin order.
fn diversify_equal_score_results(
    sorted: Vec<crate::models::SearchResult>,
) -> Vec<crate::models::SearchResult> {
    let mut diversified = Vec::with_capacity(sorted.len());
    let mut offset = 0;
    while offset < sorted.len() {
        let score = sorted[offset].score;
        let mut end = offset + 1;
        while end < sorted.len() && sorted[end].score.total_cmp(&score).is_eq() {
            end += 1;
        }
        let mut group = sorted[offset..end]
            .iter()
            .cloned()
            .map(Some)
            .collect::<Vec<_>>();
        let libraries = group
            .iter()
            .map(|result| {
                result
                    .as_ref()
                    .expect("group entry must exist")
                    .asset
                    .library_id
                    .clone()
            })
            .collect::<Vec<_>>();
        let order = diversity_order(&libraries);
        let diversified_group = order.len() > 1
            && libraries
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 1
            && order != (0..order.len()).collect::<Vec<_>>();
        for index in order {
            let mut result = group[index].take().expect("diversity index must be unique");
            if diversified_group {
                result
                    .match_reasons
                    .push("同分候选按素材库轮换，保留结果多样性".to_string());
            }
            diversified.push(result);
        }
        offset = end;
    }
    diversified
}

fn diversity_order(library_ids: &[String]) -> Vec<usize> {
    let mut queues = Vec::<(String, std::collections::VecDeque<usize>)>::new();
    for (index, library_id) in library_ids.iter().enumerate() {
        if let Some((_, indices)) = queues.iter_mut().find(|(id, _)| id == library_id) {
            indices.push_back(index);
        } else {
            queues.push((
                library_id.clone(),
                std::collections::VecDeque::from([index]),
            ));
        }
    }
    let mut order = Vec::with_capacity(library_ids.len());
    while order.len() < library_ids.len() {
        for (_, indices) in &mut queues {
            if let Some(index) = indices.pop_front() {
                order.push(index);
            }
        }
    }
    order
}

fn apply_text_explanations(
    state: &AppState,
    result: &mut crate::models::SearchResult,
    request: &crate::models::SearchRequest,
) -> AppResult<()> {
    use crate::models::AssetTextSource::{Ocr, Speech};
    for (source, label) in [(Ocr, "画面文字"), (Speech, "语音转写")] {
        let cues = state.db.list_asset_text(&result.asset.id, source)?;
        if cues.is_empty() {
            continue;
        }
        for (term, weight) in request
            .must
            .iter()
            .map(|term| (term, 2.0))
            .chain(request.should.iter().map(|term| (term, 1.0)))
        {
            let lowered = term.to_lowercase();
            if let Some(cue) = cues
                .iter()
                .find(|cue| cue.text.to_lowercase().contains(&lowered))
            {
                let location = cue
                    .start_ms
                    .map(|start| format!("（{}）", crate::core::export::timecode(start)))
                    .unwrap_or_default();
                result
                    .match_reasons
                    .retain(|reason| reason != "按关键词召回");
                result
                    .match_reasons
                    .push(format!("命中{label}：{term}{location}"));
                result.unmet_should.retain(|unmet| unmet != term);
                result.score += weight;
            }
        }
    }
    Ok(())
}

fn apply_acg_tag_explanations(
    result: &mut crate::models::SearchResult,
    request: &crate::models::SearchRequest,
    acg_tags: &[String],
) {
    let searchable =
        format!("{} {}", result.asset.file_name, result.asset.file_path).to_lowercase();
    for term in &request.must {
        if !searchable.contains(&term.to_lowercase()) && tag_matches(acg_tags, term) {
            result
                .match_reasons
                .push(format!("满足必须条件：{term}（ACG 标签）"));
            result.score += 2.0;
        }
    }
    for term in &request.should {
        if !searchable.contains(&term.to_lowercase()) && tag_matches(acg_tags, term) {
            result.unmet_should.retain(|unmet| unmet != term);
            result
                .match_reasons
                .push(format!("命中偏好条件：{term}（ACG 标签）"));
            result.score += 1.0;
        }
    }
}

fn tag_matches(acg_tags: &[String], term: &str) -> bool {
    let normalized = term.trim().to_lowercase();
    !normalized.is_empty()
        && acg_tags
            .iter()
            .any(|tag| tag.to_lowercase().contains(&normalized))
}

fn apply_segment_label_explanations(
    result: &mut crate::models::SearchResult,
    request: &crate::models::SearchRequest,
    segments: &[Segment],
) {
    for term in &request.must {
        if let Some(label) = matching_segment_label(term, segments) {
            result
                .match_reasons
                .push(format!("满足必须条件：{term}（{}）", label.explanation()));
            result.score += 2.0;
        }
    }
    for term in &request.should {
        if let Some(label) = matching_segment_label(term, segments) {
            result.unmet_should.retain(|unmet| unmet != term);
            result
                .match_reasons
                .push(format!("命中偏好条件：{term}（{}）", label.explanation()));
            result.score += 1.0;
        }
    }
}

fn matching_segment_label(term: &str, segments: &[Segment]) -> Option<crate::models::SegmentLabel> {
    crate::models::segment_label_for_term(term).filter(|label| {
        segments
            .iter()
            .any(|segment| label.matches_segment(segment))
    })
}

fn matching_segment_ids(
    request: &crate::models::SearchRequest,
    segments: &[Segment],
) -> Vec<String> {
    let required = request
        .must
        .iter()
        .filter_map(|term| crate::models::segment_label_for_term(term))
        .collect::<Vec<_>>();
    let excluded = request
        .must_not
        .iter()
        .filter_map(|term| crate::models::segment_label_for_term(term))
        .collect::<Vec<_>>();
    let preferred = request
        .should
        .iter()
        .filter_map(|term| crate::models::segment_label_for_term(term))
        .collect::<Vec<_>>();
    if required.is_empty() && excluded.is_empty() && preferred.is_empty() {
        return Vec::new();
    }
    segments
        .iter()
        .filter(|segment| {
            if !required.is_empty() || !excluded.is_empty() {
                required.iter().all(|label| label.matches_segment(segment))
                    && excluded.iter().all(|label| !label.matches_segment(segment))
            } else {
                preferred.iter().any(|label| label.matches_segment(segment))
            }
        })
        .map(|segment| segment.id.clone())
        .collect()
}

fn explain_search_result(
    asset: Asset,
    request: &crate::models::SearchRequest,
    visual_fallback: Option<&str>,
) -> crate::models::SearchResult {
    let searchable = format!("{} {}", asset.file_name, asset.file_path).to_lowercase();
    let mut match_reasons = Vec::new();
    let mut score = 0.0;
    if let Some(reason) = visual_fallback {
        match_reasons.push(reason.to_string());
        score += 1.0;
    }
    for term in &request.must {
        if searchable.contains(&term.to_lowercase()) {
            match_reasons.push(format!("满足必须条件：{term}"));
            score += 2.0;
        }
    }
    let unmet_should = request
        .should
        .iter()
        .filter(|term| !searchable.contains(&term.to_lowercase()))
        .cloned()
        .collect::<Vec<_>>();
    for term in &request.should {
        if searchable.contains(&term.to_lowercase()) {
            match_reasons.push(format!("命中偏好条件：{term}"));
            score += 1.0;
        }
    }
    if match_reasons.is_empty() {
        match_reasons.push("按关键词召回".to_string());
    }
    crate::models::SearchResult {
        asset,
        score,
        match_reasons,
        unmet_should,
        matching_segment_ids: Vec::new(),
    }
}

#[tauri::command]
pub async fn find_similar_assets(
    state: State<'_, AppState>,
    asset_id: String,
) -> AppResult<Vec<Asset>> {
    let mut assets = state.db.similar_assets(&asset_id, 100)?;
    attach_thumbnail_data(&state, &mut assets);
    Ok(assets)
}

#[tauri::command]
pub async fn find_similar_by_reference_image(
    state: State<'_, AppState>,
    image_path: String,
) -> AppResult<Vec<Asset>> {
    let path = std::path::PathBuf::from(image_path);
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !path.is_absolute()
        || !path.is_file()
        || !matches!(
            extension.as_str(),
            "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp"
        )
    {
        return Err(crate::core::error::AppError::InvalidPath(path));
    }
    let semantic_status = crate::providers::semantic_clip::status(
        &state.cache.models_path(),
        &crate::providers::semantic_clip::default_runtime_path(),
    );
    let mut assets = if semantic_status.ready {
        match crate::providers::semantic_clip::embed_image(
            &state.cache.models_path(),
            &crate::providers::semantic_clip::default_runtime_path(),
            &path,
        ) {
            Ok(vector) => state.db.assets_for_embedding_provider(
                crate::providers::semantic_clip::PROVIDER_ID,
                &vector,
                100,
            )?,
            Err(error) => {
                log::warn!("本地语义参考图检索降级为颜色索引: {error}");
                let vector = crate::providers::visual_embedding::embed_image(&path)?;
                state.db.assets_for_visual_query(&vector, 100)?
            }
        }
    } else {
        let vector = crate::providers::visual_embedding::embed_image(&path)?;
        state.db.assets_for_visual_query(&vector, 100)?
    };
    attach_thumbnail_data(&state, &mut assets);
    Ok(assets)
}

#[tauri::command]
pub async fn recent_searches(state: State<'_, AppState>) -> AppResult<Vec<String>> {
    state.db.recent_searches()
}

#[tauri::command]
pub async fn toggle_favorite(state: State<'_, AppState>, asset_id: String) -> AppResult<bool> {
    state.db.toggle_favorite(&asset_id)
}

#[tauri::command]
pub async fn favorite_asset_ids(state: State<'_, AppState>) -> AppResult<Vec<String>> {
    state.db.favorite_asset_ids()
}

#[tauri::command]
pub async fn set_asset_hidden(
    state: State<'_, AppState>,
    asset_id: String,
    hidden: bool,
) -> AppResult<()> {
    state.db.set_asset_hidden(&asset_id, hidden)
}

#[tauri::command]
pub async fn hidden_asset_ids(state: State<'_, AppState>) -> AppResult<Vec<String>> {
    state.db.hidden_asset_ids()
}

#[tauri::command]
pub async fn add_to_default_selects(state: State<'_, AppState>, asset_id: String) -> AppResult<()> {
    state.db.add_to_default_selects(&asset_id)
}

#[tauri::command]
pub async fn add_assets_to_default_selects(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
) -> AppResult<usize> {
    if asset_ids.is_empty() {
        return Ok(0);
    }
    state.db.add_assets_to_default_selects(&asset_ids)
}

#[tauri::command]
pub async fn default_select_assets(state: State<'_, AppState>) -> AppResult<Vec<Asset>> {
    let mut assets = state.db.default_select_assets()?;
    attach_thumbnail_data(&state, &mut assets);
    Ok(assets)
}

#[tauri::command]
pub async fn export_default_selects_csv(state: State<'_, AppState>, path: String) -> AppResult<()> {
    let output_path = std::path::Path::new(&path);
    if !output_path.is_absolute()
        || output_path
            .parent()
            .filter(|parent| parent.is_dir())
            .is_none()
    {
        return Err(crate::core::error::AppError::InvalidPath(
            output_path.to_path_buf(),
        ));
    }
    crate::core::export::write_select_items_csv(output_path, &state.db.default_select_items()?)
}

#[tauri::command]
pub async fn export_default_selects_json(
    state: State<'_, AppState>,
    path: String,
) -> AppResult<()> {
    let output_path = std::path::Path::new(&path);
    if !output_path.is_absolute()
        || output_path
            .parent()
            .filter(|parent| parent.is_dir())
            .is_none()
    {
        return Err(crate::core::error::AppError::InvalidPath(
            output_path.to_path_buf(),
        ));
    }
    crate::core::export::write_select_items_json(output_path, &state.db.default_select_items()?)
}

#[tauri::command]
pub async fn export_default_selects_edl(state: State<'_, AppState>, path: String) -> AppResult<()> {
    let output_path = std::path::Path::new(&path);
    if !output_path.is_absolute()
        || output_path
            .parent()
            .filter(|parent| parent.is_dir())
            .is_none()
    {
        return Err(crate::core::error::AppError::InvalidPath(
            output_path.to_path_buf(),
        ));
    }
    crate::core::export::write_select_items_edl(output_path, &state.db.default_select_items()?)
}

#[tauri::command]
pub async fn export_default_selects_fcpxml(
    state: State<'_, AppState>,
    path: String,
) -> AppResult<()> {
    let output_path = std::path::Path::new(&path);
    if !output_path.is_absolute()
        || output_path
            .parent()
            .filter(|parent| parent.is_dir())
            .is_none()
    {
        return Err(crate::core::error::AppError::InvalidPath(
            output_path.to_path_buf(),
        ));
    }
    crate::core::export::write_select_items_fcpxml(output_path, &state.db.default_select_items()?)
}

fn attach_thumbnail_data(state: &AppState, assets: &mut [Asset]) {
    for asset in assets {
        attach_thumbnail_data_url(state, asset);
    }
}

fn attach_thumbnail_data_url(state: &AppState, asset: &mut Asset) {
    let path = state.cache.thumbnail_path(&asset.id, "cover");
    if let Ok(bytes) = std::fs::read(path) {
        asset.thumbnail_data_url =
            Some(format!("data:image/jpeg;base64,{}", base64_encode(&bytes)));
    }
}

fn attach_segment_thumbnail_data_url(state: &AppState, segment: &mut crate::models::Segment) {
    let Some(path) = segment.thumbnail_path.as_ref() else {
        return;
    };
    let Ok(cache_root) = std::fs::canonicalize(state.cache.root()) else {
        return;
    };
    let Ok(path) = std::fs::canonicalize(path) else {
        return;
    };
    if !path.starts_with(&cache_root)
        || !matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("jpg") | Some("jpeg")
        )
    {
        return;
    }
    const MAX_THUMBNAIL_RESPONSE_BYTES: usize = 3 * 1024 * 1024;
    if let Ok(bytes) = std::fs::read(path) {
        if bytes.len() <= MAX_THUMBNAIL_RESPONSE_BYTES {
            segment.thumbnail_data_url =
                Some(format!("data:image/jpeg;base64,{}", base64_encode(&bytes)));
        }
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(TABLE[(((first & 0b0000_0011) << 4) | (second >> 4)) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            TABLE[(((second & 0b0000_1111) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            TABLE[(third & 0b0011_1111) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::{base64_encode, diversity_order, tag_matches};

    #[test]
    fn encodes_thumbnail_bytes_as_base64() {
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"M"), "TQ==");
    }

    #[test]
    fn matches_acg_tags_case_insensitively() {
        let tags = vec!["Game UI".to_string(), "Pink Hair".to_string()];
        assert!(tag_matches(&tags, "pink hair"));
        assert!(tag_matches(&tags, "UI"));
        assert!(!tag_matches(&tags, "water"));
        assert!(!tag_matches(&tags, "   "));
    }

    #[test]
    fn interleaves_same_score_candidates_by_library_without_dropping_any() {
        let libraries = vec!["library-a", "library-a", "library-b", "library-c"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(diversity_order(&libraries), vec![0, 2, 3, 1]);
    }
}
