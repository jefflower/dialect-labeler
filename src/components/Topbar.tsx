import {
  Download,
  Keyboard,
  Moon,
  Save,
  Settings,
  Sun,
  Sunrise,
} from "lucide-react";
import type { Theme } from "../types";

type TopbarProps = {
  status: string;
  busy: boolean;
  hasError: boolean;
  hasProject: boolean;
  hasSegments: boolean;
  theme: Theme;
  onCycleTheme: () => void;
  onOpenSettings: () => void;
  onOpenShortcuts: () => void;
  onSave: () => void;
  onLoad: () => void;
  onExport: () => void;
};

const themeIcon = {
  light: Sun,
  dark: Moon,
  system: Sunrise,
};

export function Topbar({
  status,
  busy,
  hasError,
  hasProject,
  hasSegments,
  theme,
  onCycleTheme,
  onOpenSettings,
  onOpenShortcuts,
  onSave,
  onLoad,
  onExport,
}: TopbarProps) {
  const ThemeIcon = themeIcon[theme] ?? Sunrise;
  const dotClass = hasError ? "error" : busy ? "busy" : hasProject ? "" : "idle";
  return (
    <header className="topbar">
      <div className="topbar-brand">
        <div className="brand-mark" aria-hidden>
          湘
        </div>
        <div className="brand-text">
          <h1>长沙方言标注工作台</h1>
          <p className="status-line">
            <span className={`status-dot ${dotClass}`} />
            <span>{status}</span>
          </p>
        </div>
      </div>
      <div className="topbar-actions">
        <button
          className="btn-ghost btn-icon"
          onClick={onCycleTheme}
          title="切换主题（明/暗/系统）"
          aria-label="切换主题"
        >
          <ThemeIcon size={16} />
        </button>
        <button
          className="btn-ghost btn-icon"
          onClick={onOpenShortcuts}
          title="键盘快捷键 (?)"
          aria-label="键盘快捷键"
        >
          <Keyboard size={16} />
        </button>
        <button
          className="btn-ghost btn-icon"
          onClick={onOpenSettings}
          title="设置"
          aria-label="设置"
        >
          <Settings size={16} />
        </button>
        <button onClick={onLoad} disabled={!hasProject || busy}>
          <Save size={14} />
          打开
        </button>
        <button onClick={onSave} disabled={!hasProject || busy}>
          <Save size={14} />
          保存
        </button>
        <button
          className="btn-primary"
          onClick={onExport}
          disabled={!hasSegments || busy}
        >
          <Download size={14} />
          导出 JSONL
        </button>
      </div>
    </header>
  );
}
