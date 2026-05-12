/**
 * CloudPane — modal that owns the "cloud Worker" lifecycle.
 *
 * Visible state machine:
 *   未登录  → server-URL + email/password + Sign In
 *   已登录  → user info + "auto-claim" toggle + current task progress
 *
 * Persistence:
 *   The token + base URL live in tauri-plugin-store under `cloud.session.v1`.
 *   On mount we read it back and call `cloud_set_session` so the Rust
 *   worker has credentials in memory — no round-trip to JS per claim.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Cloud,
  CloudOff,
  Download as DownloadIcon,
  Loader2,
  LogIn,
  LogOut,
  Pause,
  Play,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { Store } from "@tauri-apps/plugin-store";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { check, type Update } from "@tauri-apps/plugin-updater";
import type {
  CloudStatus,
  CloudUser,
  CloudWorkerConfig,
} from "../lib";
import { ipc } from "../lib";

const STORE_FILE = "cloud-session.json";
const SESSION_KEY = "cloud.session.v1";
const DEFAULT_BASE = "http://localhost:8080";

type SessionStored = {
  baseUrl: string;
  token: string;
  user: CloudUser;
};

type Props = {
  open: boolean;
  onClose: () => void;
  /** Bound to AppSettings, mirrored into the Rust state at toggle time
   *  so the worker uses whatever ASR/LLM config the user already
   *  configured for the windowed flow. */
  workerConfig: CloudWorkerConfig;
};

const STAGE_LABELS: Record<string, string> = {
  downloading: "下载输入",
  extracting: "解压",
  cutting: "切分音频",
  recognizing: "识别 + 润色",
  bundling: "打包产物",
  uploading: "上传产物",
};

type UpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "up-to-date"; current: string }
  | { kind: "available"; current: string; next: string; update: Update }
  | { kind: "installing"; next: string }
  | { kind: "installed"; next: string }
  | { kind: "error"; message: string };

