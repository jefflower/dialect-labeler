import { DragEvent, FormEvent, useRef, useState } from "react";
import { ApiError, createTask } from "../api";
import { formatBytes } from "../format";

interface Props {
  onClose: () => void;
  onCreated: () => void;
}

/** New-task dialog. Drag-drop OR file picker; required name field. */
export default function UploadModal({ onClose, onCreated }: Props) {
  const [name, setName] = useState("");
  const [file, setFile] = useState<File | null>(null);
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
      await createTask(name.trim() || file.name, file);
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
