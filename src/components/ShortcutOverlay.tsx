import { Keyboard, X } from "lucide-react";

type Shortcut = { keys: string[]; label: string };
type Group = { title: string; items: Shortcut[] };

const groups: Group[] = [
  {
    title: "导航 & 播放",
    items: [
      { keys: ["Space"], label: "播放 / 暂停" },
      { keys: ["J"], label: "下一段" },
      { keys: ["K"], label: "上一段" },
      { keys: ["Esc"], label: "清除字符选区 / 退出标注" },
      { keys: ["?"], label: "显示 / 隐藏本帮助" },
    ],
  },
  {
    title: "撤销 / 重做",
    items: [
      { keys: ["⌘", "Z"], label: "撤销当前段编辑" },
      { keys: ["⌘", "⇧", "Z"], label: "重做" },
      { keys: ["⌘", "Y"], label: "重做（备选）" },
    ],
  },
  {
    title: "成对包裹（标注页 + 字符选区）",
    items: [
      { keys: ["L"], label: "<laugh> 笑着说（再按移除）" },
      { keys: ["S"], label: "<strong> 重读（再按移除）" },
    ],
  },
  {
    title: "副语言事件标记 [xxx]（标注页，任意位置）",
    items: [
      { keys: ["G"], label: "[laugh] 笑声" },
      { keys: ["B"], label: "[breath] 呼吸" },
      { keys: ["C"], label: "[cough] 咳嗽" },
      { keys: ["X"], label: "[sigh] 叹气" },
      { keys: ["H"], label: "[hissing] 嘘声" },
      { keys: ["M"], label: "[lipsmack] 舔唇" },
      { keys: ["W"], label: "[swallowing] 吞口水" },
    ],
  },
  {
    title: "情感（标注页，1-7）",
    items: [
      { keys: ["1"], label: "中立" },
      { keys: ["2"], label: "开心" },
      { keys: ["3"], label: "愤怒" },
      { keys: ["4"], label: "悲伤" },
      { keys: ["5"], label: "惊讶" },
      { keys: ["6"], label: "恐惧" },
      { keys: ["7"], label: "厌恶" },
    ],
  },
  {
    title: "段级标签（无字符选区时；侧栏也可点）",
    items: [
      { keys: ["L"], label: "切换 笑声 段级标签" },
      { keys: ["B"], label: "切换 呼吸 段级标签" },
      { keys: ["C"], label: "切换 咳嗽 段级标签" },
      { keys: ["X"], label: "切换 叹气 段级标签" },
      { keys: ["H"], label: "切换 嘘声 段级标签" },
    ],
  },
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
      <div
        className="shortcut-card"
        onClick={(event) => event.stopPropagation()}
      >
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
        {groups.map((group) => (
          <div key={group.title} style={{ marginBottom: 16 }}>
            <h3
              style={{
                margin: "12px 0 8px",
                fontSize: 12,
                color: "var(--text-tertiary)",
                textTransform: "uppercase",
                letterSpacing: "0.06em",
              }}
            >
              {group.title}
            </h3>
            <div className="shortcut-list">
              {group.items.map((item) => (
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
        ))}
      </div>
    </div>
  );
}
