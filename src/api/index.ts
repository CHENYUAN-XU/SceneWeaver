import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import type {
  Asset,
  AssetTextSource,
  TextCue,
  CreateLibraryRequest,
  Job,
  Library,
  ScanProgress,
  AppStats,
  Segment,
  SearchRequest, SearchResult,
  Entity, EntityReference, CreateEntityRequest, SelectCollection, CreateSelectCollectionRequest, SelectItem, UpdateSelectItemRequest,
  ReconnectLibraryRequest, ReconnectLibraryResult,
} from '@/types';

async function invoke<T>(...args: Parameters<typeof tauriInvoke>): Promise<T> {
  try {
    return await tauriInvoke<T>(...args);
  } catch (cause) {
    if (cause instanceof Error) throw cause;
    // Tauri rejects with raw strings or a serialized Rust AppError variant.
    const message = typeof cause === 'string' ? cause
      : cause && typeof cause === 'object'
        ? Object.values(cause).find((value): value is string => typeof value === 'string')
        : undefined;
    throw new Error(message || `调用失败（${args[0]}）`);
  }
}

// Libraries
export const createLibrary = (req: CreateLibraryRequest): Promise<Library> =>
  invoke('create_library', { req });

export const listLibraries = (): Promise<Library[]> => invoke('list_libraries');

export const getLibrary = (id: string): Promise<Library | null> =>
  invoke('get_library', { id });

export const deleteLibrary = (id: string): Promise<void> =>
  invoke('delete_library', { id });

export const startScan = (libraryId: string): Promise<Job> =>
  invoke('start_scan', { libraryId });

export const setLibraryWatchEnabled = (libraryId: string, enabled: boolean): Promise<void> =>
  invoke('set_library_watch_enabled', { libraryId, enabled });

export const reconnectLibrary = (req: ReconnectLibraryRequest): Promise<ReconnectLibraryResult> =>
  invoke('reconnect_library', { req });

export const pauseJob = (jobId: string): Promise<Job> =>
  invoke('pause_job', { jobId });

export const resumeJob = (jobId: string): Promise<Job> =>
  invoke('resume_job', { jobId });

export const cancelJob = (jobId: string): Promise<Job> =>
  invoke('cancel_job', { jobId });

export const retryJob = (jobId: string): Promise<Job> =>
  invoke('retry_job', { jobId });

// Jobs
export const listJobs = (): Promise<Job[]> => invoke('list_jobs');

// Assets
export const listAssets = (libraryId: string): Promise<Asset[]> =>
  invoke('list_assets', { libraryId });

export const getAsset = (id: string): Promise<Asset | null> =>
  invoke('get_asset', { id });

export const listAssetText = (assetId: string, source: AssetTextSource): Promise<TextCue[]> =>
  invoke('list_asset_text', { assetId, source });

export const analyzeAssetText = (assetId: string, source: AssetTextSource, language: string, modelPath: string | null): Promise<TextCue[]> =>
  invoke('analyze_asset_text', { assetId, source, language, modelPath });

export const deleteAssetText = (assetId: string, source: AssetTextSource): Promise<void> =>
  invoke('delete_asset_text', { assetId, source });

export const listSegments = (assetId: string): Promise<Segment[]> =>
  invoke('list_segments', { assetId });

export const detectAssetShots = (assetId: string): Promise<Segment[]> =>
  invoke('detect_asset_shots', { assetId });

export const segmentPreviewDataUrl = (assetId: string, segmentId: string): Promise<string | null> =>
  invoke('segment_preview_data_url', { assetId, segmentId });

export const searchAssets = (request: SearchRequest): Promise<SearchResult[]> =>
  invoke('search_assets', { request });
export const findSimilarAssets = (assetId: string): Promise<Asset[]> => invoke('find_similar_assets', { assetId });
export const findSimilarByReferenceImage = (imagePath: string): Promise<Asset[]> => invoke('find_similar_by_reference_image', { imagePath });
export const recentSearches = (): Promise<string[]> => invoke('recent_searches');

export const toggleFavorite = (assetId: string): Promise<boolean> =>
  invoke('toggle_favorite', { assetId });

export const favoriteAssetIds = (): Promise<string[]> => invoke('favorite_asset_ids');
export const setAssetHidden = (assetId: string, hidden: boolean): Promise<void> => invoke('set_asset_hidden', { assetId, hidden });
export const hiddenAssetIds = (): Promise<string[]> => invoke('hidden_asset_ids');

export const addToDefaultSelects = (assetId: string): Promise<void> =>
  invoke('add_to_default_selects', { assetId });
export const addAssetsToDefaultSelects = (assetIds: string[]): Promise<number> =>
  invoke('add_assets_to_default_selects', { assetIds });

export const addSegmentToDefaultSelects = (assetId: string, segmentId: string): Promise<void> =>
  invoke('add_segment_to_default_selects', { assetId, segmentId });

export const addAssetAcgTag = (assetId: string, value: string): Promise<string[]> =>
  invoke('add_asset_acg_tag', { assetId, value });
export const listAssetAcgTags = (assetId: string): Promise<string[]> =>
  invoke('list_asset_acg_tags', { assetId });
export const removeAssetAcgTag = (assetId: string, value: string): Promise<void> =>
  invoke('remove_asset_acg_tag', { assetId, value });

export const defaultSelectAssets = (): Promise<Asset[]> => invoke('default_select_assets');

export const exportDefaultSelectsCsv = (path: string): Promise<void> =>
  invoke('export_default_selects_csv', { path });
export const exportDefaultSelectsJson = (path: string): Promise<void> =>
  invoke('export_default_selects_json', { path });
