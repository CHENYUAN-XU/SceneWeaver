export type LibraryStatus = 'idle' | 'scanning' | 'paused' | 'error';
export type IndexProfile = 'quick' | 'balanced' | 'precise';
export type AssetTextSource = 'ocr' | 'speech';

export interface TextCue {
  text: string;
  start_ms: number | null;
  end_ms: number | null;
}
export type MediaType = 'image' | 'video' | 'audio';
export type AssetStatus = 'pending' | 'indexed' | 'error' | 'offline';
export type JobStatus = 'pending' | 'running' | 'paused' | 'completed' | 'failed' | 'cancelled';
export type JobType = 'scan' | 'thumbnail' | 'shot_detect' | 'index';

export interface Library {
  id: string;
  name: string;
  root_path: string;
  status: LibraryStatus;
  index_profile: IndexProfile;
  include_patterns: string[];
  exclude_patterns: string[];
  watch_enabled: boolean;
  last_scan_at: number | null;
  created_at: number;
  updated_at: number;
}

export interface CreateLibraryRequest {
  name: string;
  root_path: string;
  index_profile?: IndexProfile;
  include_patterns?: string[];
  exclude_patterns?: string[];
}

export interface ReconnectLibraryRequest {
  library_id: string;
  root_path: string;
}

export interface ReconnectLibraryResult {
  library: Library;
  job: Job;
  rebased_assets: number;
  offline_assets: number;
}

export interface Asset {
  id: string;
  library_id: string;
  media_type: MediaType;
  file_path: string;
  normalized_path: string;
  file_name: string;
  extension: string;
  size_bytes: number;
  modified_at: number;
  quick_fingerprint: string;
  full_hash: string | null;
  duration_ms: number | null;
  width: number | null;
  height: number | null;
  fps: number | null;
  codec: string | null;
  capture_time: number | null;
  status: AssetStatus;
  index_level: number;
  analysis_version: number;
  created_at: number;
  updated_at: number;
  thumbnail_data_url?: string;
}

export interface Segment {
  id: string;
  asset_id: string;
  segment_type: string;
  segment_index: number;
  start_ms: number;
  end_ms: number;
  duration_ms: number;
  quality_score: number | null;
  subtitle_present: boolean | null;
  game_ui: boolean | null;
  representative_frame_path?: string | null;
  thumbnail_path?: string | null;
  thumbnail_data_url?: string | null;
  preview_path?: string | null;
  black_frame_score?: number | null;
  blur_score?: number | null;
}

export interface SearchRequest {
  raw_query: string;
  must: string[];
  should: string[];
  must_not: string[];
  media_types?: MediaType[];
  min_quality_score?: number | null;
  date_start_ms?: number | null;
  date_end_ms?: number | null;
}
export interface SearchResult { asset: Asset; score: number; match_reasons: string[]; unmet_should: string[]; matching_segment_ids: string[]; }

export interface Entity { id: string; entity_type: string; name: string; description: string | null; aliases: string[]; created_at: number; updated_at: number; }
export interface EntityReference { id: string; entity_id: string; asset_id: string | null; image_path: string | null; is_positive: boolean; created_at: number; }
export interface CreateEntityRequest { entity_type: string; name: string; description?: string; aliases: string[]; }

export interface SelectCollection { id: string; name: string; description: string | null; created_at: number; updated_at: number; }
export interface CreateSelectCollectionRequest { name: string; description?: string; }
export interface SelectItem {
  id: string; collection_id: string; asset_id: string; segment_id: string | null; position: number;
  rating: number | null; note: string | null; recommended_in_ms: number | null; recommended_out_ms: number | null;
  tags: string[];
  created_at: number; updated_at: number; asset: Asset; segment: Segment | null;
}
export interface UpdateSelectItemRequest { rating: number | null; note: string | null; recommended_in_ms: number | null; recommended_out_ms: number | null; }

export interface Job {
  id: string;
  job_type: JobType;
  library_id: string | null;
  asset_id: string | null;
  status: JobStatus;
  priority: number;
  progress: number;
  current_step: string;
  checkpoint_json: string | null;
  error_code: string | null;
  error_message: string | null;
  started_at: number | null;
  finished_at: number | null;
  created_at: number;
  updated_at: number;
}

export interface ScanProgress {
  job_id: string;
  library_id: string;
  status: JobStatus;
  progress: number;
  current_step: string;
  processed: number;
  total: number;
  errors: number;
}

export interface MediaProbeInfo {
  duration_ms: number | null;
  width: number | null;
  height: number | null;
  fps: number | null;
  codec: string | null;
}

export interface AppStats {
  library_count: number;
  asset_count: number;
  video_count: number;
  image_count: number;
  job_count: number;
}
