import { Navigate, Route, Routes, useLocation } from "react-router-dom";
import { AuthProvider, useAuth } from "./auth";
import AdminDashboardPage from "./pages/AdminDashboardPage";
import AdminReleasesPage from "./pages/AdminReleasesPage";
import AdminUsersPage from "./pages/AdminUsersPage";
import DownloadsPage from "./pages/DownloadsPage";
import LoginPage from "./pages/LoginPage";
import TaskDetailPage from "./pages/TaskDetailPage";
import TasksPage from "./pages/TasksPage";
import Topbar from "./Topbar";

export default function App() {
  return (
    <AuthProvider>
      <Routes>
        <Route path="/login" element={<LoginPage />} />
        {/* Downloads is intentionally public — anyone can grab the client. */}
        <Route path="/downloads" element={<PublicShell><DownloadsPage /></PublicShell>} />
        <Route
          path="/*"
          element={
            <RequireAuth>
              <AppShell />
            </RequireAuth>
          }
        />
      </Routes>
    </AuthProvider>
  );
}

function AppShell() {
  return (
    <div className="shell">
      <Topbar />
      <Routes>
        <Route index element={<Navigate to="/tasks" replace />} />
        <Route path="tasks" element={<TasksPage />} />
        <Route path="tasks/:id" element={<TaskDetailPage />} />
        <Route path="downloads" element={<DownloadsPage />} />
        <Route
          path="admin"
          element={
            <RequireAdmin>
              <AdminDashboardPage />
            </RequireAdmin>
          }
        />
        <Route
          path="admin/users"
          element={
            <RequireAdmin>
              <AdminUsersPage />
            </RequireAdmin>
          }
        />
        <Route
          path="admin/releases"
          element={
            <RequireAdmin>
              <AdminReleasesPage />
            </RequireAdmin>
          }
        />
        <Route path="*" element={<Navigate to="/tasks" replace />} />
      </Routes>
    </div>
  );
}

function PublicShell({ children }: { children: React.ReactNode }) {
  return (
    <div className="shell">
      <header className="topbar">
        <span className="title">方言标注调度</span>
        <div className="grow" />
        <a href="/login">登录</a>
      </header>
      {children}
    </div>
  );
}

function RequireAuth({ children }: { children: React.ReactNode }) {
  const { user, loading } = useAuth();
  const location = useLocation();
  if (loading) {
    return <div className="loader">检查登录中…</div>;
  }
  if (!user) {
    return <Navigate to="/login" state={{ from: location }} replace />;
  }
  return <>{children}</>;
}

function RequireAdmin({ children }: { children: React.ReactNode }) {
  const { user } = useAuth();
  if (user?.role !== "admin") {
    return <Navigate to="/tasks" replace />;
  }
  return <>{children}</>;
}
