import { useCallback, useEffect, useState } from "react";
import {
  ApiError,
  deleteRelease,
  listReleases,
  ReleaseRow,
  uploadRelease,
} from "../api";
import { formatBytes, formatRelative } from "../format";

const TARGETS = [
  { value: "windows-x86_64", label: "Windows x86_64" },
  { value: "darwin-x86_64", label: "macOS Intel" },
  { value: "darwin-aarch64", label: "macOS Apple Silicon" },
  { value: "linux-x86_64", label: "Linux x86_64" },
];

export default function AdminReleasesPage() {
  const [rows, setRows] = useState<ReleaseRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // upload form
  const [version, setVersion] = useState("");
  const [target, setTarget] = useState("windows-x86_64");
  const [channel, setChannel] = useState<"stable" | "beta">("stable");
  const [signature, setSignature] = useState("");
  const [notes, setNotes] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setRows(await listReleases());
      setError(null);
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function onUpload(e: React.FormEvent) {
    e.preventDefault();
    if (!file) {
      setError("请选择安装包文件");
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      await uploadRelease({
        version: version.trim(),
        target,
        channel,
        file,
        signature: signature.trim() || undefined,
        notes: notes.trim() || undefined,
      });
      setVersion("");
      setSignature("");
      setNotes("");
      setFile(null);
      // Clear native file input
      (document.getElementById("release-file") as HTMLInputElement | null)?.value &&
        ((document.getElementById("release-file") as HTMLInputElement).value = "");
      await refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    } finally {
      setSubmitting(false);
    }
  }

  async function onDelete(r: ReleaseRow) {
    if (!window.confirm(`删除 ${r.version} (${r.target}) ?`)) return;
    setError(null);
    try {
      await deleteRelease(r.id);
      await refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    }
  }

  return (
    <div className="page">
      <div className="section-head">
        <h2>客户端版本</h2>
      </div>

      {error && <div className="error-banner">{error}</div>}

      <form className="card" onSubmit={onUpload} style={{ padding: 16, marginBottom: 18 }}>
        <h3 style={{ marginTop: 0 }}>发布新版本</h3>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 1fr", gap: 10 }}>
          <label>
            <span style={{ fontSize: 12, color: "var(--muted)" }}>版本号</span>
            <input
              value={version}
              onChange={(e) => setVersion(e.target.value)}
              placeholder="0.1.1"
              required
              pattern="\d+(\.\d+){0,3}(-[A-Za-z0-9.]+)?"
              title="语义化版本号，如 0.1.1 或 1.0.0-beta"
            />
          </label>
          <label>
            <span style={{ fontSize: 12, color: "var(--muted)" }}>目标平台</span>
            <select value={target} onChange={(e) => setTarget(e.target.value)}>
              {TARGETS.map((t) => (
                <option key={t.value} value={t.value}>
                  {t.label}
                </option>
              ))}
            </select>
          </label>
          <label>
            <span style={{ fontSize: 12, color: "var(--muted)" }}>通道</span>
            <select
              value={channel}
              onChange={(e) => setChannel(e.target.value as "stable" | "beta")}
            >
              <option value="stable">stable</option>
              <option value="beta">beta</option>
            </select>
          </label>
        </div>
        <label style={{ display: "block", marginTop: 10 }}>
          <span style={{ fontSize: 12, color: "var(--muted)" }}>安装包文件（.msi / .exe / .dmg / ...）</span>
          <input
            id="release-file"
            type="file"
            onChange={(e) => setFile(e.target.files?.[0] ?? null)}
            required
          />
        </label>
        <label style={{ display: "block", marginTop: 10 }}>
          <span style={{ fontSize: 12, color: "var(--muted)" }}>
            Tauri 签名串（粘贴 `tauri signer sign` 输出的 base64；留空表示不可
            自动升级，只能手动下载）
          </span>
          <textarea
            value={signature}
            onChange={(e) => setSignature(e.target.value)}
            rows={3}
            placeholder="dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVR..."
          />
        </label>
        <label style={{ display: "block", marginTop: 10 }}>
          <span style={{ fontSize: 12, color: "var(--muted)" }}>发布说明</span>
          <textarea
            value={notes}
            onChange={(e) => setNotes(e.target.value)}
            rows={2}
            placeholder="什么变了？"
          />
        </label>
        <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 12 }}>
          <button className="primary" type="submit" disabled={submitting || !file}>
            {submitting ? "上传中…" : "发布"}
          </button>
        </div>
      </form>

      <div className="card">
        {rows === null ? (
          <div className="loader">加载中…</div>
        ) : rows.length === 0 ? (
          <div className="empty">还没有发布过任何版本。</div>
        ) : (
          <table className="tasks">
            <thead>
              <tr>
                <th>版本</th>
                <th>平台</th>
                <th>通道</th>
                <th>大小</th>
                <th>签名</th>
                <th>发布时间</th>
                <th style={{ textAlign: "right" }}>操作</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => (
                <tr key={r.id}>
                  <td>{r.version}</td>
                  <td>{r.target}</td>
                  <td>{r.channel}</td>
                  <td>{formatBytes(r.installer_size)}</td>
                  <td>{r.has_signature ? "✓" : "—"}</td>
                  <td>{formatRelative(r.created_at)}</td>
                  <td style={{ textAlign: "right" }}>
                    <a
                      href={`/api/releases/${r.id}/download`}
                      target="_blank"
                      rel="noreferrer"
                    >
                      <button type="button">下载</button>
                    </a>{" "}
                    <button
                      type="button"
                      className="danger"
                      onClick={() => onDelete(r)}
                    >
                      删除
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
