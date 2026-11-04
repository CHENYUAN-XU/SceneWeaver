use std::sync::Arc;

use sceneweaver_lib::core::cache::CacheManager;
use sceneweaver_lib::core::db::Database;
use sceneweaver_lib::core::error::AppError;
use sceneweaver_lib::core::export::{
    write_csv, write_edl, write_fcpxml, write_json, write_select_contact_sheet_html,
    write_select_contact_sheet_png, write_select_items_csv, write_select_items_edl,
    write_select_items_fcpxml, write_select_items_json,
};
use sceneweaver_lib::core::job_queue::{JobControl, ProgressUpdate};
use sceneweaver_lib::core::scanner::Scanner;
use sceneweaver_lib::models::{
    AssetTextSource, Entity, EntityReference, IndexProfile, Job, JobStatus, JobType, Library,
    LibraryStatus, SearchRequest, SelectCollection, UpdateSelectItemRequest,
};

struct SilentProgress;

impl ProgressUpdate for SilentProgress {
    fn report_progress(&self, _progress: f64, _processed: i64, _total: i64, _errors: i64) {}
    fn report_step(&self, _step: String) {}
    fn report_total(&self, _total: i64) {}
}

struct CancelAfterLastFile<'a>(&'a JobControl);

impl ProgressUpdate for CancelAfterLastFile<'_> {
    fn report_progress(&self, _progress: f64, processed: i64, total: i64, _errors: i64) {
        if processed == total {
            self.0.cancel();
        }
    }
    fn report_step(&self, _step: String) {}
    fn report_total(&self, _total: i64) {}
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--sleep-for-timeout-test") {
        std::thread::sleep(std::time::Duration::from_secs(30));
        return Ok(());
    }
    let root = std::env::temp_dir().join(format!("sceneweaver-smoke-{}", uuid::Uuid::new_v4()));
    let media_root = root.join("中文 素材库");
    std::fs::create_dir_all(&media_root)?;
    let image_path = media_root.join("雨夜 角色.png");
    image::RgbImage::from_pixel(32, 18, image::Rgb([12, 34, 56])).save(&image_path)?;

    let db = Arc::new(Database::new(root.join("sceneweaver.db")));
    db.init()?;
    let legacy_database_path = root.join("legacy-v6.db");
    let legacy_connection = rusqlite::Connection::open(&legacy_database_path)?;
    legacy_connection.execute_batch(
        "CREATE TABLE assets (id TEXT PRIMARY KEY);
         CREATE TABLE asset_embeddings (asset_id TEXT PRIMARY KEY, provider_id TEXT NOT NULL, model_version TEXT NOT NULL, vector_json TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, FOREIGN KEY (asset_id) REFERENCES assets(id) ON DELETE CASCADE);
         CREATE TABLE entities (id TEXT PRIMARY KEY);
         CREATE TABLE entity_references (id TEXT PRIMARY KEY, entity_id TEXT NOT NULL, asset_id TEXT, image_path TEXT, embedding_ref TEXT, is_positive INTEGER NOT NULL, created_at INTEGER NOT NULL);
         INSERT INTO assets (id) VALUES ('legacy-asset');
         INSERT INTO entities (id) VALUES ('legacy-entity');
         INSERT INTO entity_references (id, entity_id, embedding_ref, is_positive, created_at) VALUES ('legacy-reference', 'legacy-entity', '[1.0, 0.0]', 1, 1);
         INSERT INTO asset_embeddings (asset_id, provider_id, model_version, vector_json, created_at, updated_at) VALUES ('legacy-asset', 'legacy-provider', 'v1', '[1.0, 0.0]', 1, 1);
         PRAGMA user_version = 6;",
    )?;
    drop(legacy_connection);
    let legacy_database = Database::new(&legacy_database_path);
    legacy_database.init()?;
    legacy_database.upsert_asset_embedding("legacy-asset", "new-provider", "v1", &[0.0, 1.0])?;
    let legacy_embedding_count: i64 = rusqlite::Connection::open(&legacy_database_path)?
        .query_row(
            "SELECT COUNT(*) FROM asset_embeddings WHERE asset_id = 'legacy-asset'",
            [],
            |row| row.get(0),
        )?;
    assert_eq!(legacy_embedding_count, 2);
    let legacy_reference_embedding_count: i64 = rusqlite::Connection::open(&legacy_database_path)?
        .query_row(
            "SELECT COUNT(*) FROM entity_reference_embeddings WHERE reference_id = 'legacy-reference' AND provider_id = 'local-color-histogram'",
            [],
            |row| row.get(0),
        )?;
    assert_eq!(legacy_reference_embedding_count, 1);
    let cache = Arc::new(CacheManager::new(root.join("cache")));
    cache.ensure_dirs()?;
    let disposable_cache = CacheManager::new(root.join("disposable-cache"));
    disposable_cache.ensure_dirs()?;
    std::fs::write(
        disposable_cache.thumbnail_path("temporary", "cover"),
        b"cache",
    )?;
    std::fs::write(disposable_cache.models_path().join("keep-model"), b"model")?;
    assert_eq!(disposable_cache.cache_size()?, 5);
    assert_eq!(disposable_cache.clear_cache()?, 5);
    assert_eq!(disposable_cache.cache_size()?, 0);
    assert!(disposable_cache.models_path().join("keep-model").is_file());
    let semantic_status = sceneweaver_lib::providers::semantic_clip::status(
        &cache.models_path(),
        &sceneweaver_lib::providers::semantic_clip::default_runtime_path(),
    );
    assert!(
        semantic_status.runtime_available,
        "bundled ONNX Runtime must be present"
    );
    assert!(
        !semantic_status.model_installed,
        "the smoke test must not implicitly download a semantic model"
    );
    for (repository, files) in [
        (
            "models--qdrant--clip-vit-b-32-vision",
            vec!["model.onnx", "preprocessor_config.json"],
        ),
        (
            "models--qdrant--clip-vit-b-32-text",
            vec![
                "model.onnx",
                "tokenizer.json",
                "config.json",
                "special_tokens_map.json",
                "tokenizer_config.json",
            ],
        ),
    ] {
        let snapshot = cache
            .models_path()
            .join(repository)
            .join("snapshots")
            .join("smoke");
        std::fs::create_dir_all(&snapshot)?;
        for file in files {
            std::fs::write(snapshot.join(file), "smoke")?;
        }
    }
    std::fs::write(cache.models_path().join("semantic-clip-v1.ready"), "smoke")?;
    assert!(
        sceneweaver_lib::providers::semantic_clip::status(
            &cache.models_path(),
            &sceneweaver_lib::providers::semantic_clip::default_runtime_path(),
        )
        .model_installed
    );
    std::fs::remove_file(
        cache
            .models_path()
            .join("models--qdrant--clip-vit-b-32-text")
            .join("snapshots")
            .join("smoke")
            .join("tokenizer.json"),
    )?;
    assert!(
        !sceneweaver_lib::providers::semantic_clip::status(
            &cache.models_path(),
            &sceneweaver_lib::providers::semantic_clip::default_runtime_path(),
        )
        .model_installed
    );
    let exclusion_safe_prompt =
        sceneweaver_lib::providers::semantic_clip::semantic_query_prompt_for_conditions(
            &["战斗".to_string()],
            &["近景".to_string(), "粉色头发".to_string()],
            "战斗 优先 近景、粉色头发 不要 游戏 UI、字幕",
        )
        .expect("known creator terms should produce a semantic prompt");
    assert!(exclusion_safe_prompt.contains("battle"));
    assert!(exclusion_safe_prompt.contains("pink hair"));
    assert!(!exclusion_safe_prompt.contains("subtitles"));
    std::fs::remove_dir_all(cache.models_path())?;
    std::fs::create_dir_all(cache.models_path())?;
    let now = chrono::Utc::now().timestamp_millis();
    let library = Library {
        id: uuid::Uuid::new_v4().to_string(),
        name: "Smoke test 素材库".to_string(),
        root_path: media_root.to_string_lossy().to_string(),
        status: LibraryStatus::Idle,
        index_profile: IndexProfile::Quick,
        include_patterns: vec!["**/*".to_string()],
        exclude_patterns: vec![],
        watch_enabled: false,
        last_scan_at: None,
        created_at: now,
        updated_at: now,
    };
    db.create_library(&library)?;
    let snapshot_path = root.join("sceneweaver-backup.db");
    db.export_snapshot(&snapshot_path)?;
    let snapshot_library_count: i64 = rusqlite::Connection::open(&snapshot_path)?.query_row(
        "SELECT COUNT(*) FROM libraries",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(snapshot_library_count, 1);
    assert!(db.export_snapshot(&snapshot_path).is_err());
    let job = Job {
        id: uuid::Uuid::new_v4().to_string(),
        job_type: JobType::Scan,
        library_id: Some(library.id.clone()),
        asset_id: None,
        status: JobStatus::Paused,
        priority: 0,
        progress: 0.5,
        current_step: "等待恢复".to_string(),
        checkpoint_json: Some("{\"processed\":1}".to_string()),
        error_code: None,
        error_message: None,
        started_at: Some(now),
        finished_at: None,
        created_at: now,
        updated_at: now,
    };
    db.create_job(&job)?;
    let interrupted_job = Job {
        id: uuid::Uuid::new_v4().to_string(),
        status: JobStatus::Running,
        current_step: "扫描中".to_string(),
        checkpoint_json: Some("{\"processed\":3,\"total\":9}".to_string()),
        ..job.clone()
    };
    db.create_job(&interrupted_job)?;
    let reopened = Database::new(root.join("sceneweaver.db"));
    assert_eq!(
        reopened.get_job(&job.id)?.expect("job must persist").status,
        JobStatus::Paused
    );
    assert!(reopened
        .list_active_jobs()?
        .iter()
        .all(|active_job| active_job.id != job.id));
    assert_eq!(
        reopened
            .active_scan_job_for_library(&library.id)?
            .expect("running scan must prevent a duplicate queue entry")
            .id,
        interrupted_job.id
    );
    let recovered = reopened.recover_interrupted_jobs()?;
    assert_eq!(recovered.len(), 1, "only running jobs may be recovered");
    assert_eq!(recovered[0].id, interrupted_job.id);
    assert_eq!(
        recovered[0].checkpoint_json,
        interrupted_job.checkpoint_json
    );
    assert_eq!(
        reopened
            .get_job(&interrupted_job.id)?
            .expect("interrupted job must persist")
            .status,
        JobStatus::Pending
    );
    assert_eq!(
        reopened
            .get_job(&job.id)?
            .expect("paused job must persist")
            .status,
        JobStatus::Paused
    );
    assert!(
        reopened.active_scan_job_for_library(&library.id)?.is_some(),
        "a recovered pending scan or an intentional paused scan must block duplicates"
    );
    let scanner = Scanner::new(Arc::clone(&db), Arc::clone(&cache));
    let control = JobControl::new();
    let progress = SilentProgress;

    let mut slow_probe = std::process::Command::new(std::env::current_exe()?);
    slow_probe
        .arg("--sleep-for-timeout-test")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let timeout_started = std::time::Instant::now();
    assert!(matches!(
        sceneweaver_lib::core::ffprobe::run_command_with_timeout(
            &mut slow_probe,
            std::time::Duration::from_millis(100),
            "smoke timeout",
        ),
        Err(AppError::FfprobeUnavailable(message)) if message == "smoke timeout"
    ));
    assert!(
        timeout_started.elapsed() < std::time::Duration::from_secs(2),
        "timed-out probe must be terminated rather than continue in background"
    );

    // Keep the 1,000-file acceptance path isolated from the feature smoke below:
    // it proves both first indexing and the following incremental no-op scan without
    // changing any of the later asset-count assertions.
    let batch_root = root.join("batch scan library");
    std::fs::create_dir_all(&batch_root)?;
    for index in 0..1_000_u16 {
        let color = (index % 256) as u8;
        image::RgbImage::from_pixel(2, 2, image::Rgb([color, 255 - color, color / 2]))
            .save(batch_root.join(format!("asset-{index:04}.png")))?;
    }
    let batch_library = Library {
        id: uuid::Uuid::new_v4().to_string(),
        name: "1,000 file scan smoke".to_string(),
        root_path: batch_root.to_string_lossy().to_string(),
        status: LibraryStatus::Idle,
        index_profile: IndexProfile::Quick,
        include_patterns: vec!["**/*".to_string()],
        exclude_patterns: vec![],
        watch_enabled: false,
        last_scan_at: None,
        created_at: now,
        updated_at: now,
    };
    db.create_library(&batch_library)?;
    let first_batch_scan = scanner.scan_library(&batch_library, &control, &progress)?;
    assert_eq!(first_batch_scan.changed, 1_000);
    assert_eq!(db.list_assets(&batch_library.id)?.len(), 1_000);
    let second_batch_scan = scanner.scan_library(&batch_library, &control, &progress)?;
    assert_eq!(second_batch_scan.unchanged, 1_000);
    assert_eq!(second_batch_scan.changed, 0);

    let mut long_path_dir = media_root.clone();
    for index in 0..8 {
        long_path_dir.push(format!("超长路径-{index:02}-abcdefghijklmnopqrstuvwxyz"));
    }
    std::fs::create_dir_all(&long_path_dir)?;
    let long_path_image = long_path_dir.join("长路径素材.png");
    image::RgbImage::from_pixel(12, 12, image::Rgb([90, 60, 30])).save(&long_path_image)?;
    assert!(long_path_image.to_string_lossy().chars().count() > 260);

    let first = scanner.scan_library(&library, &control, &progress)?;
    let assets = db.list_assets(&library.id)?;
    assert_eq!(first.changed, 2);
    assert_eq!(assets.len(), 2);
    let primary_asset = assets
        .iter()
        .find(|asset| asset.file_name == "雨夜 角色.png")
        .cloned()
        .expect("primary smoke image must be indexed");
    assert!(cache.thumbnail_path(&primary_asset.id, "cover").is_file());
    let long_path_asset = assets
        .iter()
        .find(|asset| asset.file_name == "长路径素材.png")
        .cloned()
        .expect("long-path smoke image must be indexed");
    assert!(cache.thumbnail_path(&long_path_asset.id, "cover").is_file());

    // A library may move to another disk or receive a different mount point.
    // Reconnection must retain stable asset IDs and asset-level selects for files
    // that retain their relative paths, while explicitly taking missing files offline.
    let relocation_source = root.join("relocation source");
    let relocation_nested = relocation_source.join("nested");
    std::fs::create_dir_all(&relocation_nested)?;
    let relocation_keep = relocation_nested.join("keep.png");
    let relocation_missing = relocation_nested.join("missing.png");
    image::RgbImage::from_pixel(20, 10, image::Rgb([20, 40, 60])).save(&relocation_keep)?;
    image::RgbImage::from_pixel(20, 10, image::Rgb([60, 40, 20])).save(&relocation_missing)?;
    let relocation_library = Library {
        id: uuid::Uuid::new_v4().to_string(),
        name: "Relocation smoke library".to_string(),
        root_path: relocation_source.to_string_lossy().to_string(),
        status: LibraryStatus::Idle,
        index_profile: IndexProfile::Quick,
        include_patterns: vec!["**/*".to_string()],
        exclude_patterns: vec![],
        watch_enabled: false,
        last_scan_at: None,
        created_at: now,
        updated_at: now,
    };
    db.create_library(&relocation_library)?;
    assert_eq!(
        scanner
            .scan_library(&relocation_library, &control, &progress)?
            .changed,
        2
    );
    let relocation_asset = db
        .list_assets(&relocation_library.id)?
        .into_iter()
        .find(|asset| asset.file_name == "keep.png")
        .expect("relocation asset must be indexed");
    let relocation_collection = SelectCollection {
        id: uuid::Uuid::new_v4().to_string(),
        name: "Relocation selects".to_string(),
        description: None,
        created_at: now,
        updated_at: now,
    };
    db.create_select_collection(&relocation_collection)?;
    db.add_asset_to_select_collection(&relocation_collection.id, &relocation_asset.id)?;
    assert!(cache
        .thumbnail_path(&relocation_asset.id, "cover")
        .is_file());
    std::fs::remove_file(&relocation_missing)?;
    let relocation_target = root.join("relocation target");
    std::fs::rename(&relocation_source, &relocation_target)?;
    let relocation_target = dunce::canonicalize(&relocation_target)?;
    let (reconnected_library, rebound_assets, offline_assets) =
        db.reconnect_library_root(&relocation_library.id, &relocation_target)?;
    assert_eq!(rebound_assets, 1);
    assert_eq!(offline_assets, 1);
    let rebound_asset = db
        .get_asset(&relocation_asset.id)?
        .expect("reconnected asset must retain its ID");
    assert!(rebound_asset
        .file_path
        .starts_with(relocation_target.to_string_lossy().as_ref()));
    assert_eq!(
        rebound_asset.status,
        sceneweaver_lib::models::AssetStatus::Indexed
    );
    assert_eq!(
        db.list_select_items(&relocation_collection.id)?[0].asset_id,
        relocation_asset.id
    );
    assert!(cache
        .thumbnail_path(&relocation_asset.id, "cover")
        .is_file());
    let reconnected_scan = scanner.scan_library(&reconnected_library, &control, &progress)?;
    assert_eq!(reconnected_scan.changed, 0);
    assert_eq!(reconnected_scan.unchanged, 1);
    assert_eq!(
        db.list_assets(&relocation_library.id)?
            .into_iter()
            .find(|asset| asset.file_name == "missing.png")
            .expect("missing relocation asset must remain tracked")
            .status,
        sceneweaver_lib::models::AssetStatus::Offline
    );
    let mut similar_asset = primary_asset.clone();
    similar_asset.id = uuid::Uuid::new_v4().to_string();
    similar_asset.normalized_path = format!("{}-similar", similar_asset.normalized_path);
    similar_asset.file_path = format!("{}-similar", similar_asset.file_path);
    similar_asset.file_name = "相似参考.png".to_string();
    db.create_or_update_asset(&similar_asset)?;
    let red = sceneweaver_lib::providers::visual_embedding::embed_color_query("红色")
        .expect("red colour query must be supported");
    let blue = sceneweaver_lib::providers::visual_embedding::embed_color_query("蓝色")
        .expect("blue colour query must be supported");
    db.upsert_asset_embedding(&primary_asset.id, "test", "v1", &red)?;
    db.upsert_asset_embedding(&similar_asset.id, "test", "v1", &blue)?;
    let embedding_count: i64 = rusqlite::Connection::open(root.join("sceneweaver.db"))?.query_row(
        "SELECT COUNT(*) FROM asset_embeddings WHERE asset_id = ?1",
        [&primary_asset.id],
        |row| row.get(0),
    )?;
    assert_eq!(embedding_count, 2);
    // A database created by the current schema must already advertise v8.
    // Reopening it must not replay the legacy single-provider migration.
    let reopened = Database::new(root.join("sceneweaver.db"));
    reopened.init()?;
    let schema_version: i64 = rusqlite::Connection::open(root.join("sceneweaver.db"))?.query_row(
        "PRAGMA user_version",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(schema_version, 10);
    let text_cues = sceneweaver_lib::core::text_analysis::parse_srt(
        "0\r\n00:00:01,250 --> 00:00:03,000\r\n灯塔 lighthouse_100%\r\n\r\n",
    );
    assert_eq!(text_cues[0].start_ms, Some(1_250));
    db.store_asset_text(&primary_asset, AssetTextSource::Speech, &text_cues)?;
    let mut text_request = SearchRequest {
        raw_query: String::new(),
        must: vec!["lighthouse_100%".into()],
        should: vec![],
        must_not: vec![],
        media_types: vec![],
        min_quality_score: None,
        date_start_ms: None,
        date_end_ms: None,
    };
    assert_eq!(
        reopened.search_assets_with_conditions(&text_request, 10)?[0].id,
        primary_asset.id
    );
    text_request.must_not.append(&mut text_request.must);
    assert!(reopened
        .assets_matching_nonsemantic_filters(&text_request, 100)?
        .iter()
        .all(|asset| asset.id != primary_asset.id));
    let reopened_embedding_count: i64 = rusqlite::Connection::open(root.join("sceneweaver.db"))?
        .query_row(
            "SELECT COUNT(*) FROM asset_embeddings WHERE asset_id = ?1",
            [&primary_asset.id],
            |row| row.get(0),
        )?;
    assert_eq!(reopened_embedding_count, 2);
    assert!(db
        .similar_assets_for_provider(&primary_asset.id, "test", 5)?
        .iter()
        .any(|asset| asset.id == similar_asset.id));
    assert_eq!(
        db.assets_for_embedding_provider("test", &red, 5)?[0].id,
        primary_asset.id
    );
    assert_eq!(
        db.add_asset_acg_tag(&primary_asset.id, "游戏 UI")?,
        vec!["游戏 UI".to_string()]
    );
    assert_eq!(
        db.search_assets_with_conditions(
            &SearchRequest {
                raw_query: String::new(),
                must: vec!["游戏 UI".to_string()],
                should: vec![],
                must_not: vec![],
                media_types: vec![],
                min_quality_score: None,
                date_start_ms: None,
                date_end_ms: None,
            },
            10,
        )?[0]
            .id,
        primary_asset.id
    );
    let exact_modified_date_request = SearchRequest {
        raw_query: String::new(),
        must: vec!["游戏 UI".to_string()],
        should: vec![],
        must_not: vec![],
        media_types: vec![],
        min_quality_score: None,
        date_start_ms: Some(primary_asset.modified_at),
        date_end_ms: Some(primary_asset.modified_at),
    };
    assert!(db
        .search_assets_with_conditions(&exact_modified_date_request, 10)?
        .iter()
        .any(|asset| asset.id == primary_asset.id));
    let after_modified_date_request = SearchRequest {
        date_start_ms: Some(primary_asset.modified_at + 1),
        date_end_ms: None,
        ..exact_modified_date_request
    };
    assert!(db
        .search_assets_with_conditions(&after_modified_date_request, 10)?
        .is_empty());
    db.remove_asset_acg_tag(&primary_asset.id, "游戏 UI")?;
    assert!(db.asset_acg_tags(&primary_asset.id)?.is_empty());
    let entity = Entity {
        id: uuid::Uuid::new_v4().to_string(),
        entity_type: "character".to_string(),
        name: "红色参考".to_string(),
        description: None,
        aliases: vec!["角色".to_string()],
        pack_id: None,
        created_at: now,
        updated_at: now,
    };
    db.create_entity(&entity)?;
    let positive_reference = EntityReference {
        id: uuid::Uuid::new_v4().to_string(),
        entity_id: entity.id.clone(),
        asset_id: None,
        image_path: Some(image_path.to_string_lossy().to_string()),
        is_positive: true,
        created_at: now,
    };
    let primary_colour_embedding =
        sceneweaver_lib::providers::visual_embedding::embed_image(&image_path)?;
    db.add_entity_reference(&positive_reference, &primary_colour_embedding)?;
    db.upsert_entity_reference_embedding(&positive_reference.id, "test", "v1", &red)?;
    let entity_request = SearchRequest {
        raw_query: "角色 雨夜".to_string(),
        must: vec!["角色".to_string()],
        should: vec!["雨夜".to_string()],
        must_not: vec![],
        media_types: vec![],
        min_quality_score: None,
        date_start_ms: None,
        date_end_ms: None,
    };
    assert!(db
        .entities_matching_search_request(&entity_request)?
        .iter()
        .any(|matched| matched.id == entity.id));
    assert!(db
        .entity_candidate_assets(&entity.id, false, 5)?
        .iter()
        .any(|asset| asset.id == primary_asset.id));
    let exclusion_only_entity_request = SearchRequest {
        raw_query: "不要角色".to_string(),
        must: vec![],
        should: vec![],
        must_not: vec!["角色".to_string()],
        media_types: vec![],
        min_quality_score: None,
        date_start_ms: None,
        date_end_ms: None,
    };
    assert!(db
        .entities_matching_search_request(&exclusion_only_entity_request)?
        .is_empty());
    assert_eq!(
        db.similar_assets_for_entity_provider(&entity.id, "test", 5)?[0].id,
        primary_asset.id
    );
    assert_eq!(
        db.similar_assets_for_entity(&entity.id, 5)?[0].id,
        primary_asset.id
    );
    assert!(db
        .assets_matching_entity_terms(&entity.id, 5)?
        .iter()
        .any(|asset| asset.id == primary_asset.id));
    db.remove_entity_reference(&entity.id, &positive_reference.id)?;
    assert!(db.list_entity_references(&entity.id)?.is_empty());
    db.set_entity_asset_feedback(&entity.id, &primary_asset.id, true)?;
    assert_eq!(
        db.similar_assets_for_entity(&entity.id, 5)?[0].id,
        primary_asset.id
    );
    db.set_entity_asset_feedback(&entity.id, &primary_asset.id, false)?;
    let feedback = db.list_entity_references(&entity.id)?;
    assert_eq!(feedback.len(), 1);
    assert_eq!(
        feedback[0].asset_id.as_deref(),
        Some(primary_asset.id.as_str())
    );
    assert!(!feedback[0].is_positive);
    db.remove_entity_reference(&entity.id, &feedback[0].id)?;
    assert!(db
        .search_assets_with_conditions(
            &SearchRequest {
                raw_query: String::new(),
                must: vec![],
                should: vec![],
                must_not: vec![],
                media_types: vec!["audio".to_string()],
                min_quality_score: None,
                date_start_ms: None,
                date_end_ms: None,
            },
            10,
        )?
        .is_empty());
    db.add_to_default_selects(&primary_asset.id)?;
    db.set_asset_hidden(&primary_asset.id, true)?;
    db.set_asset_hidden(&primary_asset.id, true)?;
    assert_eq!(db.hidden_asset_ids()?, vec![primary_asset.id.clone()]);
    db.set_asset_hidden(&primary_asset.id, false)?;
    assert!(db.hidden_asset_ids()?.is_empty());
    assert_eq!(
        db.add_assets_to_default_selects(&[primary_asset.id.clone()])?,
        0
    );
    let batch_collection = SelectCollection {
        id: uuid::Uuid::new_v4().to_string(),
        name: "Batch selects".to_string(),
        description: Some("batch insertion smoke test".to_string()),
        created_at: now,
        updated_at: now,
    };
    db.create_select_collection(&batch_collection)?;
    assert_eq!(
        db.add_assets_to_select_collection(&batch_collection.id, &[primary_asset.id.clone()])?,
        1
    );
    assert_eq!(
        db.add_assets_to_select_collection(&batch_collection.id, &[primary_asset.id.clone()])?,
        0
    );
    assert_eq!(db.list_select_items(&batch_collection.id)?.len(), 1);
    let selects = db.default_select_assets()?;
    assert_eq!(selects.len(), 1);
    let csv_path = root.join("selects.csv");
    write_csv(&csv_path, &selects)?;
    assert!(std::fs::read_to_string(&csv_path)?.contains("雨夜 角色.png"));
    let json_path = root.join("selects.json");
    write_json(&json_path, &selects)?;
    assert!(std::fs::read_to_string(&json_path)?.contains("file_path"));
    let edl_path = root.join("selects.edl");
    write_edl(&edl_path, &selects)?;
    assert!(std::fs::read_to_string(&edl_path)?.contains("TITLE: SceneWeaver Selects"));
    let fcpxml_path = root.join("selects.fcpxml");
    write_fcpxml(&fcpxml_path, &selects)?;
    let fcpxml = std::fs::read_to_string(&fcpxml_path)?;
    assert!(fcpxml.contains("<fcpxml"));
    assert!(fcpxml.contains("%E4%B8%AD%E6%96%87"));
    assert!(fcpxml.contains("hasVideo=\"1\"><media-rep kind=\"original-media\" src=\"file:"));
    assert!(fcpxml.contains("id=\"format-timeline\""));
    assert!(fcpxml.contains("id=\"asset-1\""));
    assert!(fcpxml.contains("ref=\"asset-1\""));

    let segments = sceneweaver_lib::core::scene_detect::build_segments(
        &primary_asset.id,
        5_000,
        &[1_000, 3_000],
    );
    db.replace_segments(&primary_asset.id, &segments)?;
    db.add_segment_to_default_selects(&primary_asset.id, &segments[1].id)?;
    db.add_segment_to_default_selects(&primary_asset.id, &segments[1].id)?;
    db.add_segment_to_select_collection(&batch_collection.id, &primary_asset.id, &segments[1].id)?;
    db.add_segment_to_select_collection(&batch_collection.id, &primary_asset.id, &segments[1].id)?;
    let custom_segment_item = db
        .list_select_items(&batch_collection.id)?
        .into_iter()
        .find(|item| item.segment_id.as_deref() == Some(segments[1].id.as_str()))
        .expect("custom collection must retain the selected segment");
    db.update_select_item(
        &custom_segment_item.id,
        &UpdateSelectItemRequest {
            rating: Some(4),
            note: Some("custom segment range".to_string()),
            recommended_in_ms: Some(1_500),
            recommended_out_ms: Some(2_500),
        },
    )?;
    let custom_segment_csv_path = root.join("custom-segment-selects.csv");
    write_select_items_csv(
        &custom_segment_csv_path,
        &db.list_select_items(&batch_collection.id)?,
    )?;
    let custom_segment_csv = std::fs::read_to_string(&custom_segment_csv_path)?;
    assert!(custom_segment_csv.contains("00:00:01.500"));
    assert!(custom_segment_csv.contains("custom segment range"));
    let default_collection = db
        .list_select_collections()?
        .into_iter()
        .find(|collection| collection.name == "我的选片")
        .expect("default selects collection must exist");
    let select_items = db.list_select_items(&default_collection.id)?;
    assert_eq!(select_items.len(), 2);
    let segment_item = select_items
        .iter()
        .find(|item| item.segment_id.as_deref() == Some(segments[1].id.as_str()))
        .expect("segment select item must be present");
    assert_eq!(
        segment_item
            .segment
            .as_ref()
            .map(|segment| segment.start_ms),
        Some(1_000)
    );
    db.reorder_select_item(&segment_item.id, 0)?;
    assert_eq!(
        db.list_select_items(&default_collection.id)?[0].id,
        segment_item.id
    );
    assert_eq!(
        db.add_select_item_tag(&segment_item.id, "高潮")?,
        vec!["高潮".to_string()]
    );
    assert_eq!(
        db.add_select_item_tag(&segment_item.id, "高潮")?,
        vec!["高潮".to_string()]
    );
    db.add_select_item_tag(&segment_item.id, "雨夜")?;
    let asset_select_item = db
        .list_select_items(&default_collection.id)?
        .into_iter()
        .find(|item| item.segment_id.is_none())
        .expect("asset select item must be present");
    assert_eq!(
        db.add_select_item_tag_batch(
            &[segment_item.id.clone(), asset_select_item.id.clone()],
            "保留",
        )?,
        2
    );
    assert_eq!(
        db.add_select_item_tag_batch(
            &[segment_item.id.clone(), asset_select_item.id.clone()],
            "保留",
        )?,
        0
    );
    assert!(db
        .add_select_item_tag_batch(
            &[
                asset_select_item.id.clone(),
                "missing-select-item".to_string()
            ],
            "必须回滚",
        )
        .is_err());
    assert!(!db
        .list_select_items(&default_collection.id)?
        .iter()
        .any(|item| item.tags.iter().any(|tag| tag == "必须回滚")));
    assert_eq!(
        db.list_select_items(&default_collection.id)?[0].tags,
        vec!["高潮".to_string(), "雨夜".to_string(), "保留".to_string()]
    );
    let mut range_items = db.list_select_items(&default_collection.id)?;
    assert_eq!(db.default_select_items()?.len(), range_items.len());
    let range_csv_path = root.join("range-selects.csv");
    write_select_items_csv(&range_csv_path, &range_items)?;
    assert!(std::fs::read_to_string(&range_csv_path)?.contains("00:00:01.000"));
    assert!(std::fs::read_to_string(&range_csv_path)?.contains("高潮 | 雨夜"));
    let range_json_path = root.join("range-selects.json");
    write_select_items_json(&range_json_path, &range_items)?;
    assert!(std::fs::read_to_string(&range_json_path)?.contains("\"start_ms\": 1000"));
    assert!(std::fs::read_to_string(&range_json_path)?.contains("\"tags\": ["));
    let range_edl_path = root.join("range-selects.edl");
    write_select_items_edl(&range_edl_path, &range_items)?;
    assert!(std::fs::read_to_string(&range_edl_path)?.contains("00:00:01:00"));
    range_items[0].asset.fps = Some(23.976);
    range_items[0].asset.width = Some(1920);
    range_items[0].asset.height = Some(1080);
    let range_fcpxml_path = root.join("range-selects.fcpxml");
    write_select_items_fcpxml(&range_fcpxml_path, &range_items)?;
    let range_fcpxml = std::fs::read_to_string(&range_fcpxml_path)?;
    assert!(range_fcpxml.contains("start=\"1000/1000s\""));
    assert!(range_fcpxml.contains("frameDuration=\"1001/24000s\""));
    assert!(range_fcpxml.contains("format=\"format-asset-1\""));
    assert!(range_fcpxml.contains("<media-rep kind=\"original-media\""));
    let contact_sheet_path = root.join("range-selects.png");
    write_select_contact_sheet_png(&contact_sheet_path, &range_items, &cache)?;
    assert_eq!(image::open(&contact_sheet_path)?.width(), 1_000);
    let contact_sheet_html_path = root.join("range-selects.html");
    write_select_contact_sheet_html(&contact_sheet_html_path, &range_items, &cache)?;
    let contact_sheet_html = std::fs::read_to_string(&contact_sheet_html_path)?;
    assert!(contact_sheet_html.contains("SceneWeaver 选片联系表"));
    assert!(contact_sheet_html.contains("data:image/jpeg;base64,"));
    assert!(contact_sheet_html.contains("00:00:01.000"));
    assert!(contact_sheet_html.contains("高潮 · 雨夜"));

    db.replace_segments(&primary_asset.id, &[])?;
    assert_eq!(db.list_select_items(&default_collection.id)?.len(), 1);
    assert!(db.remove_select_item_tag(&segment_item.id, "高潮").is_err());

    // A source replacement must invalidate content-derived vectors and
    // segment ranges while keeping the stable asset-level select intact.
    let stale_segments =
        sceneweaver_lib::core::scene_detect::build_segments(&primary_asset.id, 2_000, &[1_000]);
    db.replace_segments(&primary_asset.id, &stale_segments)?;
    db.add_segment_to_default_selects(&primary_asset.id, &stale_segments[0].id)?;
    db.set_entity_asset_feedback(&entity.id, &primary_asset.id, true)?;
    assert!(db
        .list_select_items(&default_collection.id)?
        .iter()
        .any(|item| item.segment_id.is_some()));
    let feedback_provider_count: i64 = rusqlite::Connection::open(root.join("sceneweaver.db"))?
        .query_row(
            "SELECT COUNT(*) FROM entity_reference_embeddings e JOIN entity_references r ON r.id = e.reference_id WHERE r.entity_id = ?1 AND e.provider_id = 'test'",
            [&entity.id],
            |row| row.get(0),
        )?;
    assert_eq!(feedback_provider_count, 1);

    let corrupt_image = media_root.join("损坏素材.png");
    std::fs::write(&corrupt_image, b"not an image")?;
    let scan_with_corrupt_file = scanner.scan_library(&library, &control, &progress)?;
    assert_eq!(scan_with_corrupt_file.changed, 1);
    assert!(db
        .list_assets(&library.id)?
        .iter()
        .any(|asset| asset.normalized_path
            == sceneweaver_lib::core::scanner::normalize_path(&long_path_image)));

    let second = scanner.scan_library(&library, &control, &progress)?;
    assert_eq!(second.unchanged, 3);

    std::thread::sleep(std::time::Duration::from_millis(20));
    image::RgbImage::from_pixel(48, 27, image::Rgb([56, 34, 12])).save(&image_path)?;
    let changed = scanner.scan_library(&library, &control, &progress)?;
    assert!(db
        .list_asset_text(&primary_asset.id, AssetTextSource::Speech)?
        .is_empty());
    let changed_asset = db
        .list_assets(&library.id)?
        .into_iter()
        .find(|asset| asset.id == primary_asset.id)
        .expect("original asset must remain present");
    assert_eq!(changed.changed, 1);
    assert_eq!(
        (changed_asset.width, changed_asset.height),
        (Some(48), Some(27))
    );
    let remaining_embeddings: i64 = rusqlite::Connection::open(root.join("sceneweaver.db"))?
        .query_row(
            "SELECT COUNT(*) FROM asset_embeddings WHERE asset_id = ?1",
            [&primary_asset.id],
            |row| row.get(0),
        )?;
    assert_eq!(remaining_embeddings, 1);
    assert!(db.list_segments(&primary_asset.id)?.is_empty());
    assert!(db
        .list_select_items(&default_collection.id)?
        .iter()
        .all(|item| item.segment_id.is_none()));
    let stale_feedback_provider_count: i64 = rusqlite::Connection::open(root.join("sceneweaver.db"))?
        .query_row(
            "SELECT COUNT(*) FROM entity_reference_embeddings e JOIN entity_references r ON r.id = e.reference_id WHERE r.entity_id = ?1 AND e.provider_id = 'test'",
            [&entity.id],
            |row| row.get(0),
        )?;
    assert_eq!(stale_feedback_provider_count, 0);
    assert_eq!(
        db.similar_assets_for_entity(&entity.id, 5)?[0].id,
        primary_asset.id
    );

    std::fs::remove_file(&image_path)?;
    let cancel_control = JobControl::new();
    assert!(matches!(
        scanner.scan_library(
            &library,
            &cancel_control,
            &CancelAfterLastFile(&cancel_control)
        ),
        Err(AppError::Cancelled)
    ));
    assert_eq!(
        db.get_asset(&primary_asset.id)?
            .expect("original asset")
            .status,
        sceneweaver_lib::models::AssetStatus::Indexed
    );
    let third = scanner.scan_library(&library, &control, &progress)?;
    assert_eq!(third.removed, 1);
    assert_eq!(
        db.list_assets(&library.id)?
            .into_iter()
            .find(|asset| asset.id == primary_asset.id)
            .expect("original asset must remain present")
            .status,
        sceneweaver_lib::models::AssetStatus::Offline
    );

    let exif_root = root.join("EXIF 照片验证");
    std::fs::create_dir_all(&exif_root)?;
    let exif_photo_path = exif_root.join("带时区照片.jpg");
    write_timezone_qualified_exif_jpeg(&exif_photo_path)?;
    let exif_library = Library {
        id: uuid::Uuid::new_v4().to_string(),
        name: "EXIF smoke library".to_string(),
        root_path: exif_root.to_string_lossy().to_string(),
        status: LibraryStatus::Idle,
        index_profile: IndexProfile::Quick,
        include_patterns: vec!["**/*".to_string()],
        exclude_patterns: vec![],
        watch_enabled: false,
        last_scan_at: None,
        created_at: now,
        updated_at: now,
    };
    db.create_library(&exif_library)?;
    assert_eq!(
        scanner
            .scan_library(&exif_library, &control, &progress)?
            .changed,
        1
    );
    assert_eq!(
        db.list_assets(&exif_library.id)?[0].capture_time,
        Some(1_714_979_289_000)
    );

    if which::which("ffmpeg").is_ok() && which::which("ffprobe").is_ok() {
        let video_root = root.join("视频 验证");
        std::fs::create_dir_all(&video_root)?;
        let video_path = video_root.join("红蓝切换.mp4");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=64x48:d=1",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=64x48:d=1",
                "-filter_complex",
                "[0:v]drawbox=x=2:y=32:w=8:h=6:color=white:t=fill,drawbox=x=54:y=32:w=8:h=6:color=white:t=fill[v0];[1:v]drawbox=x=2:y=32:w=8:h=6:color=white:t=fill,drawbox=x=54:y=32:w=8:h=6:color=white:t=fill[v1];[v0][v1]concat=n=2:v=1:a=0",
                "-c:v",
                "libx264",
                "-metadata",
                "creation_time=2024-05-06T07:08:09.123Z",
            ])
            .arg(&video_path)
            .status()?;
        assert!(status.success(), "synthetic video generation must succeed");
        let video_library = Library {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Video smoke library".to_string(),
            root_path: video_root.to_string_lossy().to_string(),
            status: LibraryStatus::Idle,
            index_profile: IndexProfile::Quick,
            include_patterns: vec!["**/*".to_string()],
            exclude_patterns: vec![],
            watch_enabled: false,
            last_scan_at: None,
            created_at: now,
            updated_at: now,
        };
        db.create_library(&video_library)?;
        assert_eq!(
            scanner
                .scan_library(&video_library, &control, &progress)?
                .changed,
            1
        );
        let video_asset = db
            .list_assets(&video_library.id)?
            .pop()
            .expect("video must index");
        assert_eq!(
            video_asset.media_type,
            sceneweaver_lib::models::MediaType::Video
        );
        assert!(video_asset.duration_ms.unwrap_or(0) >= 1_900);
        assert_eq!(video_asset.capture_time, Some(1_714_979_289_000));
        let precise_id = uuid::Uuid::new_v4().to_string();
        let mut precise_segments = sceneweaver_lib::core::scene_detect::build_segments(
            &precise_id,
            video_asset.duration_ms.unwrap(),
            &[],
        );
        assert!(
            sceneweaver_lib::core::video_derivatives::generate_for_segments(
                &cache,
                &video_path,
                &precise_id,
                &mut precise_segments,
                &IndexProfile::Precise,
            )?
        );
        assert!(precise_segments[0].quality_score.is_some());
        assert!(
            !std::fs::read_dir(cache.root().join("keyframes"))?.any(|entry| {
                entry.is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with("sample-"))
            })
        );
        let mut video_segments = db.list_segments(&video_asset.id)?;
        assert!(!video_segments.is_empty(), "video must create segments");
        assert!(video_segments
            .iter()
            .any(|segment| segment.thumbnail_path.is_some()));
        assert!(video_segments
            .iter()
            .any(|segment| segment.preview_path.is_some()));
        assert!(video_segments
            .iter()
            .any(|segment| segment.quality_score.is_some()));
        assert!(video_segments
            .iter()
            .any(|segment| segment.black_frame_score.is_some()));
        assert!(video_segments
            .iter()
            .any(|segment| segment.blur_score.is_some()));
        assert!(video_segments
            .iter()
            .any(|segment| segment.subtitle_present.is_some()));
        assert!(
            video_segments
                .iter()
                .any(|segment| segment.game_ui == Some(true)),
            "synthetic dual-corner HUD must be detected automatically"
        );

        // Subtitle is a structured segment signal, not merely a filename
        // keyword. Subtitle, black-frame and blur labels must be usable as
        // hard SQLite constraints even by semantic/entity retrieval paths.
        video_segments[0].subtitle_present = Some(true);
        video_segments[0].game_ui = Some(true);
        video_segments[0].black_frame_score = Some(0.9);
        video_segments[0].blur_score = Some(0.9);
        assert!(video_segments.len() >= 2, "test video must have two shots");
        video_segments[1].subtitle_present = Some(false);
        video_segments[1].game_ui = Some(false);
        video_segments[1].black_frame_score = Some(0.1);
        video_segments[1].blur_score = Some(0.1);
        db.replace_segments(&video_asset.id, &video_segments)?;
        let subtitle_request = SearchRequest {
            raw_query: String::new(),
            must: vec!["字幕".to_string()],
            should: vec![],
            must_not: vec![],
            media_types: vec!["video".to_string()],
            min_quality_score: None,
            date_start_ms: None,
            date_end_ms: None,
        };
        assert!(db
            .search_assets_with_conditions(&subtitle_request, 10)?
            .iter()
            .any(|asset| asset.id == video_asset.id));
        assert!(db
            .assets_matching_nonsemantic_filters(&subtitle_request, 10)?
            .iter()
            .any(|asset| asset.id == video_asset.id));
        let ui_request = SearchRequest {
            raw_query: String::new(),
            must: vec!["HUD".to_string()],
            should: vec![],
            must_not: vec![],
            media_types: vec!["video".to_string()],
            min_quality_score: None,
            date_start_ms: None,
            date_end_ms: None,
        };
        assert!(db
            .search_assets_with_conditions(&ui_request, 10)?
            .iter()
            .any(|asset| asset.id == video_asset.id));
        let clean_ui_segment_request = SearchRequest {
            must: vec![],
            must_not: vec!["game ui".to_string()],
            ..ui_request
        };
        assert!(db
            .assets_matching_nonsemantic_filters(&clean_ui_segment_request, 10)?
            .iter()
            .any(|asset| asset.id == video_asset.id));
        let clean_subtitle_segment_request = SearchRequest {
            must: vec![],
            must_not: vec!["subtitle".to_string()],
            ..subtitle_request
        };
        assert!(db
            .assets_matching_nonsemantic_filters(&clean_subtitle_segment_request, 10)?
            .iter()
            .any(|asset| asset.id == video_asset.id));

        let quality_label_request = SearchRequest {
            raw_query: String::new(),
            must: vec!["黑帧".to_string()],
            should: vec!["模糊".to_string()],
            must_not: vec![],
            media_types: vec!["video".to_string()],
            min_quality_score: None,
            date_start_ms: None,
            date_end_ms: None,
        };
        assert!(db
            .search_assets_with_conditions(&quality_label_request, 10)?
            .iter()
            .any(|asset| asset.id == video_asset.id));
        let safe_segment_request = SearchRequest {
            must: vec![],
            should: vec![],
            must_not: vec!["blur".to_string()],
            ..quality_label_request.clone()
        };
        assert!(db
            .assets_matching_nonsemantic_filters(&safe_segment_request, 10)?
            .iter()
            .any(|asset| asset.id == video_asset.id));
        let conflicting_segment_request = SearchRequest {
            must: vec!["字幕".to_string()],
            should: vec![],
            must_not: vec!["模糊".to_string()],
            ..quality_label_request
        };
        assert!(db
            .search_assets_with_conditions(&conflicting_segment_request, 10)?
            .iter()
            .all(|asset| asset.id != video_asset.id));
    }

    std::fs::remove_dir_all(&root)?;
    println!(
        "core smoke passed: 1,000-file first/incremental scan, Chinese + spaced path, long/corrupt media resilience, thumbnail, multi-provider visual index, bundled semantic runtime, offline detection, selects exports, segment selects and optional FFmpeg video derivatives"
    );
    Ok(())
}

