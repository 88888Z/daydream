import { useCallback, useEffect, useRef, useState } from "react";
import {
  DndContext,
  closestCenter,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
  type DragEndEvent,
} from "@dnd-kit/core";
import {
  SortableContext,
  sortableKeyboardCoordinates,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { ListMusic, Settings, Play, Square, Power, Trash2, CheckSquare } from "lucide-react";
import { useStore } from "../store/useStore";
import VideoItem from "./VideoItem";
import DropZone from "./DropZone";
import ItemEditor from "./ItemEditor";
import GlobalSettingsPanel from "./GlobalSettings";
import AddProgress from "./AddProgress";

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke: tauriInvoke } = await import("@tauri-apps/api/core");
  return tauriInvoke<T>(cmd, args);
}

export default function LoopManager() {
  const videos = useStore((s) => s.config.videos);
  const reorderVideos = useStore((s) => s.reorderVideos);
  const setShowGlobalSettings = useStore((s) => s.setShowGlobalSettings);
  const showGlobalSettings = useStore((s) => s.showGlobalSettings);
  const isPlaying = useStore((s) => s.isPlaying);
  const nowPlaying = useStore((s) => s.nowPlaying);
  const play = useStore((s) => s.play);
  const stop = useStore((s) => s.stop);
  const config = useStore((s) => s.config);
  const idleRemaining = useStore((s) => s.idleRemaining);
  const updateGlobalSettings = useStore((s) => s.updateGlobalSettings);
  const selectedIds = useStore((s) => s.selectedIds);
  const selectAll = useStore((s) => s.selectAll);
  const clearSelection = useStore((s) => s.clearSelection);
  const deleteSelected = useStore((s) => s.deleteSelected);
  const deleteAll = useStore((s) => s.deleteAll);

  const idleEnabled = config.global.idle_enabled;
  const scrollRef = useRef<HTMLDivElement>(null);
  const [rubber, setRubber] = useState<{ x1: number; y1: number; x2: number; y2: number } | null>(null);

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    const target = e.target as HTMLElement;
    if (target.closest("button") || target.closest("input")) return;
    if (!e.shiftKey) clearSelection();
    setRubber({ x1: e.clientX, y1: e.clientY, x2: e.clientX, y2: e.clientY });
  }, [clearSelection]);

  const handleMouseMove = useCallback((e: React.MouseEvent) => {
    if (!rubber) return;
    setRubber((r) => r ? { ...r, x2: e.clientX, y2: e.clientY } : null);
  }, [rubber]);

  const handleMouseUp = useCallback(() => {
    if (!rubber) return;
    const xMin = Math.min(rubber.x1, rubber.x2);
    const xMax = Math.max(rubber.x1, rubber.x2);
    const yMin = Math.min(rubber.y1, rubber.y2);
    const yMax = Math.max(rubber.y1, rubber.y2);
    const w = xMax - xMin;
    const h = yMax - yMin;

    const toggleSelection = useStore.getState().toggleSelection;

    if (w < 5 && h < 5) {
      const el = document.elementFromPoint(rubber.x1, rubber.y1);
      const itemEl = el?.closest("[data-video-id]");
      if (itemEl) {
        const id = itemEl.getAttribute("data-video-id");
        if (id) toggleSelection(id);
      }
      setRubber(null);
      return;
    }

    const items = document.querySelectorAll<HTMLElement>("[data-video-id]");
    items.forEach((el) => {
      const rect = el.getBoundingClientRect();
      const overlap = rect.left < xMax && rect.right > xMin && rect.top < yMax && rect.bottom > yMin;
      if (overlap) {
        const id = el.getAttribute("data-video-id");
        if (id) toggleSelection(id);
      }
    });
    setRubber(null);
  }, [rubber]);

  useEffect(() => {
    if (!rubber) return;
    const onUp = () => handleMouseUp();
    window.addEventListener("mouseup", onUp);
    return () => window.removeEventListener("mouseup", onUp);
  }, [rubber, handleMouseUp]);

  const toggleIdle = useCallback(async () => {
    const newEnabled = !idleEnabled;
    await updateGlobalSettings({ ...config.global, idle_enabled: newEnabled });

    if (newEnabled) {
      await invoke("start_idle_monitor");
    } else {
      await invoke("stop_idle_monitor");
      if (isPlaying) stop();
    }
  }, [config.global, idleEnabled, updateGlobalSettings, isPlaying, stop]);

  useEffect(() => {
    if (idleEnabled) {
      invoke("start_idle_monitor").catch(() => {});
    }
  }, []);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
      if (e.code === "Space") {
        e.preventDefault();
        if (isPlaying) stop();
        else if (videos.length > 0) play();
      }
      if (e.code === "Escape") {
        clearSelection();
      }
      if (e.code === "Delete" || e.code === "Backspace" || e.code === "NumpadDecimal") {
        if (selectedIds.length > 0) deleteSelected();
      }
      if (e.ctrlKey && e.code === "KeyA") {
        e.preventDefault();
        selectAll();
      }
      if (e.shiftKey && (e.code === "Delete" || e.code === "NumpadDecimal")) {
        e.preventDefault();
        if (selectedIds.length > 0) deleteSelected();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [isPlaying, videos.length, play, stop, selectedIds, deleteSelected, clearSelection, selectAll]);

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 8 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const handleDragEnd = useCallback(
    (event: DragEndEvent) => {
      const { active, over } = event;
      if (!over || active.id === over.id) return;

      const ids = videos.map((v) => v.id);
      const oldIndex = ids.indexOf(active.id as string);
      const newIndex = ids.indexOf(over.id as string);

      if (oldIndex !== -1 && newIndex !== -1) {
        const newIds = [...ids];
        newIds.splice(oldIndex, 1);
        newIds.splice(newIndex, 0, active.id as string);
        reorderVideos(newIds);
      }
    },
    [videos, reorderVideos],
  );

  const rubberStyle = rubber ? {
    left: Math.min(rubber.x1, rubber.x2),
    top: Math.min(rubber.y1, rubber.y2),
    width: Math.abs(rubber.x2 - rubber.x1),
    height: Math.abs(rubber.y2 - rubber.y1),
  } : null;

  return (
    <div className="h-screen flex flex-col">
      <header className="flex items-center justify-between px-4 py-3 border-b border-neutral-800">
        <div className="flex items-center gap-2">
          <ListMusic className="w-5 h-5 text-indigo-400" />
          <h1 className="text-sm font-semibold">Daydream</h1>
          <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-neutral-800 text-neutral-500">
            {videos.length} video{videos.length !== 1 ? "s" : ""}
          </span>
        </div>
        <div className="flex items-center gap-2">
          {selectedIds.length > 0 && (
            <>
              <button
                onClick={clearSelection}
                className="text-[10px] px-2 py-1 rounded bg-neutral-800 hover:bg-neutral-700 transition-colors"
              >
                Clear ({selectedIds.length})
              </button>
              <button
                onClick={deleteSelected}
                className="flex items-center gap-1 text-[10px] px-2 py-1 rounded bg-red-600/20 text-red-400 hover:bg-red-600/30 transition-colors"
              >
                <Trash2 className="w-3 h-3" />
                Delete
              </button>
            </>
          )}
          {videos.length > 0 && (
            <button
              onClick={selectAll}
              className="flex items-center gap-1 text-[10px] px-2 py-1 rounded text-neutral-500 hover:text-neutral-300 hover:bg-neutral-800 transition-all"
            >
              <CheckSquare className="w-3 h-3" />
              Select
            </button>
          )}
          {videos.length > 0 && selectedIds.length === 0 && (
            <button
              onClick={deleteAll}
              className="flex items-center gap-1 text-[10px] px-2 py-1 rounded text-neutral-500 hover:text-red-400 hover:bg-red-500/10 transition-all"
            >
              <Trash2 className="w-3 h-3" />
              All
            </button>
          )}
          <button
            onClick={() => setShowGlobalSettings(true)}
            className="flex items-center gap-1.5 text-xs px-3 py-1.5 rounded-lg bg-neutral-800 hover:bg-neutral-700 transition-colors"
          >
            <Settings className="w-3.5 h-3.5" />
            Settings
          </button>
        </div>
      </header>

      <div
        ref={scrollRef}
        className="flex-1 overflow-y-auto p-4 space-y-3 relative select-none"
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
      >
        <DropZone />

        {videos.length > 0 && (
          <div className="space-y-1">
            {rubberStyle && (
              <div
                className="fixed z-50 bg-blue-500/15 border border-blue-500/40 rounded pointer-events-none"
                style={rubberStyle}
              />
            )}
            <DndContext
              sensors={sensors}
              collisionDetection={closestCenter}
              onDragEnd={handleDragEnd}
            >
              <SortableContext
                items={videos.map((v) => v.id)}
                strategy={verticalListSortingStrategy}
              >
                {videos.map((item, index) => (
                  <VideoItem key={item.id} item={item} index={index} />
                ))}
              </SortableContext>
            </DndContext>
          </div>
        )}

        {videos.length === 0 && (
          <div className="text-center py-12">
            <p className="text-sm text-neutral-500">No videos in your loop yet</p>
            <p className="text-xs text-neutral-600 mt-1">
              Drop some videos above to get started
            </p>
          </div>
        )}
      </div>

      <footer className="px-4 py-3 border-t border-neutral-800 flex items-center gap-3">
        {isPlaying ? (
          <button
            onClick={stop}
            className="flex items-center gap-2 px-4 py-2 bg-red-600 hover:bg-red-500 rounded-lg text-xs font-medium transition-colors"
          >
            <Square className="w-3.5 h-3.5" />
            Stop
          </button>
        ) : (
          <button
            onClick={play}
            disabled={videos.length === 0}
            className="flex items-center gap-2 px-4 py-2 bg-indigo-600 hover:bg-indigo-500 disabled:opacity-40 disabled:cursor-not-allowed rounded-lg text-xs font-medium transition-colors"
          >
            <Play className="w-3.5 h-3.5" />
            {videos.length === 0 ? "No videos" : "Play Loop"}
          </button>
        )}

        {isPlaying && nowPlaying && (
          <span className="text-[10px] text-neutral-500 font-mono">
            {nowPlaying.itemIndex + 1} / {videos.length}
          </span>
        )}

        <div className="flex-1" />

        <button
          onClick={toggleIdle}
          className={`flex items-center gap-2 text-xs px-3 py-1.5 rounded-lg transition-all ${
            idleEnabled
              ? idleRemaining <= 10
                ? "bg-orange-500/20 text-orange-400 animate-pulse"
                : "bg-indigo-600/20 text-indigo-400"
              : "bg-neutral-800 text-neutral-500 hover:text-neutral-300"
          }`}
        >
          <Power className={`w-3 h-3 ${idleEnabled ? "text-indigo-400" : ""}`} />
          <span>
            {idleEnabled
              ? idleRemaining === 0
                ? "Auto-play"
                : `${idleRemaining}s`
              : "Idle"}
          </span>
        </button>

        <div
          className={`w-2 h-2 rounded-full transition-colors ${
            isPlaying
              ? "bg-green-500 animate-pulse"
              : idleEnabled
                ? "bg-indigo-500"
                : "bg-neutral-700"
          }`}
        />
      </footer>

      <ItemEditor />
      {showGlobalSettings && <GlobalSettingsPanel />}
      <AddProgress />
    </div>
  );
}
