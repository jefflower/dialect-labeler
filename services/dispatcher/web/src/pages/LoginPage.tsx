import { FormEvent, useState } from "react";
import { useNavigate } from "react-router-dom";
import { ApiError, login, register } from "../api";
import { useAuth } from "../auth";
import { Logo } from "./login/Logo";
import { Pipeline } from "./login/Pipeline";
import { StatStrip } from "./login/StatStrip";
import { Waveform } from "./login/Waveform";

/**
 * Login + register on the same screen — split-pane reskin (May 2026).
 *
 * Layout:
 *   - LEFT  (1.35fr): brand + tagline + Pipeline animation + StatStrip
 *   - RIGHT (1fr):    auth form (tabs: 登录 / 注册)
 *   - Background: radial gradient + grid overlay + waveform along the
 *     bottom; corner markers on the right pane for the "industrial
 *     console" look.
 *
 * Functional behaviour:
 *   - Bootstrap admin (first ever registrant) is auto-approved and
 *     handed a token immediately — same as before.
 *   - Subsequent self-registrations get a `RegisterPendingResponse`;
 *     we surface a green-toned banner asking them to wait for admin
 *     approval, flip the tab back to "登录", and pre-fill nothing.
 *   - Login 403 from the dispatcher (pending account or other policy
 *     reject) gets shown verbatim — the error message is already in
 *     Chinese on the server side.
 *
 * Design source: `AI 工作台登录页面设计.zip` → app.jsx + login-form.jsx.
 * Styles live in `styles.css` under the `.login-shell` scope so they
 * don't bleed into the rest of the SPA.
 */
const PENDING_BANNER =
  "账号已创建，正在等待管理员审核。审核通过后才能登录。";

