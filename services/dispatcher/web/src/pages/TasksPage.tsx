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
                <th>状态</th>
                <th>输入</th>
                <th>产物</th>
                <th>创建</th>
              </tr>
            </thead>
            <tbody>
              {tasks.map((t) => {
                const s = formatStatus(t.status);
                return (
                  <tr key={t.id}>
                    <td>
                      <Link to={`/tasks/${t.id}`}>{t.name || t.id}</Link>
                    </td>
                    <td>
                      <span
                        className="status-chip"
                        style={{ background: s.color }}
                      >
                        {s.label}
                      </span>
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
