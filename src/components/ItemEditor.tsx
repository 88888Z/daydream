import { X, RotateCcw } from "lucide-react";
import { useStore } from "../store/useStore";
import type { VideoParams } from "../types";

export default function ItemEditor() {
  const editingItem = useStore((s) => s.editingItem);
  const setEditingItem = useStore((s) => s.setEditingItem);
  const updateVideoParams = useStore((s) => s.updateVideoParams);
  const globalParams = useStore((s) => s.config.global.default_params);

  if (!editingItem) return null;

  const params = editingItem.local ?? globalParams;
  const hasOverrides = editingItem.local !== null;

  const update = (partial: Partial<VideoParams>) => {
    updateVideoParams(editingItem.id, {
      ...params,
      ...partial,
    });
  };

  const resetToGlobal = () => {
    updateVideoParams(editingItem.id, null);
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="w-full max-w-md mx-4 bg-neutral-900 border border-neutral-800 rounded-2xl shadow-2xl overflow-hidden animate-in fade-in zoom-in">
        <div className="flex items-center justify-between p-4 border-b border-neutral-800">
          <h2 className="text-sm font-semibold truncate">{editingItem.filename}</h2>
          <button
            onClick={() => setEditingItem(null)}
            className="p-1 rounded-lg hover:bg-neutral-800 transition-colors"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="p-4 space-y-5">
          <SliderParam
            label="Repeats"
            value={params.repeats}
            min={1}
            max={20}
            step={1}
            display={`${params.repeats}×`}
            onChange={(v) => update({ repeats: v })}
          />
          <SliderParam
            label="Speed"
            value={params.speed}
            min={0.25}
            max={3}
            step={0.05}
            display={`${params.speed.toFixed(2)}×`}
            onChange={(v) => update({ speed: v })}
          />
          <SliderParam
            label="Volume"
            value={params.volume}
            min={0}
            max={100}
            step={1}
            display={`${Math.round(params.volume)}%`}
            onChange={(v) => update({ volume: v })}
          />
        </div>

        <div className="flex items-center justify-between p-4 border-t border-neutral-800">
          <button
            onClick={resetToGlobal}
            className={`flex items-center gap-1.5 text-xs px-3 py-1.5 rounded-lg transition-all ${
              hasOverrides
                ? "text-indigo-400 hover:bg-indigo-500/10"
                : "text-neutral-600 cursor-not-allowed"
            }`}
            disabled={!hasOverrides}
          >
            <RotateCcw className="w-3 h-3" />
            Reset to global
          </button>
          <button
            onClick={() => setEditingItem(null)}
            className="text-xs px-4 py-1.5 bg-indigo-600 hover:bg-indigo-500 rounded-lg transition-colors"
          >
            Done
          </button>
        </div>
      </div>
    </div>
  );
}

function SliderParam({
  label,
  value,
  min,
  max,
  step,
  display,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  display: string;
  onChange: (v: number) => void;
}) {
  return (
    <div>
      <div className="flex items-center justify-between mb-1.5">
        <label className="text-xs font-medium text-neutral-400">{label}</label>
        <span className="text-xs font-mono text-neutral-300">{display}</span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(parseFloat(e.target.value))}
        className="w-full h-1.5 bg-neutral-800 rounded-full appearance-none cursor-pointer accent-indigo-500 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:w-4 [&::-webkit-slider-thumb]:h-4 [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-indigo-500 [&::-webkit-slider-thumb]:shadow-lg"
      />
    </div>
  );
}