export default function LoginPage() {
  const { applyToken } = useAuth();
  const navigate = useNavigate();
  const [mode, setMode] = useState<"login" | "register">("login");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [showPwd, setShowPwd] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    setInfo(null);
    setSubmitting(true);
    try {
      if (mode === "login") {
        const result = await login(email, password);
        applyToken(result.access_token, result.user);
        navigate("/tasks", { replace: true });
      } else {
        const result = await register(email, password);
        if ("access_token" in result) {
          // Bootstrap admin path: auto-approved, has a token.
          applyToken(result.access_token, result.user);
          navigate("/tasks", { replace: true });
        } else {
          // Pending path: show banner, flip back to login tab.
          setInfo(result.detail || PENDING_BANNER);
          setMode("login");
          setPassword("");
        }
      }
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div className="login-shell">
      {/* Background layers — purely decorative, behind everything. */}
      <Waveform bars={72} />
      <div className="login-grid-overlay" aria-hidden="true" />

      <div className="login-split">
        {/* ===== LEFT: brand + pipeline + KPIs ===== */}
        <section className="login-pane-left">
          <header className="login-pane-left-header">
            <Logo />
            <div className="login-env-badge">
              <span className="login-env-dot" />
              <span>CLOUD · PRODUCTION</span>
            </div>
          </header>

          <div className="login-pane-left-body">
            <div className="login-tagline">
              <div className="login-tagline-mono">
                // 方言语料 · 切割 · AI 预处理 · 回流标注
              </div>
              <h2 className="login-tagline-title">
                从原始压缩包到可标注语料，
                <br />
                <span className="login-tagline-accent">
                  让分布式算力替你跑通中间的每一步。
                </span>
              </h2>
              <p className="login-tagline-desc">
                平台接收语音压缩包并生成待处理任务，分布式客户端节点消费、切片并执行
                AI 预处理，结构化产物自动回流至工作台，标注员只需专注核心判断。
              </p>
            </div>

            <Pipeline />
            <StatStrip />
          </div>

          <footer className="login-pane-left-footer">
            <span>© 2026 方言工坊 · 内部使用</span>
            <span style={{ fontFamily: "var(--font-mono)" }}>
              build · 2026.05.13 · stable
            </span>
          </footer>
        </section>

        {/* ===== RIGHT: auth form ===== */}
        <section className="login-pane-right">
          <div className="login-pane-right-inner">
            <div className="login-form-heading">
              <div className="login-form-eyebrow">
                <span className="login-form-eyebrow-dot" />
                <span>WELCOME · v2.4.0</span>
              </div>
              <h1 className="login-form-title">
                让 <span className="login-form-title-accent">AI</span>
                训练
                <br />
                更简单
              </h1>
              <p className="login-form-subtitle">
                方言语料的切割、AI 预处理与回流标注，
                <br />
                一个工作台搞定全流程。
              </p>
            </div>

            <div className="login-form-card">
              {/* Tabs — login / register. The indicator slides between
                  the two halves; this is the same shape as the design
                  prototype's 个人/企业 toggle, just relabeled. */}
              <div className="login-tabs" role="tablist">
                <button
                  type="button"
                  role="tab"
                  aria-selected={mode === "login"}
                  className={`login-tab ${mode === "login" ? "active" : ""}`}
                  onClick={() => {
                    setMode("login");
                    setError(null);
                  }}
                >
                  登 录
                </button>
                <button
                  type="button"
                  role="tab"
                  aria-selected={mode === "register"}
                  className={`login-tab ${mode === "register" ? "active" : ""}`}
                  onClick={() => {
                    setMode("register");
                    setError(null);
                    setInfo(null);
                  }}
                >
                  申请使用
                </button>
                <div
                  className="login-tab-indicator"
                  style={{
                    transform:
                      mode === "login"
                        ? "translateX(0%)"
                        : "translateX(100%)",
                  }}
                />
              </div>

              <form onSubmit={onSubmit} className="login-form">
                <Field
                  label="邮箱"
                  value={email}
                  onChange={setEmail}
                  placeholder="you@example.com"
                  type="email"
                  icon={<UserIcon />}
                  autoFocus
                  autoComplete="email"
                />
                <Field
                  label={mode === "register" ? "密码（≥ 8 位）" : "密码"}
                  value={password}
                  onChange={setPassword}
                  placeholder={
                    mode === "register" ? "至少 8 位" : "请输入密码"
                  }
                  type={showPwd ? "text" : "password"}
                  icon={<LockIcon />}
                  autoComplete={
                    mode === "login" ? "current-password" : "new-password"
                  }
                  minLength={mode === "register" ? 8 : undefined}
                  suffix={
                    <button
                      type="button"
                      onClick={() => setShowPwd((s) => !s)}
                      className="login-field-suffix-btn"
                      aria-label="切换密码可见"
                    >
                      {showPwd ? <EyeOpenIcon /> : <EyeClosedIcon />}
                    </button>
                  }
                />

                {info && (
                  <div
                    className="login-banner login-banner-info"
                    role="status"
                  >
                    <span className="login-banner-dot" />
                    <span>{info}</span>
                  </div>
                )}
                {error && (
                  <div
                    className="login-banner login-banner-error"
                    role="alert"
                  >
                    <span className="login-banner-dot" />
                    <span>{error}</span>
                  </div>
                )}

                <button
                  type="submit"
                  disabled={submitting || !email || !password}
                  className={`login-submit ${submitting ? "loading" : ""}`}
                >
                  <span className="login-submit-bg" />
                  <span className="login-submit-content">
                    {submitting ? (
                      <>
                        <SpinnerIcon />
                        {mode === "login" ? "正在登录…" : "正在提交…"}
                      </>
                    ) : mode === "login" ? (
                      <>
                        登 录
                        <ArrowIcon />
                      </>
                    ) : (
                      <>
                        提交申请
                        <ArrowIcon />
                      </>
                    )}
                  </span>
                </button>
              </form>

              <div className="login-form-footer">
                <span style={{ color: "var(--text-3)" }}>
                  {mode === "login" ? "没有账号？" : "已有账号？"}
                </span>
                <a
                  href="#"
                  className="login-link"
                  onClick={(e) => {
                    e.preventDefault();
                    setMode(mode === "login" ? "register" : "login");
                    setError(null);
                    setInfo(null);
                  }}
                >
                  {mode === "login" ? "申请使用" : "去登录"}
                </a>
                <span className="login-form-footer-divider" />
                <a
                  href="/downloads"
                  className="login-link"
                  onClick={(e) => {
                    // SPA-internal route — let react-router handle it.
                    if (e.metaKey || e.ctrlKey) return;
                    e.preventDefault();
                    navigate("/downloads");
                  }}
                >
                  下载工作台客户端
                </a>
              </div>

              <p className="login-form-footnote">
                {mode === "register"
                  ? "首位注册者自动成为管理员；其他注册需管理员审核通过后方可登录。"
                  : "忘记密码？请联系管理员重置。"}
              </p>
            </div>
          </div>

          <CornerMark pos="tr" />
          <CornerMark pos="br" />
        </section>
      </div>
    </div>
  );
}

