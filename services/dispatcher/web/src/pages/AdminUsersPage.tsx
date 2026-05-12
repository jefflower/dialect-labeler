import { useCallback, useEffect, useState } from "react";
import {
  ApiError,
  createUser,
  deleteUser,
  listUsers,
  updateUserRole,
  User,
} from "../api";
import { useAuth } from "../auth";
import { formatRelative } from "../format";

/** Admin-only user management. The owning admin's own row is read-only
 *  on the dangerous controls (role, delete) — the dispatcher will reject
 *  those anyway but it's better UX to gray them out client-side. */
export default function AdminUsersPage() {
  const { user: me } = useAuth();
  const [users, setUsers] = useState<User[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // create form
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [role, setRole] = useState<"admin" | "user">("user");
  const [submitting, setSubmitting] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setUsers(await listUsers());
      setError(null);
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function onCreate(e: React.FormEvent) {
    e.preventDefault();
    setSubmitting(true);
    setError(null);
    try {
      await createUser(email.trim(), password, role);
      setEmail("");
      setPassword("");
      setRole("user");
      await refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    } finally {
      setSubmitting(false);
    }
  }

  async function onToggleRole(u: User) {
    setError(null);
    try {
      await updateUserRole(u.id, u.role === "admin" ? "user" : "admin");
      await refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    }
  }

  async function onDelete(u: User) {
    if (!window.confirm(`Delete ${u.email}?`)) return;
    setError(null);
    try {
      await deleteUser(u.id);
      await refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    }
  }

  return (
    <div className="page">
      <div className="section-head">
        <h2>用户管理</h2>
      </div>

      {error && <div className="error-banner">{error}</div>}

      <form className="card" onSubmit={onCreate} style={{ padding: 16, marginBottom: 18 }}>
        <h3 style={{ marginTop: 0 }}>新建账号</h3>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 140px auto", gap: 10 }}>
          <input
            type="email"
            placeholder="邮箱"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            required
          />
          <input
            type="password"
            placeholder="密码（≥8 字符）"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            minLength={8}
            required
          />
          <select value={role} onChange={(e) => setRole(e.target.value as "admin" | "user")}>
            <option value="user">用户</option>
            <option value="admin">管理员</option>
          </select>
          <button className="primary" type="submit" disabled={submitting}>
            {submitting ? "创建中…" : "创建"}
          </button>
        </div>
      </form>

      <div className="card">
        {users === null ? (
          <div className="loader">加载中…</div>
        ) : (
          <table className="tasks">
            <thead>
              <tr>
                <th>邮箱</th>
                <th>角色</th>
                <th>创建</th>
                <th>上次登录</th>
                <th style={{ textAlign: "right" }}>操作</th>
              </tr>
            </thead>
            <tbody>
              {users.map((u) => {
                const isSelf = me?.id === u.id;
                return (
                  <tr key={u.id}>
                    <td>{u.email}</td>
                    <td>
                      <span
                        className="status-chip"
                        style={{
                          background: u.role === "admin" ? "#0078d4" : "#888",
                        }}
                      >
                        {u.role === "admin" ? "管理员" : "用户"}
                      </span>
                    </td>
                    <td>{formatRelative(u.created_at)}</td>
                    <td>{u.last_login_at ? formatRelative(u.last_login_at) : "—"}</td>
                    <td style={{ textAlign: "right" }}>
                      <button
                        onClick={() => onToggleRole(u)}
                        disabled={isSelf}
                        title={isSelf ? "无法修改自己的角色" : ""}
                      >
                        切换角色
                      </button>{" "}
                      <button
                        className="danger"
                        onClick={() => onDelete(u)}
                        disabled={isSelf}
                        title={isSelf ? "无法删除自己" : ""}
                      >
                        删除
                      </button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
