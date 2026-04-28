import type { ProgressState } from "../types";

export function ProgressPanel({ progress }: { progress: ProgressState }) {
  if (!progress.visible) return null;
  const indeterminate = progress.indeterminate || progress.total <= 0;
  const percent = progress.total > 0
    ? Math.min(100, Math.round((progress.current / progress.total) * 100))
    : 0;

  return (
    <section className="progress-panel">
      <div className="label-block">
        <strong>{progress.label}</strong>
        <span title={progress.detail}>{progress.detail}</span>
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
      <span className="progress-numbers">
        {progress.total > 0
          ? `${progress.current}/${progress.total} · ${percent}%`
          : "进行中…"}
      </span>
    </section>
  );
}
