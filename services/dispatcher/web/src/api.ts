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
    | "expired";
  input_size: number | null;
  input_uploaded_at: string | null;
  output_size: number | null;
  output_ready_at: string | null;
  output_downloaded_at: string | null;
  files_cleaned_at: string | null;
  error: string | null;
  summary: TaskSummary | null;
  created_at: string;
  updated_at: string;
  completed_at: string | null;
}

export interface TokenResponse {
  access_token: string;
  token_type: string;
  user: User;
}

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
  return request<TokenResponse>("/api/auth/register", {
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

export function createTask(name: string, file: File) {
  const fd = new FormData();
  fd.append("name", name);
  fd.append("file", file);
  return request<Task>("/api/tasks", { method: "POST", body: fd });
}

export function deleteTask(id: string) {
  return request<void>(`/api/tasks/${id}`, { method: "DELETE" });
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
