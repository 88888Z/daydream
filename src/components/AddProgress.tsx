import { useStore } from "../store/useStore";

export default function AddProgress() {
  const addProgress = useStore((s) => s.addProgress);
  if (!addProgress) return null;

  const pct = Math.round((addProgress.current / addProgress.total) * 100);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm">
      <div className="w-80 bg-neutral-900 border border-neutral-800 rounded-2xl shadow-2xl p-6">
        <p className="text-sm font-semibold text-neutral-200 mb-1">Adding videos</p>
        <p className="text-xs text-neutral-500 mb-4">
          {addProgress.current} / {addProgress.total}
        </p>
        <div className="w-full h-2 bg-neutral-800 rounded-full overflow-hidden">
          <div
            className="h-full bg-indigo-500 rounded-full transition-all duration-200 ease-out"
            style={{ width: `${pct}%` }}
          />
        </div>
        <p className="text-[10px] text-neutral-600 mt-2 text-right font-mono">{pct}%</p>
      </div>
    </div>
  );
}
