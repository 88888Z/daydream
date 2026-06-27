import { useCallback, useEffect } from "react";
import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { GripVertical, Pencil, Trash2, Film, ChevronUp, ChevronDown } from "lucide-react";
import type { VideoItem as VideoItemType } from "../types";
import { useStore } from "../store/useStore";

interface Props {
  item: VideoItemType;
  index: number;
}

export default function VideoItem({ item, index }: Props) {
  const removeVideo = useStore((s) => s.removeVideo);
  const setEditingItem = useStore((s) => s.setEditingItem);
  const globalParams = useStore((s) => s.config.global.default_params);
  const nowPlaying = useStore((s) => s.nowPlaying);
  const videos = useStore((s) => s.config.videos);
  const reorderVideos = useStore((s) => s.reorderVideos);
  const isActive = nowPlaying?.itemIndex === index && nowPlaying !== null;
  const thumbSrc = useStore((s) => s.thumbnailCache[item.path] ?? null);
  const getThumbnail = useStore((s) => s.getThumbnail);
  const selectedIds = useStore((s) => s.selectedIds);
  const isSelected = selectedIds.includes(item.id);

  useEffect(() => {
    if (!useStore.getState().thumbnailCache[item.path]) {
      getThumbnail(item.path);
    }
  }, [item.path, getThumbnail]);

  const moveUp = useCallback(() => {
    if (index === 0) return;
    const ids = videos.map((v) => v.id);
    const tmp = ids[index - 1]!;
    ids[index - 1] = ids[index]!;
    ids[index] = tmp;
    reorderVideos(ids);
  }, [index, videos, reorderVideos]);

  const moveDown = useCallback(() => {
    if (index === videos.length - 1) return;
    const ids = videos.map((v) => v.id);
    const tmp = ids[index]!;
    ids[index] = ids[index + 1]!;
    ids[index + 1] = tmp;
    reorderVideos(ids);
  }, [index, videos, reorderVideos]);

  const {
    attributes,
    listeners,
    setNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id: item.id });

  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
  };

  const params = item.local ?? globalParams;
  const hasOverrides = item.local !== null;

  return (
    <div
      ref={setNodeRef}
      style={style}
      data-video-id={item.id}
      className={`group flex items-center gap-3 p-3 rounded-lg border transition-all duration-200 cursor-pointer ${
        isDragging
          ? "border-indigo-500/50 bg-indigo-500/10 shadow-lg shadow-indigo-500/10"
          : isActive
            ? "border-green-500/50 bg-green-500/10 shadow-sm shadow-green-500/10"
            : isSelected
              ? "border-blue-500/60 bg-blue-500/10 ring-1 ring-blue-500/40"
              : "border-neutral-800 bg-neutral-900 hover:border-neutral-700"
      }`}
    >
      <button
        {...attributes}
        {...listeners}
        className="cursor-grab active:cursor-grabbing text-neutral-600 hover:text-neutral-300 transition-colors"
        aria-label="Drag to reorder"
      >
        <GripVertical className="w-4 h-4" />
      </button>

      <div className="flex flex-col gap-0.5">
        <button
          onClick={moveUp}
          disabled={index === 0}
          className="p-0.5 rounded text-neutral-600 hover:text-neutral-200 hover:bg-neutral-800 opacity-0 group-hover:opacity-100 transition-all disabled:opacity-0 disabled:pointer-events-none"
          aria-label="Move up"
        >
          <ChevronUp className="w-3 h-3" />
        </button>
        <button
          onClick={moveDown}
          disabled={index === videos.length - 1}
          className="p-0.5 rounded text-neutral-600 hover:text-neutral-200 hover:bg-neutral-800 opacity-0 group-hover:opacity-100 transition-all disabled:opacity-0 disabled:pointer-events-none"
          aria-label="Move down"
        >
          <ChevronDown className="w-3 h-3" />
        </button>
      </div>

      <div className={`w-16 h-10 rounded-md flex-shrink-0 overflow-hidden ${
        isActive ? "ring-2 ring-green-500" : ""
      }`}>
        {thumbSrc ? (
          <img
            src={thumbSrc}
            alt=""
            className="w-full h-full object-cover"
            loading="lazy"
          />
        ) : (
          <div className="w-full h-full bg-neutral-800 flex items-center justify-center">
            {isActive ? (
              <div className="w-3 h-3 rounded-full bg-green-500 animate-pulse" />
            ) : (
              <Film className="w-4 h-4 text-neutral-500" />
            )}
          </div>
        )}
      </div>

      <div className="flex-1 min-w-0">
        <p className="text-sm font-medium truncate">{item.filename}</p>
        <div className="flex items-center gap-2 mt-0.5">
          <span className="text-xs text-neutral-500">
            {params.repeats > 1 ? `${params.repeats}x` : "1x"}
          </span>
          <span className="text-xs text-neutral-600">·</span>
          <span className="text-xs text-neutral-500">{params.speed.toFixed(2)}×</span>
          <span className="text-xs text-neutral-600">·</span>
          <span className="text-xs text-neutral-500">{Math.round(params.volume)}%</span>
          {hasOverrides && (
            <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-indigo-500/20 text-indigo-400 font-medium">
              custom
            </span>
          )}
        </div>
      </div>

      <button
        onClick={() => setEditingItem(item)}
        className="p-1.5 rounded-md text-neutral-600 hover:text-neutral-200 hover:bg-neutral-800 opacity-0 group-hover:opacity-100 transition-all"
        aria-label="Edit parameters"
      >
        <Pencil className="w-3.5 h-3.5" />
      </button>

      <button
        onClick={() => removeVideo(item.id)}
        className="p-1.5 rounded-md text-neutral-600 hover:text-red-400 hover:bg-red-500/10 opacity-0 group-hover:opacity-100 transition-all"
        aria-label="Remove video"
      >
        <Trash2 className="w-3.5 h-3.5" />
      </button>
    </div>
  );
}
