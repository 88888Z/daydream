export interface VideoParams {
  repeats: number;
  speed: number;
  volume: number;
}

export interface VideoItem {
  id: string;
  path: string;
  filename: string;
  local: VideoParams | null;
}

export interface GlobalSettings {
  default_params: VideoParams;
  idle_timeout_seconds: number;
  idle_enabled: boolean;
  autoplay_on_idle: boolean;
  start_on_boot: boolean;
  last_played_entry: number | null;
}

export interface AppConfig {
  global: GlobalSettings;
  videos: VideoItem[];
}

export interface AddVideosResult {
  items: VideoItem[];
  added: number;
  duplicates: number;
}