fn write_timezone_qualified_exif_jpeg(
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    image::RgbImage::from_pixel(12, 12, image::Rgb([30, 60, 90])).save(path)?;
    let jpeg = std::fs::read(path)?;
    assert!(jpeg.starts_with(&[0xff, 0xd8]));

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&42_u16.to_le_bytes());
    tiff.extend_from_slice(&8_u32.to_le_bytes());
    tiff.extend_from_slice(&1_u16.to_le_bytes());
    tiff.extend_from_slice(&0x8769_u16.to_le_bytes());
    tiff.extend_from_slice(&4_u16.to_le_bytes());
    tiff.extend_from_slice(&1_u32.to_le_bytes());
    tiff.extend_from_slice(&26_u32.to_le_bytes());
    tiff.extend_from_slice(&0_u32.to_le_bytes());

    tiff.extend_from_slice(&2_u16.to_le_bytes());
    tiff.extend_from_slice(&0x9003_u16.to_le_bytes());
    tiff.extend_from_slice(&2_u16.to_le_bytes());
    tiff.extend_from_slice(&20_u32.to_le_bytes());
    tiff.extend_from_slice(&56_u32.to_le_bytes());
    tiff.extend_from_slice(&0x9011_u16.to_le_bytes());
    tiff.extend_from_slice(&2_u16.to_le_bytes());
    tiff.extend_from_slice(&7_u32.to_le_bytes());
    tiff.extend_from_slice(&76_u32.to_le_bytes());
    tiff.extend_from_slice(&0_u32.to_le_bytes());
    tiff.extend_from_slice(b"2024:05:06 15:08:09\0");
    tiff.extend_from_slice(b"+08:00\0");

    let mut app1_payload = b"Exif\0\0".to_vec();
    app1_payload.extend_from_slice(&tiff);
    let mut output = Vec::with_capacity(jpeg.len() + app1_payload.len() + 4);
    output.extend_from_slice(&jpeg[..2]);
    output.extend_from_slice(&[0xff, 0xe1]);
    output.extend_from_slice(&((app1_payload.len() + 2) as u16).to_be_bytes());
    output.extend_from_slice(&app1_payload);
    output.extend_from_slice(&jpeg[2..]);
    std::fs::write(path, output)?;
    Ok(())
}
