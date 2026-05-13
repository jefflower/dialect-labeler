import { FormEvent, useState } from "react";
import { useNavigate } from "react-router-dom";
import { ApiError, login, register } from "../api";
import { useAuth } from "../auth";

/**
 * Login + register on the same screen.
 *
 * Re-skin (May 2026) — moved from a small card-on-grey to a centered
 * panel with a subtle gradient backdrop and a segmented tab switcher
 * between login + register. The dispatcher silently promotes the
 * first registrant to admin, so the same page doubles as the
 * first-time setup flow.
 */
export default function LoginPage() {
  const { applyToken } = useAuth();
  const navigate = useNavigate();
  const [mode, setMode] = useState<"login" | "register">("login");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    setSubmitting(true);
    try {
      const fn = mode === "login" ? login : register;
      const result = await fn(email, password);
      applyToken(result.access_token, result.user);
      navigate("/tasks", { replace: true });
    } catch (err) {
      if (err instanceof ApiError) {
        setError(err.message);
      } else {
        setError(String(err));
      }
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div className="login-shell">
      <div className="login-panel" role="dialog" aria-label="登录或注册">
        <div className="login-brand">
          <div className="login-logo" aria-hidden>
            <span style={{ fontWeight: 800, fontSize: 22 }}>方</span>
          </div>
          <h1>方言标注调度</h1>
          <p className="login-tagline">
            上传 · 自动处理 · 下载产物。
          </p>
        </div>

        <div className="login-tabs" role="tablist" aria-label="登录或注册">
          <button
            type="button"
            role="tab"
            aria-selected={mode === "login"}
            className={mode === "login" ? "active" : ""}
            onClick={() => {
              setMode("login");
              setError(null);
            }}
          >
            登录
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={mode === "register"}
            className={mode === "register" ? "active" : ""}
            onClick={() => {
              setMode("register");
              setError(null);
            }}
          >
            注册
          </button>
        </div>

        <form onSubmit={onSubmit} className="login-form">
          <label htmlFor="email">邮箱</label>
          <input
            id="email"
            type="email"
            autoComplete="email"
            placeholder="you@example.com"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            required
            autoFocus
          />
          <label htmlFor="password">
            密码
            {mode === "register" && (
              <span className="login-hint">至少 8 位</span>
            )}
          </label>
          <input
            id="password"
            type="password"
            autoComplete={mode === "login" ? "current-password" : "new-password"}
            placeholder={mode === "register" ? "≥ 8 位" : "你的密码"}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            required
            minLength={mode === "register" ? 8 : undefined}
          />
          {error && (
            <div className="login-error" role="alert">
              {error}
            </div>
          )}
          <button
            type="submit"
            className="login-submit primary"
            disabled={submitting || !email || !password}
          >
            {submitting ? (
              <span className="login-spinner" aria-hidden />
            ) : mode === "login" ? (
              "登录"
            ) : (
              "创建账号"
            )}
          </button>
        </form>

        <p className="login-footnote">
          {mode === "register"
            ? "首次访问？首位注册者自动成为管理员。"
            : "忘记密码？请联系管理员重置。"}
        </p>
      </div>
    </div>
  );
}
