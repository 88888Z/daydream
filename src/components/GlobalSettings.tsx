import { X, Repeat, Gauge, Volume2, Power, Clock } from "lucide-react";
import { useStore } from "../store/useStore";
import type { GlobalSettings } from "../types";

export default function GlobalSettingsPanel() {
  const settings = useStore((s) => s.config.global);
  const setShowGlobalSettings = useStore((s) => s.setShowGlobalSettings);
  const updateGlobalSettings = useStore((s) => s.updateGlobalSettings);

  const update = (partial: Partial<GlobalSettings>) => {
    updateGlobalSettings({ ...settings, ...partial });
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="w-full max-w-md mx-4 bg-neutral-900 border border-neutral-800 rounded-2xl shadow-2xl overflow-hidden">
        <div className="flex items-center justify-between p-4 border-b border-neutral-800">
          <h2 className="text-sm font-semibold">Settings</h2>
          <button
            onClick={() => setShowGlobalSettings(false)}
            className="p-1 rounded-lg hover:bg-neutral-800 transition-colors"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="p-4 space-y-5 max-h-[60vh] overflow-y-auto">
          <ToggleRow
            icon={<Power className="w-4 h-4" />}
            label="Start on Boot"
            description="Launch Daydream automatically when you log in"
            checked={settings.start_on_boot}
            onChange={async (v) => {
              update({ start_on_boot: v });
              try {
                const { enable, disable } = await import(
                  "@tauri-apps/plugin-autostart"
                );
                if (v) await enable();
                else await disable();
              } catch (e) {
                console.error("Autostart error", e);
              }
            }}
          />

          <SliderParam
            icon={<Clock className="w-4 h-4" />}
            label="Idle Timeout"
            value={settings.idle_timeout_seconds}
            min={10}
            max={600}
            step={10}
            display={`${Math.floor(settings.idle_timeout_seconds / 60)}m ${settings.idle_timeout_seconds % 60}s`}
            onChange={(v) => update({ idle_timeout_seconds: v })}
          />

          <div className="pt-2 border-t border-neutral-800">
            <p className="text-xs font-medium text-neutral-400 mb-4">Default Playback Parameters</p>
            <SliderParam
              icon={<Repeat className="w-4 h-4" />}
              label="Repeats"
              value={settings.default_params.repeats}
              min={1}
              max={20}
              step={1}
              display={`${settings.default_params.repeats}×`}
              onChange={(v) =>
                update({
                  default_params: { ...settings.default_params, repeats: v },
                })
              }
            />
            <SliderParam
              icon={<Gauge className="w-4 h-4" />}
              label="Speed"
              value={settings.default_params.speed}
              min={0.25}
              max={3}
              step={0.05}
              display={`${settings.default_params.speed.toFixed(2)}×`}
              onChange={(v) =>
                update({
                  default_params: { ...settings.default_params, speed: v },
                })
              }
            />
            <SliderParam
              icon={<Volume2 className="w-4 h-4" />}
              label="Volume"
              value={settings.default_params.volume}
              min={0}
              max={100}
              step={1}
              display={`${Math.round(settings.default_params.volume)}%`}
              onChange={(v) =>
                update({
                  default_params: { ...settings.default_params, volume: v },
                })
              }
            />
          </div>
        </div>

        <div className="p-4 border-t border-neutral-800">
          <button
            onClick={() => setShowGlobalSettings(false)}
            className="w-full text-xs py-2 bg-indigo-600 hover:bg-indigo-500 rounded-lg transition-colors"
          >
            Done
          </button>
        </div>
      </div>
    </div>
  );
}

function ToggleRow({
  icon,
  label,
  description,
  checked,
  onChange,
}: {
  icon: React.ReactNode;
  label: string;
  description: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <div className="flex items-start gap-3">
      <div className="mt-0.5 text-neutral-500">{icon}</div>
      <div className="flex-1 min-w-0">
        <label className="text-sm font-medium">{label}</label>
        <p className="text-xs text-neutral-500 mt-0.5">{description}</p>
      </div>
      <button
        onClick={() => onChange(!checked)}
        className={`relative w-9 h-5 rounded-full transition-colors flex-shrink-0 ${
          checked ? "bg-indigo-600" : "bg-neutral-700"
        }`}
      >
        <span
          className={`absolute top-0.5 left-0.5 w-4 h-4 rounded-full bg-white transition-transform ${
            checked ? "translate-x-4" : "translate-x-0"
          }`}
        />
      </button>
    </div>
  );
}

function SliderParam({
  icon,
  label,
  value,
  min,
  max,
  step,
  display,
  onChange,
}: {
  icon: React.ReactNode;
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  display: string;
  onChange: (v: number) => void;
}) {
  return (
    <div className="flex items-start gap-3">
      <div className="mt-0.5 text-neutral-500">{icon}</div>
      <div className="flex-1">
        <div className="flex items-center justify-between mb-1">
          <label className="text-xs font-medium text-neutral-300">{label}</label>
          <span className="text-xs font-mono text-neutral-500">{display}</span>
        </div>
        <input
          type="range"
          min={min}
          max={max}
          step={step}
          value={value}
          onChange={(e) => onChange(parseFloat(e.target.value))}
          className="w-full h-1.5 bg-neutral-800 rounded-full appearance-none cursor-pointer accent-indigo-500 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:w-4 [&::-webkit-slider-thumb]:h-4 [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-indigo-500"
        />
      </div>
    </div>
  );
}
