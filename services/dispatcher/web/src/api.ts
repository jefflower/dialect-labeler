/**
 * Typed wrapper over `fetch` for the dispatcher API.
 *
 * Token lives in localStorage under `dispatcher.token` so it survives
 * page reloads but doesn't bleed across browser profiles. Every request
 * attaches it as `Authorization: Bearer …` if present.
 */

const TOKEN_KEY = "dispatcher.token";

export interface User {
  id: number;
  email: string;
  role: "admin" | "user";
  /** Admin-approval gate. Self-registered users start with
   *  `is_approved=false` and can't log in until an admin flips this
   *  true via POST /api/users/{id}/approve. Defaults to `true` when
   *  the field is missing (old client / old dispatcher response). */
  is_approved: boolean;
  approved_at: string | null;
  approved_by_id: number | null;
  created_at: string;
  last_login_at: string | null;
}

export interface TaskSummary {
  segment_count?: number;
  duration_by_role?: Record<string, number>;
  source_files?: number;
  asr_engine?: string;
  llm_endpoints?: string[];
  elapsed_ms?: number;
  [k: string]: unknown;
}

/** Cut algorithm picked at upload time. Mirrors the Tauri client's
 *  CutMode union; the dispatcher stores it on the Task row and feeds
 *  it back to the Worker so the user's choice is authoritative. */
export type TaskMode = "dialect" | "semantic";

export interface Task {
  id: string;
  owner_id: number;
  name: string;
  status:
    | "pending"
    | "claimed"
    | "running"
    | "succeeded"
    | "failed"
    | "expired"
    | "closed";
  /** Defaults to "dialect" when the column is absent (older dispatcher). */
  mode: TaskMode;
  input_size: number | null;
  input_uploaded_at: string | null;
  output_size: number | null;
  output_ready_at: string | null;
  output_downloaded_at: string | null;
  files_cleaned_at: string | null;
  error: string | null;
  summary: TaskSummary | null;
  /** Worker-pushed progress snapshot. NULL until the first push lands. */
  progress_percent: number | null;
  progress_stage: string | null;
  progress_detail: string | null;
  progress_updated_at: string | null;
  created_at: string;
  updated_at: string;
  completed_at: string | null;
  claimer_email: string | null;
  claim_expires_at: string | null;
}

export interface TokenResponse {
  access_token: string;
  token_type: string;
  user: User;
}

/**
 * Response shape when self-registration succeeded but the user is in
 * the admin-approval queue. No token issued — the user has to come
 * back after the admin signs off. We use a discriminator on `status`
 * so callers can `if ("access_token" in body)` and pick the right
 * branch.
 */
export interface RegisterPendingResponse {
  status: "pending_approval";
  detail: string;
  user: User;
}

export type RegisterResponse = TokenResponse | RegisterPendingResponse;

export function getToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setToken(token: string | null): void {
  if (token) localStorage.setItem(TOKEN_KEY, token);
  else localStorage.removeItem(TOKEN_KEY);
}

export class ApiError extends Error {
  status: number;
  body: unknown;
  constructor(status: number, body: unknown, message?: string) {
    super(message ?? `HTTP ${status}`);
    this.status = status;
    this.body = body;
  }
}

async function request<T>(
  path: string,
  options: RequestInit = {},
  parseJson = true
): Promise<T> {
  const headers = new Headers(options.headers);
  const token = getToken();
  if (token) headers.set("Authorization", `Bearer ${token}`);
  // Don't force application/json when caller is sending FormData — the
  // browser needs to set the multipart boundary itself.
  if (
    options.body &&
    !(options.body instanceof FormData) &&
    !headers.has("Content-Type")
  ) {
    headers.set("Content-Type", "application/json");
  }

  const resp = await fetch(path, { ...options, headers });
  // Sliding session: backend rotates the JWT on aged requests via this
  // header. Pick it up before checking resp.ok so the new token survives
  // even on 4xx responses (e.g., 409 conflicts on an authenticated POST).
  const refreshed = resp.headers.get("X-Refreshed-Token");
  if (refreshed) {
    setToken(refreshed);
  }
  if (!resp.ok) {
    let body: unknown = null;
    try {
      body = await resp.json();
    } catch {
      try {
        body = await resp.text();
      } catch {
        body = null;
      }
    }
    const detail =
      typeof body === "object" && body && "detail" in body
        ? String((body as { detail: unknown }).detail)
        : undefined;
    throw new ApiError(resp.status, body, detail);
  }
  if (resp.status === 204) return undefined as T;
  if (!parseJson) return (await resp.blob()) as unknown as T;
  return (await resp.json()) as T;
}

// ---- Auth -------------------------------------------------------------
export function register(email: string, password: string) {
  return request<RegisterResponse>("/api/auth/register", {
    method: "POST",
    body: JSON.stringify({ email, password }),
  });
}