export function CloudPane({ open, onClose, workerConfig }: Props) {
  const [status, setStatus] = useState<CloudStatus | null>(null);
  const [baseUrl, setBaseUrl] = useState(DEFAULT_BASE);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [update, setUpdate] = useState<UpdateState>({ kind: "idle" });

  const refresh = useCallback(async () => {
    try {
      const next = await ipc.cloudStatus();
      setStatus(next);
      if (next.base_url && !baseUrl.startsWith(next.base_url)) {
        setBaseUrl(next.base_url);
      }
    } catch (err) {
      setError(String(err));
    }
  }, [baseUrl]);

  // Hydrate from local store on first mount, push back to Rust if found.
  // We treat an empty / missing token as "session forgotten" rather than
  // trying to silently re-login — better to ask the user than to fail
  // a worker_loop claim with a stale 401.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const store = await Store.load(STORE_FILE);
        const saved = await store.get<SessionStored>(SESSION_KEY);
        if (saved && saved.token && !cancelled) {
          setBaseUrl(saved.baseUrl);
          await ipc.cloudSetSession({
            baseUrl: saved.baseUrl,
            token: saved.token,
            user: saved.user,
          });
        }
      } catch (err) {
        console.warn("cloud store load failed", err);
      }
      if (!cancelled) await refresh();
    })();
    return () => {
      cancelled = true;
    };
  }, [refresh]);

  // Re-render on backend status-changed events (worker started, task
  // stage advanced, etc.).
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    listen("cloud://status-changed", () => {
      void refresh();
    }).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, [refresh]);

  // Poll periodically as a fallback — the backend emits an event for
  // each stage transition but the heartbeat happens in a child thread
  // that doesn't emit, so we still want a slow refresh.
  useEffect(() => {
    if (!open) return;
    const handle = window.setInterval(refresh, 3000);
    return () => window.clearInterval(handle);
  }, [open, refresh]);

  const onLogin = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const url = baseUrl.trim().replace(/\/+$/, "");
      const result = await ipc.cloudLogin({
        baseUrl: url,
        email: email.trim(),
        password,
      });
      // Persist for next launch. Both pieces matter: the URL so we don't
      // ask the user to retype it, the token so a relaunch picks up the
      // worker loop without a fresh login.
      const store = await Store.load(STORE_FILE);
      await store.set(SESSION_KEY, {
        baseUrl: url,
        token: result.token,
        user: result.user,
      } satisfies SessionStored);
      await store.save();
      setPassword("");
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, [baseUrl, email, password, refresh]);

  const onLogout = useCallback(async () => {
    setBusy(true);
    try {
      await ipc.cloudLogout();
      const store = await Store.load(STORE_FILE);
      await store.delete(SESSION_KEY);
      await store.save();
      await refresh();
    } finally {
      setBusy(false);
    }
  }, [refresh]);

  const onToggleWorker = useCallback(
    async (next: boolean) => {
      setBusy(true);
      setError(null);
      try {
        await ipc.cloudSetWorkerConfig({ config: workerConfig });
        await ipc.cloudSetWorkerEnabled({ enabled: next });
        await refresh();
      } catch (err) {
        setError(String(err));
      } finally {
        setBusy(false);
      }
    },
    [refresh, workerConfig]
  );

  const stageLabel = useMemo(() => {
    const stage = status?.current_task?.stage;
    if (!stage) return null;
    return STAGE_LABELS[stage] ?? stage;
  }, [status?.current_task?.stage]);

  // Load the running app's version once for "you're on vX.Y.Z" display.
  useEffect(() => {
    invoke<string>("app_version").then(setAppVersion).catch(() => undefined);
  }, []);

  const onCheckUpdate = useCallback(async () => {
    setUpdate({ kind: "checking" });
    try {
      const result = await check();
      if (!result) {
        setUpdate({ kind: "up-to-date", current: appVersion ?? "?" });
        return;
      }
      setUpdate({
        kind: "available",
        current: appVersion ?? "?",
        next: result.version,
        update: result,
      });
    } catch (err) {
      setUpdate({ kind: "error", message: String(err) });
    }
  }, [appVersion]);

  const onInstallUpdate = useCallback(async () => {
    if (update.kind !== "available") return;
    setUpdate({ kind: "installing", next: update.next });
    try {
      await update.update.downloadAndInstall();
      setUpdate({ kind: "installed", next: update.next });
    } catch (err) {
      setUpdate({ kind: "error", message: String(err) });
    }
  }, [update]);

  if (!open) return null;

  const loggedIn = status?.logged_in === true;

  return (
    <div className="modal-overlay" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="cloud-pane" role="dialog" aria-modal="true">
        <header>
          <h2>
            <Cloud size={18} /> 云端 Worker
          </h2>
          <button className="btn-ghost" onClick={onClose}>
            关闭
          </button>
        </header>

        {!loggedIn ? (
          <section className="cloud-login">
            <p className="cloud-hint">
              登录调度服务后，本机可以勾选「自动接单」从云端拉任务。
            </p>
            <label>
              <span>服务器地址</span>
              <input
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                placeholder="http://example.com:8080"
              />
            </label>
            <label>
              <span>邮箱</span>
              <input
                type="email"
                autoComplete="email"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                placeholder="you@example.com"
              />
            </label>
            <label>
              <span>密码</span>
              <input
                type="password"
                autoComplete="current-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </label>
            {error && <div className="cloud-error">{error}</div>}
            <button
              className="btn-primary"
              onClick={onLogin}
              disabled={busy || !email.trim() || !password.trim()}
            >
              {busy ? <Loader2 size={14} className="spin" /> : <LogIn size={14} />}
              登录
            </button>
          </section>
        ) : (
          <section className="cloud-session">
            <div className="cloud-row">
              <div>
                <strong>{status?.user?.email}</strong>
                <span className="cloud-role">
                  {status?.user?.role === "admin" ? "管理员" : "用户"}
                </span>
              </div>
              <button className="btn-ghost" onClick={onLogout} disabled={busy}>
                <LogOut size={14} /> 注销
              </button>
            </div>
            <div className="cloud-row" style={{ color: "var(--text-secondary, #888)" }}>
              <span>服务器</span>
              <span>{status?.base_url}</span>
            </div>

            <div className="cloud-worker-row">
              <div>
                <strong>
                  {status?.worker_enabled ? (
                    <>
                      <Cloud size={14} /> 自动接单已开启
                    </>
                  ) : (
                    <>
                      <CloudOff size={14} /> 自动接单已关闭
                    </>
                  )}
                </strong>
                <p className="cloud-hint">
                  开启后保持运行（窗口可关到托盘），后台轮询并消费任务。
                </p>
              </div>
              <button
                className={status?.worker_enabled ? "btn-ghost" : "btn-primary"}
                onClick={() => onToggleWorker(!status?.worker_enabled)}
                disabled={busy}
              >
                {status?.worker_enabled ? <Pause size={14} /> : <Play size={14} />}
                {status?.worker_enabled ? "停止" : "开始接单"}
              </button>
            </div>

            {status?.current_task && (
              <div className="cloud-current-task">
                <Loader2 size={14} className="spin" />
                <div>
                  <strong>{status.current_task.name}</strong>
                  <div className="cloud-hint">{stageLabel}</div>
                </div>
              </div>
            )}

            {status?.last_error && (
              <div className="cloud-error">最近错误：{status.last_error}</div>
            )}

            <div className="cloud-worker-row">
              <div>
                <strong>
                  <DownloadIcon size={14} /> 客户端版本
                </strong>
                <p className="cloud-hint">
                  当前 v{appVersion ?? "?"}。{updateBlurb(update)}
                </p>
              </div>
              {update.kind === "available" ? (
                <button
                  className="btn-primary"
                  onClick={onInstallUpdate}
                  disabled={busy}
                >
                  安装 v{update.next}
                </button>
              ) : update.kind === "installed" ? (
                <span className="cloud-hint">下次启动生效</span>
              ) : (
                <button
                  className="btn-ghost"
                  onClick={onCheckUpdate}
                  disabled={update.kind === "checking" || update.kind === "installing"}
                >
                  {update.kind === "checking" ? (
                    <Loader2 size={14} className="spin" />
                  ) : (
                    <DownloadIcon size={14} />
                  )}
                  检查更新
                </button>
              )}
            </div>
          </section>
        )}
      </div>
    </div>
  );
}

function updateBlurb(update: UpdateState): string {
  switch (update.kind) {
    case "idle":
      return "点「检查更新」看看有没有新版";
    case "checking":
      return "正在检查…";
    case "up-to-date":
      return "已经是最新版";
    case "available":
      return `服务器上有新版 v${update.next}`;
    case "installing":
      return `下载安装 v${update.next} 中…`;
    case "installed":
      return `已安装 v${update.next}，下次启动生效`;
    case "error":
      return `检查失败：${update.message}`;
  }
}

export default CloudPane;
