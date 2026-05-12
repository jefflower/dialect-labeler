import { FormEvent, useState } from "react";
import { useNavigate } from "react-router-dom";
import { ApiError, login, register } from "../api";
import { useAuth } from "../auth";

/** One screen that handles both login and registration. The dispatcher
 *  silently promotes the first registrant to admin, so this also doubles
 *  as the first-time setup screen. */
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
    <div className="login-card">
      <h1>方言标注调度</h1>
      <p className="subtitle">
        {mode === "login" ? "登录以管理你的标注任务" : "首次访问？创建账号——首位用户自动成为管理员"}
      </p>
      <form onSubmit={onSubmit}>
        <label htmlFor="email">邮箱</label>
        <input
          id="email"
          type="email"
          autoComplete="email"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          required
          autoFocus
        />
        <label htmlFor="password">密码</label>
        <input
          id="password"
          type="password"
          autoComplete={mode === "login" ? "current-password" : "new-password"}
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          required
          minLength={mode === "register" ? 8 : undefined}
        />
        {error && <div className="error">{error}</div>}
        <div style={{ marginTop: 16 }}>
          <button
            type="submit"
            className="primary"
            disabled={submitting}
            style={{ width: "100%" }}
          >
            {submitting ? "请稍候..." : mode === "login" ? "登录" : "注册"}
          </button>
        </div>
      </form>
      <div className="mode-toggle">
        {mode === "login" ? (
          <>
            还没有账号？
            <button type="button" onClick={() => setMode("register")}>
              注册
            </button>
          </>
        ) : (
          <>
            已有账号？
            <button type="button" onClick={() => setMode("login")}>
              登录
            </button>
          </>
        )}
      </div>
    </div>
  );
}
