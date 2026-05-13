import { DragEvent, FormEvent, useRef, useState } from "react";
import { ApiError, createTask, TaskMode } from "../api";
import { formatBytes } from "../format";

interface Props {
  onClose: () => void;
  onCreated: () => void;
}

/** New-task dialog. Drag-drop OR file picker; required name + mode. */
export default function UploadModal({ onClose, onCreated }: Props) {
  const [name, setName] = useState("");
  const [file, setFile] = useState<File | null>(null);
  // Mode picked at upload time — committed on the server with the
  // upload. The Mac Worker uses this value verbatim and IGNORES whatever
  // the bundle's project.json says, so the user's choice here is the
  // only one that matters. Default `dialect` matches the historical
  // path (silence-based cutter for dialect recordings).
  const [mode, setMode] = useState<TaskMode>("dialect");
  const [dragging, setDragging] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  function pick(f: File) {
    setFile(f);
    // If the user hasn't named the task, default to the file's stem.
    if (!name.trim()) {
      setName(f.name.replace(/\.[^.]+$/, ""));
    }
  }

  function onDrop(e: DragEvent<HTMLDivElement>) {
    e.preventDefault();
    setDragging(false);
    const f = e.dataTransfer.files?.[0];
    if (f) pick(f);
  }

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    if (!file) {
      setError("请选择要上传的 zip 文件");
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      await createTask(name.trim() || file.name, file, mode);
      onCreated();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div
      className="modal-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="modal" role="dialog" aria-modal="true">
        <h2>新建任务</h2>
        <form onSubmit={onSubmit}>
          {/* Mode picker comes FIRST in the form on purpose — without it,
              the same input zip could be processed wildly differently
              (silence-based cut vs. LLM semantic cut), and "what mode
              did the worker run?" would be invisible after the fact. */}
          <div className="field">
            <label>处理模式</label>
            <div
              role="radiogroup"
              aria-label="处理模式"
              style={{ display: "flex", gap: 8, flexWrap: "wrap" }}
            >
              <label
                style={{
                  display: "flex",
                  flex: "1 1 200px",
                  gap: 8,
                  padding: "10px 12px",
                  border: "1px solid",
                  borderColor:
                    mode === "dialect" ? "var(--accent, #0d9488)" : "#cbd5e1",
                  borderRadius: 8,
                  cursor: "pointer",
                  background:
                    mode === "dialect" ? "rgba(13, 148, 136, 0.06)" : "transparent",
                }}
              >
                <input
                  type="radio"
                  name="mode"
                  value="dialect"
                  checked={mode === "dialect"}
                  onChange={() => setMode("dialect")}
                />
                <span style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                  <strong>模式 1 · 方言</strong>
                  <small style={{ opacity: 0.75 }}>
                    按静音切；保留全部声学事件
                  </small>
                </span>
              </label>
              <label
                style={{
                  display: "flex",
                  flex: "1 1 200px",
                  gap: 8,
                  padding: "10px 12px",
                  border: "1px solid",
                  borderColor:
                    mode === "semantic" ? "var(--accent, #0d9488)" : "#cbd5e1",
                  borderRadius: 8,
                  cursor: "pointer",
                  background:
                    mode === "semantic" ? "rgba(13, 148, 136, 0.06)" : "transparent",
                }}
              >
                <input
                  type="radio"
                  name="mode"
                  value="semantic"
                  checked={mode === "semantic"}
                  onChange={() => setMode("semantic")}
                />
                <span style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                  <strong>模式 2 · 语义</strong>
                  <small style={{ opacity: 0.75 }}>
                    LLM 滑窗找切点；≤ 90s/段
                  </small>
                </span>
              </label>
            </div>
          </div>

          {/* Upload-format helper — explains what to put in the zip
              and what comes back. Mode-dependent so the user always
              sees the right shape for their selected algorithm. */}
          <div
            className="field"
            style={{
              background: "rgba(13, 148, 136, 0.06)",
              border: "1px solid rgba(13, 148, 136, 0.18)",
              borderRadius: 8,
              padding: "10px 12px",
              fontSize: 12,
              color: "var(--text)",
              lineHeight: 1.6,
            }}
          >
            <div style={{ fontWeight: 600, marginBottom: 4 }}>
              📦 上传 zip 该装什么
            </div>
            {mode === "dialect" ? (
              <>
                <div>
                  打包一个 <strong>文件夹</strong>，文件夹里按角色分目录放 WAV：
                </div>
                <pre style={{ margin: "6px 0", fontSize: 11, background: "rgba(0,0,0,0.04)", padding: 8, borderRadius: 4, overflow: "auto" }}>
{`<你的批次名>/
├── 发音人/
│   ├── XXX-发音人-自由对话-话题01.wav
│   └── ...
└── 陪聊人/
    ├── XXX-陪聊人-自由对话-话题01.wav
    └── ...`}
                </pre>
                <div>
                  · 音频格式：<strong>WAV / 16 或 24 bit / 44.1k 或 48k</strong>（单声道优先）
                  <br />
                  · 角色识别按文件夹名（<code>发音人</code> / <code>陪聊人</code>）
                  <br />
                  · 产物：Mode 1 项目目录（含 <code>project.json</code> + 切片 wav + <code>export.jsonl</code>）
                </div>
              </>
            ) : (
              <>
                <div>
                  打包一个 <strong>文件夹</strong>，里面放一个或多个长录音 WAV：
                </div>
                <pre style={{ margin: "6px 0", fontSize: 11, background: "rgba(0,0,0,0.04)", padding: 8, borderRadius: 4, overflow: "auto" }}>
{`<你的批次名>/
├── 录音01.wav        (任意命名，任意时长)
├── 录音02.wav
└── ...`}
                </pre>
                <div>
                  · 音频格式：<strong>WAV / 16 或 24 bit / 44.1k 或 48k</strong>（单声道优先）
                  <br />
                  · 子目录任意，扫描会递归找全部 wav
                  <br />
                  · 算法：每次取 ≤ 90s 窗口 → Whisper 转写 → LLM 在自然句末选切点 → 切下来 → 滑窗继续
                  <br />
                  · 产物：Mode-1 兼容项目目录（含 <code>project.json</code> + 切片 wav，文本是 Whisper 转写初稿），Windows 客户端直接打开 <code>project.json</code> 即可标注
                </div>
              </>
            )}
          </div>
          <div className="field">
            <label htmlFor="name">任务名</label>
            <input
              id="name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="例如：长沙话 0001 批次"
              required
            />
          </div>
          <div className="field">
            <label>输入包（zip）</label>
            <div
              className={`drop-zone ${dragging ? "active" : ""}`}
              onDragOver={(e) => {
                e.preventDefault();
                setDragging(true);
              }}
              onDragLeave={() => setDragging(false)}
              onDrop={onDrop}
              onClick={() => inputRef.current?.click()}
              role="button"
              tabIndex={0}
            >
              {file ? (
                <>
                  <strong>{file.name}</strong>
                  <div style={{ marginTop: 4 }}>{formatBytes(file.size)}</div>
                </>
              ) : (
                <>拖入文件，或<strong>点击选择</strong>。</>
              )}
              <input
                ref={inputRef}
                type="file"
                accept=".zip,application/zip"
                style={{ display: "none" }}
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (f) pick(f);
                }}
              />
            </div>
          </div>
          {error && <div className="error-banner">{error}</div>}
          <div className="actions">
            <button type="button" onClick={onClose} disabled={submitting}>
              取消
            </button>
            <button
              type="submit"
              className="primary"
              disabled={submitting || !file}
            >
              {submitting ? "上传中…" : "创建任务"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
