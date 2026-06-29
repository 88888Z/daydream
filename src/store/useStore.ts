import { create } from "zustand";
import type { AppConfig, AddVideosResult, GlobalSettings, VideoItem, VideoParams } from "../types";

interface NowPlaying {
  itemIndex: number;
  entryIndex: number;
  totalEntries: number;
}

interface AppState {
  config: AppConfig;
  editingItem: VideoItem | null;
  showGlobalSettings: boolean;
  loaded: boolean;
  isPlaying: boolean;
  nowPlaying: NowPlaying | null;
  idleRemaining: number;
  thumbnailCache: Record<string, string>;
  selectedIds: string[];
  addProgress: { current: number; total: number } | null;
  toast: { message: string; variant: "success" | "error" | "info" } | null;
  showToast: (message: string, variant: "success" | "error" | "info") => void;
  clearToast: () => void;
  loadConfig: () => Promise<void>;
  setEditingItem: (item: VideoItem | null) => void;
  setShowGlobalSettings: (show: boolean) => void;
  addVideos: (paths: string[]) => Promise<void>;
  removeVideo: (id: string) => Promise<void>;
  reorderVideos: (ids: string[]) => Promise<void>;
  updateVideoParams: (id: string, params: VideoParams | null) => Promise<void>;
  updateGlobalSettings: (settings: GlobalSettings) => Promise<void>;
  play: () => Promise<void>;
  stop: () => Promise<void>;
  getThumbnail: (videoPath: string) => Promise<string>;
  toggleSelection: (id: string) => void;
  selectAll: () => void;
  clearSelection: () => void;
  deleteSelected: () => Promise<void>;
  deleteAll: () => Promise<void>;
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const start = performance.now();
  const { invoke: tauriInvoke } = await import("@tauri-apps/api/core");
  try {
    return await tauriInvoke<T>(cmd, args);
  } finally {
    console.log(`[TIMING] invoke ${cmd} ${Math.round(performance.now() - start)}ms`);
  }
}

