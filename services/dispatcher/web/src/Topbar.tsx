import { NavLink, useNavigate } from "react-router-dom";
import { useAuth } from "./auth";

export default function Topbar() {
  const { user, signOut } = useAuth();
  const navigate = useNavigate();
  if (!user) return null;
  return (
    <header className="topbar">
      <span className="title">方言标注调度</span>
      <nav className="topbar-nav">
        <NavLink to="/tasks">任务</NavLink>
        <NavLink to="/downloads">下载</NavLink>
        {user.role === "admin" && (
          <>
            <NavLink to="/admin/users">用户</NavLink>
            <NavLink to="/admin/releases">版本</NavLink>
          </>
        )}
      </nav>
      <div className="grow" />
      <span className="user">
        {user.email}
        <span className="role-chip">
          {user.role === "admin" ? "管理员" : "用户"}
        </span>
      </span>
      <button
        onClick={() => {
          signOut();
          navigate("/login", { replace: true });
        }}
      >
        退出
      </button>
    </header>
  );
}
