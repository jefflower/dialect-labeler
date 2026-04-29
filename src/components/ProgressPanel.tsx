import type { ProgressState } from "../types";

export function ProgressPanel({
  progress,
  onCancel,
}: {
  progress: ProgressState;
  /** Optional cancel handler — when provided, a button is rendered
   *  next to the label. Use for long-running tasks (Whisper / Ollama
   *  recognition) where the user might want to abort mid-run. */
  onCancel?: () => void;
}) {
  if (!progress.visible) return null;
  const indeterminate = progress.indeterminate || progress.total <= 0;
  const percent =
    progress.total > 0
      ? Math.min(100, Math.round((progress.current / progress.total) * 100))
      : 0;

  return (
    <section className="progress-panel" aria-live="polite">
      <div className="label-block">
        <strong>{progress.label}</strong>
        <span className="progress-numbers">
          {progress.total > 0
            ? `${progress.current}/${progress.total} · ${percent}%`
            : "进行中…"}
        </span>
        {onCancel && (
          <button
            type="button"
            className="btn-ghost progress-cancel"
            onClick={onCancel}
            title="停止当前识别（已完成的段会保留缓存，下次启动会跳过）"
          >
            停止
          </button>
        )}
      </div>
      <div
        className={`progress-track ${indeterminate ? "indeterminate" : ""}`}
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={percent}
      >
        <div className="progress-fill" style={{ width: `${percent}%` }} />
      </div>
      <span
        className="progress-detail"
        title={progress.detail}
      >
        {progress.detail}
      </span>
    </section>
  );
}
