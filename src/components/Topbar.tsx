import {
  Download,
  Keyboard,
  Moon,
  Package,
  Save,
  Settings,
  Sun,
  Sunrise,
} from "lucide-react";
import type { Theme } from "../types";
import { BUILD_CHANNEL, REVIEW_ONLY } from "../env";

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
  onExportBundle: () => void;
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
  onExportBundle,
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
          <h1>
            长沙方言标注工作台
            {REVIEW_ONLY && (
              <span className="build-channel-badge" title="无 Whisper / Ollama 集成；只用于审核已处理过的标注">
                {BUILD_CHANNEL}
              </span>
            )}
          </h1>
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
          onClick={onExport}
          disabled={!hasSegments || busy}
          title="只导出 JSONL（不复制音频文件）"
        >
          <Download size={14} />
          导出 JSONL
        </button>
        <button
          className="btn-primary"
          onClick={onExportBundle}
          disabled={!hasSegments || busy}
          title="复制全部切割文件 + 源音频 + JSONL 到一个目录，可直接整目录上传 OSS"
        >
          <Package size={14} />
          一键打包
        </button>
      </div>
    </header>
  );
}
