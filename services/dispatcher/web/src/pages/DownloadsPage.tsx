import { useEffect, useState } from "react";
import { ApiError, getLatestRelease, LatestRelease } from "../api";
import { formatBytes, formatRelative } from "../format";

/** Public-facing client download page.
 *
 *  Linked from outside the auth wall (no token required). Lists the
 *  latest Windows installer; an admin manages versions from
 *  /admin/releases. */
export default function DownloadsPage() {
  const [latest, setLatest] = useState<LatestRelease | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const data = await getLatestRelease("windows-x86_64");
        if (!cancelled) setLatest(data);
      } catch (err) {
        if (!cancelled) {
          if (err instanceof ApiError && err.status === 404) {
            setError("还没有发布过 Windows 客户端");
          } else {
            setError(err instanceof Error ? err.message : String(err));
          }
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <div className="page">
      <div className="section-head">
        <h2>下载 Windows 客户端</h2>
      </div>

      {error && <div className="empty">{error}</div>}

      {latest && (
        <div className="card" style={{ padding: 22 }}>
          <h3 style={{ marginTop: 0 }}>v{latest.version}</h3>
          <p style={{ color: "var(--muted)", margin: "4px 0 14px" }}>
            {latest.channel} · {formatBytes(latest.installer_size)} · 发布于{" "}
            {formatRelative(latest.pub_date)}
          </p>
          {latest.notes && (
            <pre className="summary-box" style={{ marginBottom: 16 }}>
              {latest.notes}
            </pre>
          )}
          <a href={latest.download_url} download>
            <button className="primary" type="button">
              下载安装包
            </button>
          </a>
          <p style={{ marginTop: 18, color: "var(--muted)", fontSize: 13 }}>
            安装后，在客户端「云端 Worker」面板登录本服务即可自动接单。
            后续版本会通过应用内自动升级提示安装。
          </p>
        </div>
      )}
    </div>
  );
}
