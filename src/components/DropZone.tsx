import { useEffect, useState, useCallback, useRef } from "react";
import { Film, FileUp } from "lucide-react";
import { useStore } from "../store/useStore";

const VIDEO_EXTENSIONS = [
  ".mp4", ".mkv", ".avi", ".mov", ".wmv", ".flv", ".webm", ".m4v",
  ".mpg", ".mpeg", ".ts", ".mts", ".m2ts", ".ogv", ".3gp",
];

function isVideo(path: string): boolean {
  const ext = path.toLowerCase().slice(path.lastIndexOf("."));
  return VIDEO_EXTENSIONS.includes(ext);
}

export default function DropZone() {
  const addVideos = useStore((s) => s.addVideos);
  const [isDragOver, setIsDragOver] = useState(false);
  const dragCounter = useRef(0);
  const processingDrop = useRef(false);
  const dialogOpen = useRef(false);

  useEffect(() => {
    let unlisten: (() => void) | undefined;

    (async () => {
      const { listen } = await import("@tauri-apps/api/event");

      const unlistenEnter = await listen<{ paths: string[]; position: { x: number; y: number } }>(
        "tauri://drag-enter",
        () => { dragCounter.current += 1; setIsDragOver(true); }
      );
      const unlistenLeave = await listen(
        "tauri://drag-leave",
        () => {
          dragCounter.current -= 1;
          if (dragCounter.current <= 0) { dragCounter.current = 0; setIsDragOver(false); }
        }
      );
      const unlistenOver = await listen(
        "tauri://drag-over",
        () => { setIsDragOver(true); }
      );
      const unlistenDrop = await listen<{ paths: string[]; position: { x: number; y: number } }>(
        "tauri://drag-drop",
        (event) => {
          if (processingDrop.current) return;
          processingDrop.current = true;
          setTimeout(() => { processingDrop.current = false; }, 300);

          dragCounter.current = 0;
          setIsDragOver(false);
          const paths = event.payload.paths.filter(isVideo);
          if (paths.length > 0) addVideos(paths);
        }
      );

      unlisten = () => {
        unlistenEnter();
        unlistenLeave();
        unlistenOver();
        unlistenDrop();
      };
    })();

    return () => unlisten?.();
  }, [addVideos]);

  const handleClick = useCallback(async () => {
    if (dialogOpen.current) return;
    dialogOpen.current = true;
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({
        multiple: true,
        filters: [{
          name: "Videos",
          extensions: VIDEO_EXTENSIONS.map(e => e.replace(".", "")),
        }],
      });
      if (selected) {
        const paths = Array.isArray(selected) ? selected : [selected];
        const videoPaths = paths.filter(isVideo);
        if (videoPaths.length > 0) addVideos(videoPaths);
      }
    } finally {
      dialogOpen.current = false;
    }
  }, [addVideos]);

  return (
    <button
      type="button"
      onClick={handleClick}
      className={`relative w-full border-2 border-dashed rounded-xl p-8 text-center transition-all duration-300 cursor-pointer ${
        isDragOver
          ? "border-indigo-400 bg-indigo-400/10 scale-[1.02]"
          : "border-neutral-700 hover:border-neutral-500 bg-neutral-900/50"
      }`}
    >
      <div className="flex flex-col items-center gap-2 pointer-events-none">
        {isDragOver ? (
          <Film className="w-10 h-10 text-indigo-400 animate-pulse" />
        ) : (
          <FileUp className="w-10 h-10 text-neutral-500" />
        )}
        <p className="text-sm text-neutral-400">
          {isDragOver
            ? "Drop videos here"
            : "Click to browse or drag & drop videos"}
        </p>
        <p className="text-xs text-neutral-600">
          mp4, mkv, avi, mov, webm, and more
        </p>
      </div>
    </button>
  );
}
