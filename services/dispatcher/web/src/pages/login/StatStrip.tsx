/**
 * Stat strip — the small KPIs + live clock bar above the pipeline.
 *
 * Pulls real numbers from `/api/public/stats` (no auth, safe to call
 * pre-login). Fall back to "—" placeholders if the request fails —
 * the strip is informational, never blocking.
 *
 * Refreshes every 30s so a long-idle login page eventually catches
 * up; we don't poll faster than that since these are aggregate
 * numbers, not live-tail.
 */

import { useEffect, useState } from "react";

interface PublicStats {
  active_user_count: number;
  completed_task_count: number;
  processed_audio_seconds: number;
  completed_last_24h: number;
}

function formatHours(seconds: number): string {
  if (!seconds || seconds < 60) return "0h";
  const h = seconds / 3600;
  if (h >= 1000) return `${Math.round(h).toLocaleString()}h`;
  if (h >= 100) return `${h.toFixed(0)}h`;
  if (h >= 10) return `${h.toFixed(1)}h`;
  return `${h.toFixed(2)}h`;
}

function formatTime(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export function StatStrip() {
  const [stats, setStats] = useState<PublicStats | null>(null);
  const [time, setTime] = useState(() => formatTime(new Date()));

  useEffect(() => {
    let cancelled = false;
    const fetchStats = async () => {
      try {
        const resp = await fetch("/api/public/stats");
        if (!resp.ok) return;
        const data = (await resp.json()) as PublicStats;
        if (!cancelled) setStats(data);
      } catch {
        // Silent failure — strip just shows placeholders.
      }
    };
    fetchStats();
    const t = window.setInterval(fetchStats, 30_000);
    return () => {
      cancelled = true;
      window.clearInterval(t);
    };
  }, []);

  useEffect(() => {
    const t = window.setInterval(
      () => setTime(formatTime(new Date())),
      1000,
    );
    return () => window.clearInterval(t);
  }, []);

  // Live data — display "—" while loading so the layout doesn't jump.
  const fmt = (n: number | undefined) =>
    n === undefined ? "—" : n.toLocaleString();

  return (
    <div className="login-stat-strip">
      <Stat label="审核用户" value={fmt(stats?.active_user_count)} />
      <Stat label="累计任务" value={fmt(stats?.completed_task_count)} />
      <Stat label="处理时长" value={formatHours(stats?.processed_audio_seconds ?? 0)} />
      <Stat label="24h 完成" value={fmt(stats?.completed_last_24h)} />
      <div className="login-stat-divider" />
      <div className="login-stat-clock">
        <span className="login-stat-clock-dot" />
        <span>SYS</span>
        <span style={{ color: "var(--text-1)" }}>{time}</span>
      </div>
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="login-stat">
      <div className="login-stat-value">{value}</div>
      <div className="login-stat-label">{label}</div>
    </div>
  );
}
