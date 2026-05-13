import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { ApiError, listTasks, Task } from "../api";
import {
  formatBytes,
  formatRelative,
  formatStatus,
} from "../format";
import UploadModal from "./UploadModal";

const REFRESH_INTERVAL_MS = 5000;

/** Owner-facing task list. Refreshes on an interval so a Worker picking
 *  up a pending task shows up without the user reloading. */
export default function TasksPage() {
  const [tasks, setTasks] = useState<Task[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showUpload, setShowUpload] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const next = await listTasks();
      setTasks(next);
      setError(null);
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const handle = window.setInterval(refresh, REFRESH_INTERVAL_MS);
    return () => window.clearInterval(handle);
  }, [refresh]);

  return (
    <div className="page">
      <div className="section-head">
        <h2>任务</h2>
        <div className="grow" />
        <button onClick={() => void refresh()} title="刷新">
          ↻
        </button>
        <button className="primary" onClick={() => setShowUpload(true)}>
          + 新建任务
        </button>
      </div>

      {error && <div className="error-banner">{error}</div>}

      <div className="card">
        {tasks === null ? (
          <div className="loader">加载中…</div>
        ) : tasks.length === 0 ? (
          <div className="empty">
            还没有任务。点右上角「新建任务」上传一个 zip 试试。
          </div>
        ) : (
          <table className="tasks">
            <thead>
              <tr>
                <th>名称</th>
                <th>模式</th>
                <th>状态</th>
                <th>进度</th>
                <th>输入</th>
                <th>产物</th>
                <th>创建</th>
              </tr>
            </thead>
            <tbody>
              {tasks.map((t) => {
                const s = formatStatus(t.status);
                const mode = t.mode ?? "dialect";
                const modeLabel = mode === "semantic" ? "模式 2 · 语义" : "模式 1 · 方言";
                const modeBg =
                  mode === "semantic" ? "rgba(124, 58, 237, 0.12)" : "rgba(13, 148, 136, 0.12)";
                const modeFg =
                  mode === "semantic" ? "#7c3aed" : "#0d9488";
                const showProgress =
                  t.progress_percent != null &&
                  (t.status === "claimed" || t.status === "running");
                return (
                  <tr key={t.id}>
                    <td>
                      <Link to={`/tasks/${t.id}`}>{t.name || t.id}</Link>
                    </td>
                    <td>
                      <span
                        className="status-chip"
                        style={{
                          background: modeBg,
                          color: modeFg,
                          fontWeight: 500,
                        }}
                        title={
                          mode === "semantic"
                            ? "Mode 2：LLM 决定切点，规范化文本，产出 xlsx"
                            : "Mode 1：按静音切，保留全部声学事件"
                        }
                      >
                        {modeLabel}
                      </span>
                    </td>
                    <td>
                      <span
                        className="status-chip"
                        style={{ background: s.color }}
                      >
                        {s.label}
                      </span>
                    </td>
                    <td style={{ minWidth: 140 }}>
                      {showProgress ? (
                        <div
                          title={`${t.progress_stage ?? ""} · ${t.progress_detail ?? ""}`.trim()}
                        >
                          <div
                            style={{
                              fontSize: 11,
                              opacity: 0.7,
                              marginBottom: 2,
                            }}
                          >
                            {t.progress_stage ?? "处理中"} · {t.progress_percent}%
                          </div>
                          <div
                            style={{
                              height: 4,
                              borderRadius: 2,
                              background: "#e2e8f0",
                              overflow: "hidden",
                            }}
                          >
                            <div
                              style={{
                                width: `${t.progress_percent ?? 0}%`,
                                height: "100%",
                                background: "var(--accent, #0d9488)",
                                transition: "width 0.4s ease",
                              }}
                            />
                          </div>
                        </div>
                      ) : (
                        <span style={{ opacity: 0.4 }}>—</span>
                      )}
                    </td>
                    <td>{formatBytes(t.input_size)}</td>
                    <td>{formatBytes(t.output_size)}</td>
                    <td title={t.created_at}>
                      {formatRelative(t.created_at)}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </div>

      {showUpload && (
        <UploadModal
          onClose={() => setShowUpload(false)}
          onCreated={() => {
            setShowUpload(false);
            void refresh();
          }}
        />
      )}
    </div>
  );
}
