import {
  Cloud,
  Download,
  Keyboard,
  Languages,
  Moon,
  Package,
  Save,
  Scissors,
  Settings,
  Sun,
  Sunrise,
  XCircle,
} from "lucide-react";
import type { CutMode, Theme } from "../types";
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
  onOpenCloud: () => void;
  onSave: () => void;
  onLoad: () => void;
  onExport: () => void;
  onExportBundle: () => void;
  /** Clear all project-scoped state so the user can pick a fresh
   *  input/output pair. Doesn't touch the disk. */
  onCloseProject: () => void;
  /** Work-mode tabs live in the topbar header now (used to be a
   *  separate band below). When `null` the tab block is hidden — that's
   *  the case for review-only builds that don't run the cutter. Locked
   *  flag disables the tabs while a project is already loaded so the
   *  user can't change mode mid-stream. */
  mode?: CutMode;
  onModeChange?: (next: CutMode) => void;
  modeLocked?: boolean;
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
  onOpenCloud,
  onSave,
  onLoad,
  onExport,
  onExportBundle,
  onCloseProject,
  mode,
  onModeChange,
  modeLocked,
}: TopbarProps) {
  const ThemeIcon = themeIcon[theme] ?? Sunrise;
  const dotClass = hasError ? "error" : busy ? "busy" : hasProject ? "" : "idle";
  // Show mode tabs only when the parent wired them in. Review-only
  // builds skip the cutter entirely so we hide the picker.
  const showModeTabs = mode != null && onModeChange != null;

  const tabStyle = (selected: boolean): React.CSSProperties => ({
    background: selected ? "rgba(13, 148, 136, 0.12)" : "transparent",
    border: "none",
    padding: "6px 14px",
    display: "inline-flex",
    alignItems: "center",
    gap: 6,
    cursor: modeLocked ? "not-allowed" : "pointer",
    color: selected
      ? "var(--text-primary, #0f172a)"
      : "var(--text-secondary, #64748b)",
    fontSize: 13,
    fontWeight: selected ? 600 : 400,
    borderRadius: 6,
    opacity: modeLocked && !selected ? 0.5 : 1,
    whiteSpace: "nowrap",
  });

  return (
    <header className="topbar">
      <div className="topbar-brand">
        <div className="brand-text">
          <h1>
            方言标注工作台
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
        {/* Work-mode tabs sit right next to the title — inlined here
            (used to be a dedicated row below the topbar) so the picker
            is always the first decision the user sees and no vertical
            space is wasted on it. Locked once a project is loaded so
            the choice stays committed for the active session. */}
        {showModeTabs && (
          <div
            role="tablist"
            aria-label="工作模式"
            style={{
              display: "flex",
              alignItems: "center",
              gap: 4,
              marginLeft: 18,
              paddingLeft: 18,
              borderLeft: "1px solid var(--border, #e2e8f0)",
            }}
            title={
              modeLocked
                ? "已加载项目，模式锁定。关闭项目后可重选。"
                : "导入文件夹前先选好模式"
            }
          >
            <button
              role="tab"
              aria-selected={mode === "dialect"}
              disabled={modeLocked}
              onClick={() => !modeLocked && onModeChange?.("dialect")}
              style={tabStyle(mode === "dialect")}
            >
              <Scissors size={13} />
              模式 1 · 方言
            </button>
            <button
              role="tab"
              aria-selected={mode === "semantic"}
              disabled={modeLocked}
              onClick={() => !modeLocked && onModeChange?.("semantic")}
              style={tabStyle(mode === "semantic")}
            >
              <Languages size={13} />
              模式 2 · 语义
            </button>
          </div>
        )}
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
        {!REVIEW_ONLY && (
          <button
            className="btn-ghost btn-icon"
            onClick={onOpenCloud}
            title="云端 Worker — 登录调度，自动接单"
            aria-label="云端 Worker"
          >
            <Cloud size={16} />
          </button>
        )}
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
          onClick={onCloseProject}
          disabled={!hasProject || busy}
          title="清空当前界面状态，便于切换不同的输入/输出目录配对（不删磁盘文件）"
        >
          <XCircle size={14} />
          关闭项目
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