// ---------- Form field ----------

interface FieldProps {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  type?: string;
  icon?: React.ReactNode;
  suffix?: React.ReactNode;
  autoFocus?: boolean;
  autoComplete?: string;
  minLength?: number;
}

function Field({
  label,
  value,
  onChange,
  placeholder,
  type = "text",
  icon,
  suffix,
  autoFocus,
  autoComplete,
  minLength,
}: FieldProps) {
  const [focused, setFocused] = useState(false);
  return (
    <div className="login-field">
      <label className="login-field-label">{label}</label>
      <div
        className={`login-field-box ${focused ? "focused" : ""}`}
      >
        {icon && <span className="login-field-icon">{icon}</span>}
        <input
          type={type}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
          className="login-field-input"
          autoFocus={autoFocus}
          autoComplete={autoComplete}
          minLength={minLength}
          required
          spellCheck={false}
        />
        {suffix}
      </div>
    </div>
  );
}

// ---------- Decorative corner brackets ----------

function CornerMark({ pos }: { pos: "tr" | "br" }) {
  const map: Record<typeof pos, React.CSSProperties> = {
    tr: { top: 24, right: 24, transform: "rotate(90deg)" },
    br: { bottom: 24, right: 24, transform: "rotate(180deg)" },
  };
  return <span className="login-corner-mark" style={map[pos]} />;
}

// ---------- Icons ----------

function UserIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none">
      <circle cx="12" cy="8" r="4" stroke="currentColor" strokeWidth="1.5" />
      <path
        d="M4 21c0-4.4 3.6-8 8-8s8 3.6 8 8"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  );
}
function LockIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none">
      <rect
        x="4"
        y="10"
        width="16"
        height="11"
        rx="2"
        stroke="currentColor"
        strokeWidth="1.5"
      />
      <path
        d="M8 10V7a4 4 0 1 1 8 0v3"
        stroke="currentColor"
        strokeWidth="1.5"
      />
    </svg>
  );
}
function EyeOpenIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none">
      <path
        d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z"
        stroke="currentColor"
        strokeWidth="1.5"
      />
      <circle cx="12" cy="12" r="3" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  );
}
function EyeClosedIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none">
      <path
        d="M3 3l18 18"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
      <path
        d="M10.6 6.1A10.7 10.7 0 0 1 12 6c6.5 0 10 6 10 6a17.4 17.4 0 0 1-3.4 4M6.5 7.5A17.5 17.5 0 0 0 2 12s3.5 6 10 6a10.5 10.5 0 0 0 4.5-1"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  );
}
function ArrowIcon() {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      style={{ marginLeft: 6 }}
    >
      <path
        d="M5 12h14M13 5l7 7-7 7"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
function SpinnerIcon() {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      style={{
        marginRight: 6,
        animation: "login-spin 0.8s linear infinite",
      }}
    >
      <path
        d="M12 3a9 9 0 1 1-9 9"
        stroke="currentColor"
        strokeWidth="2.4"
        strokeLinecap="round"
      />
    </svg>
  );
}
