import { useRef } from "react";
import {
  ArrowLeft,
  Download,
  Pause,
  Play,
  Save,
  Sparkles,
  StickyNote,
  Tag as TagIcon,
  Wand2,
} from "lucide-react";
import type {
  InlineTagDef,
  ProgressState,
  SegmentRecord,
  SegmentTagDef,
} from "../types";
import { roleLabels } from "../defaults";
import { REVIEW_ONLY } from "../env";
import { formatClock, formatMsRange } from "../lib";
import { AlignedTimeline } from "./AlignedTimeline";
import type { SelectionRange } from "./AlignedTimeline";
import { ProgressPanel } from "./ProgressPanel";

type AnnotationViewProps = {
  segment: SegmentRecord;
  playbackPath: string;
  playbackStatus: string;
  durationMs: number;
  currentMs: number;
  isPlaying: boolean;
  busy: boolean;
  llmEnabled: boolean;
  progress: ProgressState;
  selection: SelectionRange | null;
  inlineTags: InlineTagDef[];
  segmentTags: SegmentTagDef[];
  emotions: string[];
  onSelectionChange: (range: SelectionRange | null) => void;
  onApplyInlineTag: (tag: string, kind?: "paired" | "bracket") => boolean;
  onClose: () => void;
  onTogglePlay: () => void;
  onSeekRatio: (ratio: number) => void;
  onUpdate: (patch: Partial<SegmentRecord>) => void;
  onRecognizeOne: () => void;
  onPolishOnly: () => void;
  onSave: () => void;
  onExport: () => void;
  onPrev: () => void;
  onNext: () => void;
};

