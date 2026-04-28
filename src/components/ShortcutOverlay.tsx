import { Keyboard, X } from "lucide-react";

type Shortcut = { keys: string[]; label: string };

const shortcuts: Shortcut[] = [
  { keys: ["Space"], label: "播放 / 暂停" },
  { keys: ["J"], label: "下一段" },
  { keys: ["K"], label: "上一段" },
  { keys: ["L"], label: "切换 笑声 标签" },
  { keys: ["B"], label: "切换 呼吸 标签" },
  { keys: ["P"], label: "切换 停顿 标签" },
  { keys: ["U"], label: "切换 听不清 标签" },
  { keys: ["N"], label: "切换 噪声 标签" },
  { keys: ["?"], label: "显示 / 隐藏本帮助" },
  { keys: ["Esc"], label: "关闭弹窗 / 退出标注" },
  { keys: ["⌘", "S"], label: "保存项目（输入框外）" },
];

type ShortcutOverlayProps = {
  open: boolean;
  onClose: () => void;
};

export function ShortcutOverlay({ open, onClose }: ShortcutOverlayProps) {
  if (!open) return null;
  return (
    <div
      className={`shortcut-overlay ${open ? "open" : ""}`}
      onClick={onClose}
    >
      <div className="shortcut-card" onClick={(event) => event.stopPropagation()}>
        <h2>
          <Keyboard size={20} />
          键盘快捷键
          <button
            className="btn-ghost btn-icon"
            style={{ marginLeft: "auto" }}
            onClick={onClose}
            aria-label="关闭"
          >
            <X size={16} />
          </button>
        </h2>
        <div className="shortcut-list">
          {shortcuts.map((item) => (
            <div className="shortcut-item" key={item.label}>
              <span>{item.label}</span>
              <span style={{ display: "flex", gap: 4 }}>
                {item.keys.map((key) => (
                  <kbd key={key}>{key}</kbd>
                ))}
              </span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