export const exportDefaultSelectsEdl = (path: string): Promise<void> => invoke('export_default_selects_edl', { path });
export const exportDefaultSelectsFcpxml = (path: string): Promise<void> => invoke('export_default_selects_fcpxml', { path });
export const exportSelectCollectionCsv = (collectionId: string, path: string): Promise<void> =>
  invoke('export_select_collection_csv', { collectionId, path });
export const exportSelectCollectionJson = (collectionId: string, path: string): Promise<void> =>
  invoke('export_select_collection_json', { collectionId, path });
export const exportSelectCollectionEdl = (collectionId: string, path: string): Promise<void> =>
  invoke('export_select_collection_edl', { collectionId, path });
export const exportSelectCollectionFcpxml = (collectionId: string, path: string): Promise<void> =>
  invoke('export_select_collection_fcpxml', { collectionId, path });
export const exportSelectCollectionContactSheet = (collectionId: string, path: string): Promise<void> =>
  invoke('export_select_collection_contact_sheet', { collectionId, path });
export const exportSelectCollectionContactSheetHtml = (collectionId: string, path: string): Promise<void> =>
  invoke('export_select_collection_contact_sheet_html', { collectionId, path });

export const listEntities = (): Promise<Entity[]> => invoke('list_entities');
export const createEntity = (request: CreateEntityRequest): Promise<Entity> => invoke('create_entity', { request });
export const addEntityReferenceImage = (entityId: string, imagePath: string, isPositive = true): Promise<EntityReference> => invoke('add_entity_reference_image', { entityId, imagePath, isPositive });
export const listEntityReferences = (entityId: string): Promise<EntityReference[]> => invoke('list_entity_references', { entityId });
export const removeEntityReference = (entityId: string, referenceId: string): Promise<void> => invoke('remove_entity_reference', { entityId, referenceId });
export const setEntityAssetFeedback = (entityId: string, assetId: string, isPositive: boolean): Promise<void> => invoke('set_entity_asset_feedback', { entityId, assetId, isPositive });
export const findAssetsForEntity = (entityId: string): Promise<Asset[]> => invoke('find_assets_for_entity', { entityId });

export const listSelectCollections = (): Promise<SelectCollection[]> => invoke('list_select_collections');
export const createSelectCollection = (request: CreateSelectCollectionRequest): Promise<SelectCollection> => invoke('create_select_collection', { request });
export const listSelectItems = (collectionId: string): Promise<SelectItem[]> => invoke('list_select_items', { collectionId });
export const addAssetsToSelectCollection = (collectionId: string, assetIds: string[]): Promise<number> =>
  invoke('add_assets_to_select_collection', { collectionId, assetIds });
export const addSegmentToSelectCollection = (collectionId: string, assetId: string, segmentId: string): Promise<void> =>
  invoke('add_segment_to_select_collection', { collectionId, assetId, segmentId });
export const addSelectItemTag = (itemId: string, value: string): Promise<string[]> =>
  invoke('add_select_item_tag', { itemId, value });
export const addSelectItemTagBatch = (itemIds: string[], value: string): Promise<number> =>
  invoke('add_select_item_tag_batch', { itemIds, value });
export const removeSelectItemTag = (itemId: string, value: string): Promise<void> =>
  invoke('remove_select_item_tag', { itemId, value });
export const updateSelectItem = (itemId: string, request: UpdateSelectItemRequest): Promise<SelectItem> => invoke('update_select_item', { itemId, request });
export const removeSelectItem = (itemId: string): Promise<void> => invoke('remove_select_item', { itemId });
export const moveSelectItem = (itemId: string, collectionId: string): Promise<void> => invoke('move_select_item', { itemId, collectionId });
export const reorderSelectItem = (itemId: string, targetPosition: number): Promise<void> => invoke('reorder_select_item', { itemId, targetPosition });

// Stats
export const getAppStats = (): Promise<AppStats> => invoke('get_app_stats');
export const cacheSize = (): Promise<number> => invoke('cache_size');
export const clearMediaCache = (): Promise<number> => invoke('clear_media_cache');
export const exportDatabaseSnapshot = (path: string): Promise<void> => invoke('export_database_snapshot', { path });
export const acgCreatorPackEnabled = (): Promise<boolean> => invoke('acg_creator_pack_enabled');
export const setAcgCreatorPackEnabled = (enabled: boolean): Promise<void> => invoke('set_acg_creator_pack_enabled', { enabled });
export type SemanticModelStatus = {
  ready: boolean;
  modelInstalled: boolean;
  runtimeAvailable: boolean;
  providerId: string;
  message: string;
};
export type SemanticIndexResult = {
  indexed: number;
  skipped: number;
  failed: number;
  entityReferencesIndexed: number;
  entityReferencesSkipped: number;
  entityReferencesFailed: number;
};
export const semanticModelStatus = (): Promise<SemanticModelStatus> => invoke('semantic_model_status');
export const installSemanticModel = (): Promise<SemanticModelStatus> => invoke('install_semantic_model');
export const reindexSemanticAssets = (): Promise<SemanticIndexResult> => invoke('reindex_semantic_assets');

// Shell
export const openAsset = (assetId: string): Promise<void> =>
  invoke('open_asset', { assetId });

export const revealAssetInFolder = (assetId: string): Promise<void> =>
  invoke('reveal_asset_in_folder', { assetId });

export const copyToClipboard = (text: string): Promise<void> =>
  invoke('copy_to_clipboard', { text });
export const copyAssetRelativePath = (assetId: string): Promise<void> =>
  invoke('copy_asset_relative_path', { assetId });

// Progress events are emitted from Rust and listened via Tauri event API
export type { ScanProgress };
