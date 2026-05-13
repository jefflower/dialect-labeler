import { Cpu, X } from "lucide-react";
import type { AppSettings } from "../types";

type SettingsDrawerProps = {
  open: boolean;
  settings: AppSettings;
  onChange: (patch: Partial<AppSettings>) => void;
  onClose: () => void;
};

/**
 * App-global settings drawer.
 *
 * Trimmed down from the v0.2 version where it also owned every Mode-1
 * processing knob (Whisper, Ollama, dialect prompts, OSS prefix, tag
 * dictionaries, dependency health). Per design feedback the drawer
 * was misleading: those settings are all mode-scoped — Mode 2 exports
 * xlsx without tags/emotions/system-prompt, and uses a different
 * recognize / LLM pipeline. Everything mode-scoped now lives in the
 * matching Mode panel inside ConfigBand. The drawer keeps only the
 * truly app-global preference: theme.
 *
 * Leaving the drawer (vs deleting it entirely) so we have a natural
 * home for future global preferences (e.g. account info, keyboard
 * layout, telemetry toggle).
 */
export function SettingsDrawer({
  open,
  settings,
  onChange,
  onClose,
}: SettingsDrawerProps) {
  return (
    <>
      <div
        className={`drawer-overlay ${open ? "open" : ""}`}
        onClick={onClose}
      />
      <aside className={`drawer ${open ? "open" : ""}`} aria-hidden={!open}>
        <header className="drawer-head">
          <h2>设置</h2>
          <button
            className="btn-ghost btn-icon"
            onClick={onClose}
            aria-label="关闭设置"
          >
            <X size={16} />
          </button>
        </header>
        <div className="drawer-body">
          <section className="drawer-section">
            <h3>
              <Cpu size={14} />
              主题
            </h3>
            <div className="drawer-row">
              <label>外观</label>
              <select
                value={settings.theme}
                onChange={(event) =>
                  onChange({ theme: event.target.value as AppSettings["theme"] })
                }
              >
                <option value="system">跟随系统</option>
                <option value="light">浅色</option>
                <option value="dark">深色</option>
              </select>
            </div>
          </section>
          <p
            style={{
              fontSize: 12,
              color: "var(--text-tertiary, #94a3b8)",
              padding: "12px 20px 0",
              lineHeight: 1.6,
            }}
          >
            其他设置已按模式归类，请到主界面的「<strong>切割策略</strong>」折叠里查找：
            <br />· Mode 1：Whisper / Ollama / 方言预设 / 标签字典 / 导出配置 / 依赖体检
            <br />· Mode 2：语义参数 / 端点池
          </p>
        </div>
        <footer className="drawer-foot">
          <span style={{ fontSize: 12, color: "var(--text-tertiary)" }}>
            设置自动保存到本地浏览器存储
          </span>
          <button className="btn-primary" onClick={onClose}>
            完成
          </button>
        </footer>
      </aside>
    </>
  );
}
