import { useCallback, useEffect, useState } from "react";
import { AdminStats, ApiError, getAdminStats } from "../api";
import { formatBytes, formatStatus } from "../format";

const REFRESH_MS = 5000;

/** Admin landing page — single GET /api/admin/stats, polled. Designed
 *  to give an oncall a glance: queue depth, in-flight, last-24h
 *  throughput, where the disk is going. */
export default function AdminDashboardPage() {
  const [stats, setStats] = useState<AdminStats | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStats(await getAdminStats());
      setError(null);
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const handle = window.setInterval(refresh, REFRESH_MS);
    return () => window.clearInterval(handle);
  }, [refresh]);

  return (
    <div className="page">
      <div className="section-head">
        <h2>仪表盘</h2>
        <div className="grow" />
        <button onClick={() => void refresh()}>↻</button>
      </div>

      {error && <div className="error-banner">{error}</div>}

      {!stats ? (
        <div className="loader">加载中…</div>
      ) : (
        <>
          <div className="kpi-row">
            <KPI label="排队中" value={stats.queue_depth} color="#888" />
            <KPI label="处理中" value={stats.in_flight} color="#0078d4" />
            <KPI label="24h 成功" value={stats.succeeded_24h} color="#107c10" />
            <KPI label="24h 失败" value={stats.failed_24h} color="#d13438" />
          </div>

          <div className="card" style={{ marginTop: 18 }}>
            <div style={{ padding: 16 }}>
              <h3 style={{ marginTop: 0 }}>任务状态分布</h3>
              <table className="tasks">
                <thead>
                  <tr>
                    <th>状态</th>
                    <th style={{ textAlign: "right" }}>计数</th>
                  </tr>
                </thead>
                <tbody>
                  {Object.entries(stats.task_counts).length === 0 ? (
                    <tr>
                      <td colSpan={2} className="empty">
                        还没有任务
                      </td>
                    </tr>
                  ) : (
                    Object.entries(stats.task_counts).map(([key, value]) => {
                      const s = formatStatus(key);
                      return (
                        <tr key={key}>
                          <td>
                            <span
                              className="status-chip"
                              style={{ background: s.color }}
                            >
                              {s.label}
                            </span>
                          </td>
                          <td style={{ textAlign: "right" }}>{value}</td>
                        </tr>
                      );
                    })
                  )}
                </tbody>
              </table>
            </div>
          </div>

          <div className="card" style={{ marginTop: 18 }}>
            <div style={{ padding: 16 }}>
              <h3 style={{ marginTop: 0 }}>磁盘占用</h3>
              <table className="tasks">
                <thead>
                  <tr>
                    <th>类别</th>
                    <th style={{ textAlign: "right" }}>文件数</th>
                    <th style={{ textAlign: "right" }}>大小</th>
                  </tr>
                </thead>
                <tbody>
                  <tr>
                    <td>输入包</td>
                    <td style={{ textAlign: "right" }}>
                      {stats.storage.inputs_files}
                    </td>
                    <td style={{ textAlign: "right" }}>
                      {formatBytes(stats.storage.inputs_bytes)}
                    </td>
                  </tr>
                  <tr>
                    <td>产物包</td>
                    <td style={{ textAlign: "right" }}>
                      {stats.storage.outputs_files}
                    </td>
                    <td style={{ textAlign: "right" }}>
                      {formatBytes(stats.storage.outputs_bytes)}
                    </td>
                  </tr>
                  <tr>
                    <td>客户端发布</td>
                    <td style={{ textAlign: "right" }}>
                      {stats.storage.releases_files}
                    </td>
                    <td style={{ textAlign: "right" }}>
                      {formatBytes(stats.storage.releases_bytes)}
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          </div>

          <div
            style={{
              marginTop: 18,
              color: "var(--muted)",
              fontSize: 13,
            }}
          >
            账号：管理员 {stats.total_admin_count} · 用户 {stats.total_user_count}
          </div>
        </>
      )}
    </div>
  );
}

function KPI({
  label,
  value,
  color,
}: {
  label: string;
  value: number;
  color: string;
}) {
  return (
    <div className="kpi-card">
      <div className="kpi-label">{label}</div>
      <div className="kpi-value" style={{ color }}>
        {value}
      </div>
    </div>
  );
}
