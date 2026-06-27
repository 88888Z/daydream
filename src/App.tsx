import { useEffect } from "react";
import { useStore } from "./store/useStore";
import LoopManager from "./components/LoopManager";

export default function App() {
  const loaded = useStore((s) => s.loaded);
  const loadConfig = useStore((s) => s.loadConfig);

  useEffect(() => {
    loadConfig();
  }, [loadConfig]);

  if (!loaded) {
    return (
      <div className="h-screen flex items-center justify-center bg-neutral-950">
        <div className="flex flex-col items-center gap-3">
          <div className="w-8 h-8 border-2 border-indigo-500 border-t-transparent rounded-full animate-spin" />
          <p className="text-xs text-neutral-500">Loading Daydream...</p>
        </div>
      </div>
    );
  }

  return <LoopManager />;
}