export function login(email: string, password: string) {
  return request<TokenResponse>("/api/auth/login", {
    method: "POST",
    body: JSON.stringify({ email, password }),
  });
}

export function me() {
  return request<User>("/api/auth/me");
}

// ---- Tasks ------------------------------------------------------------
export function listTasks() {
  return request<Task[]>("/api/tasks");
}

export function getTask(id: string) {
  return request<Task>(`/api/tasks/${id}`);
}

export function createTask(name: string, file: File, mode: TaskMode = "dialect") {
  const fd = new FormData();
  fd.append("name", name);
  fd.append("file", file);
  fd.append("mode", mode);
  return request<Task>("/api/tasks", { method: "POST", body: fd });
}

export function deleteTask(id: string) {
  return request<void>(`/api/tasks/${id}`, { method: "DELETE" });
}

export function retryTask(id: string) {
  return request<Task>(`/api/tasks/${id}/retry`, { method: "POST" });
}

/** Returns the download URL — we let the browser handle the actual GET
 *  through an <a download> link so the Save dialog opens. The token is
 *  embedded as a query string only if needed (it isn't here — the
 *  browser sends Authorization headers on same-origin fetches we control).
 *  Caller is expected to use `downloadOutput` for the fetch+blob flow. */
export async function downloadOutput(task: Task): Promise<void> {
  const blob = await request<Blob>(
    `/api/tasks/${task.id}/output`,
    { method: "GET" },
    /* parseJson */ false
  );
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `${task.name || task.id}.zip`;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

// ---- Admin: users -----------------------------------------------------
export function listUsers() {
  return request<User[]>("/api/users");
}
export function createUser(email: string, password: string, role: "admin" | "user") {
  return request<User>("/api/users", {
    method: "POST",
    body: JSON.stringify({ email, password, role }),
  });
}
export function updateUserRole(userId: number, role: "admin" | "user") {
  return request<User>(`/api/users/${userId}`, {
    method: "PATCH",
    body: JSON.stringify({ role }),
  });
}
export function deleteUser(userId: number) {
  return request<void>(`/api/users/${userId}`, { method: "DELETE" });
}

/**
 * Flip a pending account's `is_approved` to true so the user can log
 * in. Idempotent — calling on an already-approved account is a no-op
 * returning 200 with the current row. The dispatcher prefers this
 * over a 409 conflict so the UI can be tolerant of double-clicks.
 */
export function approveUser(userId: number) {
  return request<User>(`/api/users/${userId}/approve`, { method: "POST" });
}

// ---- Admin: releases --------------------------------------------------
export interface ReleaseRow {
  id: number;
  version: string;
  channel: "stable" | "beta";
  target: string;
  notes: string | null;
  installer_size: number | null;
  has_signature: boolean;
  created_at: string;
}

export function listReleases() {
  return request<ReleaseRow[]>("/api/releases");
}

export function uploadRelease(args: {
  version: string;
  target: string;
  channel: string;
  file: File;
  signature?: string;
  notes?: string;
}) {
  const fd = new FormData();
  fd.append("version", args.version);
  fd.append("target", args.target);
  fd.append("channel", args.channel);
  fd.append("file", args.file);
  if (args.signature) fd.append("signature", args.signature);
  if (args.notes) fd.append("notes", args.notes);
  return request<ReleaseRow>("/api/releases", { method: "POST", body: fd });
}

export function deleteRelease(id: number) {
  return request<void>(`/api/releases/${id}`, { method: "DELETE" });
}

// ---- Public: latest download -----------------------------------------
export interface LatestRelease {
  version: string;
  target: string;
  channel: string;
  notes: string | null;
  download_url: string;
  installer_size: number | null;
  pub_date: string;
}

export function getLatestRelease(target = "windows-x86_64") {
  // Public endpoint — no auth required
  return fetch(
    `/api/releases/latest?target=${encodeURIComponent(target)}`
  ).then(async (r) => {
    if (!r.ok) throw new ApiError(r.status, await r.text().catch(() => null));
    return (await r.json()) as LatestRelease;
  });
}

// ---- Admin: stats -----------------------------------------------------
export interface AdminStats {
  task_counts: Record<string, number>;
  queue_depth: number;
  in_flight: number;
  succeeded_24h: number;
  failed_24h: number;
  total_user_count: number;
  total_admin_count: number;
  /** Self-registered users waiting for admin approval. UI surfaces
   *  this as a "你有 N 个待审核账号" badge on the dashboard. */
  pending_user_count: number;
  storage: {
    inputs_bytes: number;
    inputs_files: number;
    outputs_bytes: number;
    outputs_files: number;
    releases_bytes: number;
    releases_files: number;
  };
}

export function getAdminStats() {
  return request<AdminStats>("/api/admin/stats");
}
