import { useEffect, useRef } from "react";
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
import type { ProgressState, SegmentRecord } from "../types";
import { emotionOptions, inlineTags, roleLabels, tagOptions } from "../defaults";
import { formatClock, formatMsRange } from "../lib";
import { Waveform } from "./Waveform";
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

  useEffect(() => {
    textareaRef.current?.focus();
  }, [segment.id]);

  function insertInlineTag(tag: string) {
    const editor = textareaRef.current;
    if (!editor) return;
    const start = editor.selectionStart;
    const end = editor.selectionEnd;
    const value = segment.phoneticText;
    const selected = value.slice(start, end);
    const open = `<${tag}>`;
    const close = `</${tag}>`;
    const next = `${value.slice(0, start)}${open}${selected}${close}${value.slice(end)}`;
    onUpdate({ phoneticText: next });
    requestAnimationFrame(() => {
      editor.focus();
      const caret = start + open.length;
      editor.setSelectionRange(caret, caret + selected.length);
    });
  }

  function toggleEmotion(emotion: string) {
    if (segment.emotion.includes(emotion)) {
      onUpdate({ emotion: segment.emotion.filter((e) => e !== emotion) });
    } else {
      onUpdate({ emotion: [...segment.emotion, emotion] });
    }
  }

  function toggleTag(tag: string) {
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
          >
            {isPlaying ? <Pause size={18} /> : <Play size={18} />}
          </button>
          <Waveform
            path={playbackPath}
            durationMs={durationMs}
            currentMs={currentMs}
            onSeek={onSeekRatio}
            height={88}
          />
          <span className="player-bar-time">
            {formatClock(currentMs)} / {formatClock(durationMs)}
          </span>
        </section>

        {progress.visible && <ProgressPanel progress={progress} />}

        <section className="annotation-editor">
          <div className="editor-column">
            <div className="field">
              <span className="field-label">
                <span>记音字（最终输出）</span>
                <span style={{ color: "var(--text-tertiary)" }}>
                  按"发音接近"原则选字
                </span>
              </span>
              <div className="editor-toolbar">
                <span className="group-label">情绪</span>
                {emotionOptions.map((emotion) => (
                  <button
                    key={emotion}
                    className={`tag-chip ${segment.emotion.includes(emotion) ? "active" : ""}`}
                    onClick={() => toggleEmotion(emotion)}
                  >
                    {emotion}
                  </button>
                ))}
              </div>
              <div className="editor-toolbar">
                <span className="group-label">内联标签</span>
                {inlineTags.map((item) => (
                  <button
                    key={item.tag}
                    className="tag-chip"
                    onClick={() => insertInlineTag(item.tag)}
                  >
                    + {item.label}
                  </button>
                ))}
              </div>
              <textarea
                ref={textareaRef}
                className="annotation-textarea"
                value={segment.phoneticText}
                placeholder="在此输入或编辑长沙方言记音字…"
                onChange={(event) =>
                  onUpdate({ phoneticText: event.target.value })
                }
                spellCheck={false}
              />
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
              <div className="tag-grid">
                {tagOptions.map((tag) => (
                  <button
                    key={tag.value}
                    className={`tag-chip ${segment.tags.includes(tag.value) ? "active" : ""}`}
                    onClick={() => toggleTag(tag.value)}
                    title={`快捷键 ${tag.key}`}
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
                情感（用于 emotion 字段）
              </h3>
              <input
                value={segment.emotion.join("，")}
                onChange={(event) => updateEmotionRaw(event.target.value)}
                placeholder="多个情绪用逗号分隔"
              />
              <p className="help-tip">
                导出时这些情绪会写入<code>emotion</code>字段。建议每段 1 个。
              </p>
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
