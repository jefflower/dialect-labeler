/** Display helpers — keep formatting decisions out of the components. */

export function formatBytes(n: number | null | undefined): string {
  if (n == null) return "—";
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 10 ? 0 : 1)} ${units[i]}`;
}

export function formatDuration(ms: number | null | undefined): string {
  if (ms == null) return "—";
  const totalSec = Math.round(ms / 1000);
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  if (h > 0) return `${h}h ${m}m ${s}s`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

export function formatRelative(iso: string | null | undefined): string {
  if (!iso) return "—";
  const t = new Date(iso).getTime();
  if (!Number.isFinite(t)) return iso;
  const delta = Date.now() - t;
  const abs = Math.abs(delta);
  const sec = Math.round(abs / 1000);
  const min = Math.round(sec / 60);
  const hr = Math.round(min / 60);
  const day = Math.round(hr / 24);
  const phrase =
    sec < 60
      ? `${sec}秒`
      : min < 60
      ? `${min}分钟`
      : hr < 48
      ? `${hr}小时`
      : `${day}天`;
  return delta >= 0 ? `${phrase}前` : `${phrase}后`;
}

export function formatStatus(status: string): { label: string; color: string } {
  switch (status) {
    case "pending":
      return { label: "排队中", color: "#888" };
    case "claimed":
      return { label: "已认领", color: "#0078d4" };
    case "running":
      return { label: "处理中", color: "#0078d4" };
    case "succeeded":
      return { label: "成功", color: "#107c10" };
    case "failed":
      return { label: "失败", color: "#d13438" };
    case "expired":
      return { label: "超时", color: "#d13438" };
    default:
      return { label: status, color: "#666" };
  }
}
