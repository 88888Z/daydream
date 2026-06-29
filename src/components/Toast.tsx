import { useEffect } from "react";
import { useStore } from "../store/useStore";
import { X, CheckCircle, Info, AlertTriangle } from "lucide-react";

function toastStyle(variant: string) {
  switch (variant) {
    case "success": return { bg: "bg-green-900/80", border: "border-green-700", icon: <CheckCircle className="w-4 h-4 text-green-400 shrink-0" /> };
    case "error": return { bg: "bg-red-900/80", border: "border-red-700", icon: <AlertTriangle className="w-4 h-4 text-red-400 shrink-0" /> };
    default: return { bg: "bg-blue-900/80", border: "border-blue-700", icon: <Info className="w-4 h-4 text-blue-400 shrink-0" /> };
  }
}

export default function Toast() {
  const toast = useStore((s) => s.toast);
  const clearToast = useStore((s) => s.clearToast);

  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(clearToast, 4000);
    return () => clearTimeout(timer);
  }, [toast, clearToast]);

  if (!toast) return null;

  const s = toastStyle(toast.variant);

  return (
    <div className="fixed bottom-6 left-1/2 -translate-x-1/2 z-[100] animate-in fade-in slide-in-from-bottom-2 duration-300">
      <div className={`flex items-center gap-2.5 px-4 py-2.5 rounded-xl border shadow-xl backdrop-blur-sm ${s.bg} ${s.border}`}>
        {s.icon}
        <p className="text-sm text-neutral-100">{toast.message}</p>
        <button onClick={clearToast} className="p-0.5 ml-1 rounded hover:bg-white/10 transition-colors">
          <X className="w-3.5 h-3.5 text-neutral-400" />
        </button>
      </div>
    </div>
  );
}
