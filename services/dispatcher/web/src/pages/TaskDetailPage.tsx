import { useCallback, useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import {
  ApiError,
  deleteTask,
  downloadOutput,
  getTask,
  Task,
} from "../api";
import {
  formatBytes,
  formatDuration,
  formatRelative,
  formatStatus,
} from "../format";

const REFRESH_INTERVAL_MS = 4000;

export default function TaskDetailPage() {
  const { id = "" } = useParams();
  const navigate = useNavigate();
  const [task, setTask] = useState<Task | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const data = await getTask(id);
      setTask(data);
      setError(null);
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    }
  }, [id]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Only poll while a task is in motion. Once it's a terminal state, the
  // detail is static — no need to spam the API every few seconds.
  useEffect(() => {
    if (!task) return;
    const terminal = ["succeeded", "failed", "expired"].includes(task.status);
    if (terminal) return;
    const handle = window.setInterval(refresh, REFRESH_INTERVAL_MS);
    return () => window.clearInterval(handle);
  }, [task, refresh]);

  async function onDownload() {
    if (!task) return;
    setBusy(true);
    setError(null);
    try {
      await downloadOutput(task);
      // Refresh — the server set output_downloaded_at, which arms the
      // cleaner. Showing it gives the user a visible cue that the bundle
      // will soon disappear from disk.
      await refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  async function onDelete() {
    if (!task) return;
    if (!window.confirm("彻底删除这个任务（含文件）？")) return;
    setBusy(true);
    try {
      await deleteTask(task.id);
      navigate("/tasks", { replace: true });
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
      setBusy(false);
    }
  }

  if (!task) {
    return (
      <div className="page">
        {error ? (
          <div className="error-banner">{error}</div>
        ) : (
          <div className="loader">加载中…</div>
        )}
      </div>
    );
  }

  const s = formatStatus(task.status);
  const cleaned = task.files_cleaned_at != null;
  const canDownload =
    task.status === "succeeded" && task.output_size != null && !cleaned;

  const summary = task.summary;
  const durationByRole = summary?.duration_by_role as
    | Record<string, number>
    | undefined;

  return (
    <div className="page">
      <div className="section-head">
        <h2>{task.name || task.id}</h2>
        <span className="status-chip" style={{ background: s.color }}>
          {s.label}
        </span>
        <div className="grow" />
        <button onClick={() => navigate("/tasks")}>← 返回列表</button>
        <button
          className="danger"
          onClick={onDelete}
          disabled={busy}
        >
          删除
        </button>
      </div>

      {error && <div className="error-banner">{error}</div>}

      {task.error && (
        <div className="error-banner">
          <strong>Worker 报错：</strong> {task.error}
        </div>
      )}

      <div className="card" style={{ marginBottom: 18 }}>
        <dl className="detail-grid">
          <dt>任务 ID</dt>
          <dd>{task.id}</dd>
          <dt>创建时间</dt>
          <dd title={task.created_at}>{formatRelative(task.created_at)}</dd>
          <dt>输入</dt>
          <dd>
            {formatBytes(task.input_size)}
            {task.input_uploaded_at && (
              <> · 上传于 {formatRelative(task.input_uploaded_at)}</>
            )}
            {cleaned && task.input_size != null && (
              <span style={{ color: "var(--muted)" }}> · 已清理</span>
            )}
          </dd>
          <dt>产物</dt>
          <dd>
            {task.output_size != null ? (
              <>
                {formatBytes(task.output_size)}
                {task.output_ready_at && (
                  <> · {formatRelative(task.output_ready_at)} 上传</>
                )}
                {task.output_downloaded_at && (
                  <> · 已下载 {formatRelative(task.output_downloaded_at)}</>
                )}
                {cleaned && (
                  <span style={{ color: "var(--muted)" }}> · 已清理</span>
                )}
              </>
            ) : (
              "—"
            )}
          </dd>
          {task.completed_at && (
            <>
              <dt>完成于</dt>
              <dd title={task.completed_at}>
                {formatRelative(task.completed_at)}
              </dd>
            </>
          )}
        </dl>
        <div
          style={{
            padding: "14px 22px",
            borderTop: "1px solid var(--border)",
            display: "flex",
            gap: 8,
          }}
        >
          <button
            className="primary"
            disabled={!canDownload || busy}
            onClick={onDownload}
            title={
              cleaned
                ? "产物已被清理"
                : task.status !== "succeeded"
                ? "任务尚未成功"
                : ""
            }
          >
            下载产物 zip
          </button>
        </div>
      </div>

      {summary && (
        <div className="card" style={{ marginBottom: 18 }}>
          <div style={{ padding: "16px 22px" }}>
            <h3 style={{ marginTop: 0 }}>产出概况</h3>
            <dl className="detail-grid" style={{ padding: 0 }}>
              {summary.segment_count != null && (
                <>
                  <dt>切片总数</dt>
                  <dd>{summary.segment_count}</dd>
                </>
              )}
              {summary.source_files != null && (
                <>
                  <dt>源音频文件</dt>
                  <dd>{summary.source_files}</dd>
                </>
              )}
              {durationByRole && (
                <>
                  <dt>各角色总时长</dt>
                  <dd>
                    {Object.entries(durationByRole).map(([role, ms]) => (
                      <div key={role}>
                        {roleLabel(role)}：{formatDuration(ms)}
                      </div>
                    ))}
                  </dd>
                </>
              )}
              {summary.asr_engine && (
                <>
                  <dt>ASR 引擎</dt>
                  <dd>{summary.asr_engine}</dd>
                </>
              )}
              {summary.elapsed_ms != null && (
                <>
                  <dt>耗时</dt>
                  <dd>{formatDuration(summary.elapsed_ms)}</dd>
                </>
              )}
            </dl>
            <details style={{ marginTop: 12 }}>
              <summary style={{ cursor: "pointer", color: "var(--muted)" }}>
                查看原始 summary JSON
              </summary>
              <pre className="summary-box">
                {JSON.stringify(summary, null, 2)}
              </pre>
            </details>
          </div>
        </div>
      )}
    </div>
  );
}

function roleLabel(role: string): string {
  switch (role) {
    case "user":
      return "陪聊";
    case "assistant":
      return "发音人";
    case "unknown":
      return "未标注";
    default:
      return role;
  }
}