export const useStore = create<AppState>((set, get) => ({
  config: {
    global: {
      default_params: { repeats: 1, speed: 1, volume: 100 },
      idle_timeout_seconds: 120,
      idle_enabled: false,
      autoplay_on_idle: true,
      start_on_boot: false,
      last_played_entry: null,
    },
    videos: [],
  },
  editingItem: null,
  showGlobalSettings: false,
  loaded: false,
  isPlaying: false,
  nowPlaying: null,
  idleRemaining: 120,
  thumbnailCache: {},
  selectedIds: [],
  addProgress: null,
  toast: null,
  showToast: (message, variant) => {
    set({ toast: { message, variant } });
  },
  clearToast: () => set({ toast: null }),

  loadConfig: async () => {
    try {
      const config = await invoke<AppConfig>("get_config");
      set({ config, loaded: true, idleRemaining: config.global.idle_timeout_seconds });

      const { listen } = await import("@tauri-apps/api/event");
      listen("playback-started", () => set({ isPlaying: true }));
      listen("playback-stopped", () => set({ isPlaying: false, nowPlaying: null }));
      listen<NowPlaying>("now-playing", (event) => {
        set({ nowPlaying: event.payload });
      });
      listen<{ remaining: number }>("idle-status", (event) => {
        console.log("idle-status received:", event.payload.remaining);
        set({ idleRemaining: event.payload.remaining });
      });
      listen<{ current: number; total: number }>("add-progress", (event) => {
        const p = event.payload;
        if (p.current >= p.total) {
          set({ addProgress: null });
        } else {
          set({ addProgress: p });
        }
      });
    } catch (e) {
      console.error("Failed to load config", e);
    }
  },

  setEditingItem: (item) => set({ editingItem: item }),
  setShowGlobalSettings: (show) => set({ showGlobalSettings: show }),

  addVideos: async (paths) => {
    try {
      const result = await invoke<AddVideosResult>("add_videos", { paths });
      set((s) => ({ config: { ...s.config, videos: [...s.config.videos, ...result.items] } }));
      const showToast = useStore.getState().showToast;
      if (result.duplicates > 0 && result.added > 0) {
        showToast(`Added ${result.added} video${result.added !== 1 ? "s" : ""} (${result.duplicates} duplicate${result.duplicates !== 1 ? "s" : ""} skipped)`, "success");
      } else if (result.added > 0) {
        showToast(`Added ${result.added} video${result.added !== 1 ? "s" : ""}`, "success");
      } else if (result.duplicates > 0) {
        showToast(`${result.duplicates} duplicate${result.duplicates !== 1 ? "s" : ""} skipped — no new videos added`, "info");
      }
    } catch (e) {
      console.error("Failed to add videos", e);
      useStore.getState().showToast("Failed to add videos", "error");
    }
  },

  removeVideo: async (id) => {
    try {
      await invoke("remove_video", { id });
      set((s) => ({
        config: { ...s.config, videos: s.config.videos.filter((v) => v.id !== id) },
        editingItem: s.editingItem?.id === id ? null : s.editingItem,
      }));
    } catch (e) {
      console.error("Failed to remove video", e);
    }
  },

  reorderVideos: async (ids) => {
    const ordered = ids
      .map((id) => get().config.videos.find((v) => v.id === id))
      .filter(Boolean) as VideoItem[];
    set((s) => ({ config: { ...s.config, videos: ordered } }));
    try {
      await invoke("reorder_videos", { ids });
    } catch (e) {
      console.error("Failed to reorder videos", e);
    }
  },

  updateVideoParams: async (id, params) => {
    set((s) => ({
      config: {
        ...s.config,
        videos: s.config.videos.map((v) => (v.id === id ? { ...v, local: params } : v)),
      },
      editingItem:
        s.editingItem?.id === id
          ? { ...s.editingItem, local: params }
          : s.editingItem,
    }));
    try {
      await invoke("update_video_params", { id, params });
    } catch (e) {
      console.error("Failed to update video params", e);
    }
  },

  updateGlobalSettings: async (settings) => {
    set((s) => ({ config: { ...s.config, global: settings } }));
    try {
      await invoke("update_global_settings", { settings });
    } catch (e) {
      console.error("Failed to update global settings", e);
    }
  },

  play: async () => {
    try {
      const selectedIds = get().selectedIds;
      await invoke("manual_play", { selectedIds });
      set({ isPlaying: true });
    } catch (e) {
      console.error("Failed to start playback", e);
      const { message } = await import("@tauri-apps/plugin-dialog");
      message(String(e), { title: "Playback Error", kind: "error" });
    }
  },

  stop: async () => {
    try {
      await invoke("manual_stop");
      set({ isPlaying: false });
    } catch (e) {
      console.error("Failed to stop playback", e);
    }
  },

  getThumbnail: async (videoPath) => {
    const cache = get().thumbnailCache;
    if (cache[videoPath]) return cache[videoPath];

    const b64 = await invoke<string | null>("get_thumbnail_base64", { path: videoPath });
    const src = b64 ?? "";
    set((s) => ({ thumbnailCache: { ...s.thumbnailCache, [videoPath]: src } }));
    return src;
  },

  toggleSelection: (id) => {
    set((s) => {
      const has = s.selectedIds.includes(id);
      return { selectedIds: has ? s.selectedIds.filter((x) => x !== id) : [...s.selectedIds, id] };
    });
  },

  selectAll: () => {
    set((s) => ({ selectedIds: s.config.videos.map((v) => v.id) }));
  },

  clearSelection: () => {
    set({ selectedIds: [] });
  },

  deleteSelected: async () => {
    const ids = get().selectedIds;
    if (ids.length === 0) return;
    for (const id of ids) {
      try {
        await invoke("remove_video", { id });
      } catch (e) {
        console.error("Failed to remove video", e);
      }
    }
    set((s) => ({
      config: { ...s.config, videos: s.config.videos.filter((v) => !ids.includes(v.id)) },
      selectedIds: [],
      editingItem: s.editingItem && ids.includes(s.editingItem.id) ? null : s.editingItem,
    }));
  },

  deleteAll: async () => {
    const ids = get().config.videos.map((v) => v.id);
    for (const id of ids) {
      try {
        await invoke("remove_video", { id });
      } catch (e) {
        console.error("Failed to remove video", e);
      }
    }
    set({ config: { ...get().config, videos: [] }, selectedIds: [], editingItem: null });
  },
}));