export function AnnotationView({
  segment,
  playbackPath,
  playbackStatus,
  durationMs,
  currentMs,
  isPlaying,
  busy,
  llmEnabled,
  progress,
  selection,
  inlineTags,
  segmentTags,
  emotions,
  onSelectionChange,
  onApplyInlineTag,
  onClose,
  onTogglePlay,
  onSeekRatio,
  onUpdate,
  onRecognizeOne,
  onPolishOnly,
  onSave,
  onExport,
  onPrev,
  onNext,
}: AnnotationViewProps) {
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);

  function toggleEmotion(emotion: string) {
    if (segment.emotion.includes(emotion)) {
      onUpdate({ emotion: segment.emotion.filter((e) => e !== emotion) });
    } else {
      onUpdate({ emotion: [...segment.emotion, emotion] });
    }
  }

  function toggleSegmentTag(tag: string) {
    if (segment.tags.includes(tag)) {
      onUpdate({ tags: segment.tags.filter((t) => t !== tag) });
    } else {
      onUpdate({ tags: [...segment.tags, tag] });
    }
  }

  function updateEmotionRaw(value: string) {
    const emotion = value
      .split(/[,\s，、]+/)
      .map((item) => item.trim())
      .filter(Boolean);
    onUpdate({ emotion });
  }

  /**
   * Wrap selection (or insert at cursor) in the raw textarea — fallback path
   * for users who'd rather edit raw text than use the visual timeline.
   * Operates on the textarea's current selectionStart/End.
   */
  function wrapTextareaSelection(tag: string, kind: "paired" | "bracket") {
    const editor = textareaRef.current;
    const value = segment.phoneticText;
    if (kind === "bracket") {
      const pos = editor ? editor.selectionStart : value.length;
      const next = `${value.slice(0, pos)}[${tag}]${value.slice(pos)}`;
      onUpdate({ phoneticText: next });
      return;
    }
    if (!editor) return;
    const start = editor.selectionStart;
    const end = editor.selectionEnd;
    if (start === end) return;
    const inner = value.slice(start, end);
    const next = `${value.slice(0, start)}<${tag}>${inner}</${tag}>${value.slice(end)}`;
    onUpdate({ phoneticText: next });
    requestAnimationFrame(() => {
      editor.focus();
      const caret = start + tag.length + 2;
      editor.setSelectionRange(caret, caret + inner.length);
    });
  }

  const selectionLength = selection
    ? selection.endFlat - selection.startFlat + 1
    : 0;

  return (
    <main className="annotation-shell">
      <header className="annotation-topbar">
        <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
          <button className="btn-ghost" onClick={onClose}>
            <ArrowLeft size={14} />
            返回
          </button>
          <div className="info">
            <h1>{segment.segmentFileName}</h1>
            <p>
              <span className={`badge-role ${segment.role ?? "unknown"}`}>
                {roleLabels[segment.role ?? "unknown"]}
              </span>
              <span style={{ marginLeft: 8 }}>
                {playbackStatus || formatMsRange(segment.startMs, segment.endMs)}
              </span>
            </p>
          </div>
        </div>
        <div className="topbar-actions">
          <button onClick={onPrev} disabled={busy} title="上一段（K）">
            ← 上一段
          </button>
          <button onClick={onNext} disabled={busy} title="下一段（J）">
            下一段 →
          </button>
          {!REVIEW_ONLY && (
            <>
              <button
                className="btn-soft"
                onClick={onRecognizeOne}
                disabled={busy}
                title={llmEnabled ? "Whisper 识别 → Ollama 改写本段" : "Whisper 识别本段"}
              >
                <Sparkles size={14} />
                {llmEnabled ? "识别+改写本段" : "Whisper 识别本段"}
              </button>
              <button
                onClick={onPolishOnly}
                disabled={busy || !llmEnabled || !segment.phoneticText.trim()}
                title="只调用 Ollama 对当前文本再润色一次"
              >
                <Wand2 size={14} />
                再次润色
              </button>
            </>
          )}
          <button onClick={onSave} disabled={busy}>
            <Save size={14} />
            保存
          </button>
          <button className="btn-primary" onClick={onExport} disabled={busy}>
            <Download size={14} />
            导出
          </button>
        </div>
      </header>

      <section className="annotation-body">
        <section className="annotation-player">
          <button
            className="transport-button"
            onClick={onTogglePlay}
            disabled={!playbackPath}
            aria-label={isPlaying ? "暂停" : "播放"}
            title="播放 / 暂停（空格）"
          >
            {isPlaying ? <Pause size={18} /> : <Play size={18} />}
          </button>
          <div className="annotation-player-meta">
            <strong>对齐编辑器</strong>
            <span>
              点字跳转 · 拖选后 L 笑着说 / S 重读 · 任何时候 G 笑声 / B 呼吸 / C 咳嗽 / X 叹气 · 1-7 情绪
            </span>
          </div>
          <span className="player-bar-time">
            {formatClock(currentMs)} / {formatClock(durationMs)}
          </span>
        </section>

        <section className="aligned-section">
          <AlignedTimeline
            audioPath={playbackPath}
            durationMs={durationMs}
            currentMs={currentMs}
            text={segment.phoneticText}
            onSeek={onSeekRatio}
            onSelectionChange={onSelectionChange}
          />
        </section>

        {progress.visible && <ProgressPanel progress={progress} />}

        <section className="annotation-editor">
          <div className="editor-column">
            <div className="field">
              <span className="field-label">
                <span>记音字（最终输出）</span>
                <span style={{ color: "var(--text-tertiary)" }}>
                  {selection
                    ? `已选 ${selectionLength} 字${
                        selection.commonTags.length
                          ? ` · 在 ${selection.commonTags.join("/")} 中（再按对应键可移除）`
                          : ""
                      }`
                    : "上方拖选字 → 工具栏一键标注 · 文字按时间均匀分布"}
                </span>
              </span>
              <div className="editor-toolbar">
                <span className="group-label">情绪</span>
                {emotions.map((emotion, i) => (
                  <button
                    key={emotion}
                    className={`tag-chip ${segment.emotion.includes(emotion) ? "active" : ""}`}
                    onClick={() => toggleEmotion(emotion)}
                    title={`快捷键 ${i + 1}`}
                  >
                    {emotion}
                    <kbd style={{ marginLeft: 6 }}>{i + 1}</kbd>
                  </button>
                ))}
              </div>
              <div className="editor-toolbar">
                <span className="group-label">成对包裹</span>
                {inlineTags
                  .filter((item) => item.kind === "paired")
                  .map((item) => {
                    const isActive =
                      !!selection?.commonTags.includes(item.tag);
                    const enabled = !!selection;
                    return (
                      <button
                        key={`pair-${item.tag}`}
                        className={`tag-chip ${isActive ? "active" : ""} ${enabled ? "primary" : ""}`}
                        disabled={!enabled}
                        onClick={() => onApplyInlineTag(item.tag, "paired")}
                        title={`${item.hint}（${item.key}）`}
                      >
                        {isActive ? "− " : "+ "}
                        {item.label}
                        <kbd style={{ marginLeft: 6 }}>{item.key}</kbd>
                      </button>
                    );
                  })}
              </div>
              <div className="editor-toolbar">
                <span className="group-label">副语言事件</span>
                {inlineTags
                  .filter((item) => item.kind === "bracket")
                  .map((item) => (
                    <button
                      key={`bracket-${item.tag}`}
                      className="tag-chip"
                      onClick={() => onApplyInlineTag(item.tag, "bracket")}
                      title={`${item.hint}${item.key ? `（${item.key}）` : ""}`}
                    >
                      [{item.label}]
                      {item.key && <kbd style={{ marginLeft: 6 }}>{item.key}</kbd>}
                    </button>
                  ))}
              </div>
              <textarea
                ref={textareaRef}
                className="annotation-textarea"
                value={segment.phoneticText}
                placeholder="在此直接输入或编辑长沙方言记音字（含 <laugh> 等内联标签）"
                onChange={(event) =>
                  onUpdate({ phoneticText: event.target.value })
                }
                onDoubleClick={() => onSelectionChange(null)}
                spellCheck={false}
              />
              <div className="editor-toolbar" style={{ paddingTop: 4 }}>
                <span className="group-label">文本框选区</span>
                {inlineTags.map((item) => (
                  <button
                    key={`raw-${item.kind}-${item.tag}`}
                    className="tag-chip"
                    onClick={() => wrapTextareaSelection(item.tag, item.kind)}
                    title={
                      item.kind === "paired"
                        ? `在文本框选区外包裹 <${item.tag}>...</${item.tag}>`
                        : `在文本框光标处插入 [${item.tag}]`
                    }
                  >
                    {item.kind === "paired" ? `<${item.label}>` : `[${item.label}]`}
                  </button>
                ))}
              </div>
            </div>
            <div className="field">
              <span className="field-label">
                <span>原文本（参考 / 可选）</span>
              </span>
              <textarea
                value={segment.originalText}
                onChange={(event) =>
                  onUpdate({ originalText: event.target.value })
                }
                rows={3}
              />
            </div>
            <div className="field">
              <span className="field-label">
                <span>备注</span>
              </span>
              <textarea
                value={segment.notes}
                onChange={(event) => onUpdate({ notes: event.target.value })}
                rows={2}
                placeholder="录音瑕疵、需要复核、特殊处理等…"
              />
            </div>
          </div>

          <aside className="editor-side">
            <div className="side-card">
              <h3>
                <TagIcon size={14} />
                段级标签
              </h3>
              <p className="help-tip" style={{ marginTop: 0 }}>
                整段属性，会写入 JSONL <code>tags</code> 字段。
                未选中字时按 L/B/P/U/N 切换。
              </p>
              <div className="tag-grid">
                {segmentTags.map((tag) => (
                  <button
                    key={tag.value}
                    className={`tag-chip ${segment.tags.includes(tag.value) ? "active" : ""}`}
                    onClick={() => toggleSegmentTag(tag.value)}
                    title={`快捷键 ${tag.key}（无字符选区时）`}
                  >
                    {tag.label}
                    <kbd style={{ marginLeft: 6 }}>{tag.key}</kbd>
                  </button>
                ))}
              </div>
            </div>

            <div className="side-card">
              <h3>
                <StickyNote size={14} />
                情感（emotion 字段）
              </h3>
              <input
                value={segment.emotion.join("，")}
                onChange={(event) => updateEmotionRaw(event.target.value)}
                placeholder="多个情绪用逗号分隔"
              />
            </div>

            <div className="side-card">
              <h3>
                <Sparkles size={14} />
                时间信息
              </h3>
              <dl className="info-stat-grid">
                <dt>开始</dt>
                <dd>{formatClock(segment.startMs)}</dd>
                <dt>结束</dt>
                <dd>{formatClock(segment.endMs)}</dd>
                <dt>时长</dt>
                <dd>{formatClock(segment.durationMs)}</dd>
                <dt>原文件</dt>
                <dd style={{ wordBreak: "break-all" }}>{segment.sourceFileName}</dd>
              </dl>
            </div>
          </aside>
        </section>
      </section>
    </main>
  );
}
