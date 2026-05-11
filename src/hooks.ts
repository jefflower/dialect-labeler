import { useCallback, useEffect, useRef, useState } from "react";
import type { AppSettings, Theme, Toast } from "./types";
import { defaultAppSettings, SETTINGS_KEY } from "./defaults";

const ENDPOINTS_MIGRATION_KEY = "dialect-labeler/migrations/v3-tailnet-endpoints";
const WHISPER_ENDPOINTS_MIGRATION_KEY =
  "dialect-labeler/migrations/v4-whisper-endpoints";

export function useTheme(theme: Theme) {
  useEffect(() => {
    const root = document.documentElement;
    if (theme === "system") {
      root.removeAttribute("data-theme");
    } else {
      root.setAttribute("data-theme", theme);
    }
  }, [theme]);
}

export function useSettings() {
  const [settings, setSettings] = useState<AppSettings>(() => {
    try {
      const raw = localStorage.getItem(SETTINGS_KEY);
      if (!raw) return defaultAppSettings;
      const parsed = JSON.parse(raw);
      const merged = { ...defaultAppSettings, ...parsed };
      // Migrate legacy `ollamaExtraUrls: string[]` → new
      // `ollamaExtraEndpoints: { url, model? }[]`. Each old URL becomes
      // an entry with no model override (uses primary model).
      const legacyUrls: unknown = parsed?.ollamaExtraUrls;
      if (
        Array.isArray(legacyUrls) &&
        legacyUrls.length > 0 &&
        merged.ollamaExtraEndpoints.length === 0
      ) {
        merged.ollamaExtraEndpoints = legacyUrls
          .filter((u): u is string => typeof u === "string" && u.trim() !== "")
          .map((url) => ({ url, model: undefined }));
      }
      // 一次性迁移：把旧的单机 localhost / 老 IP 设置迁到三机 tailnet 池。
      // 只覆盖端点/并发/模型四个字段；prompt、tags、systemPrompt 等保留用户改动。
      if (!localStorage.getItem(ENDPOINTS_MIGRATION_KEY)) {
        merged.ollamaUrl = defaultAppSettings.ollamaUrl;
        merged.ollamaExtraEndpoints = defaultAppSettings.ollamaExtraEndpoints;
        merged.llmConcurrency = defaultAppSettings.llmConcurrency;
        merged.ollamaModel = defaultAppSettings.ollamaModel;
        try {
          localStorage.setItem(ENDPOINTS_MIGRATION_KEY, "1");
        } catch {
          // ignore — 下次还会再跑一次，幂等
        }
      }
      // v4: 引入 whisperEndpoints。旧版本字段不存在 → 用默认双机池填充；
      // 不去碰用户 whisperConcurrency（如有自定义则保留）。
      if (!localStorage.getItem(WHISPER_ENDPOINTS_MIGRATION_KEY)) {
        if (!Array.isArray(merged.whisperEndpoints)) {
          merged.whisperEndpoints = defaultAppSettings.whisperEndpoints;
        }
        try {
          localStorage.setItem(WHISPER_ENDPOINTS_MIGRATION_KEY, "1");
        } catch {
          // ignore
        }
      }
      return merged;
    } catch {
      return defaultAppSettings;
    }
  });

  useEffect(() => {
    try {
      localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
    } catch {
      // ignore quota / privacy errors
    }
  }, [settings]);

  const update = useCallback(
    (patch: Partial<AppSettings>) =>
      setSettings((current) => ({ ...current, ...patch })),
    [],
  );

  const reset = useCallback(() => setSettings(defaultAppSettings), []);

  return { settings, update, reset, setSettings };
}

let toastId = 0;

export function useToasts() {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const timers = useRef<Map<number, number>>(new Map());

  const dismiss = useCallback((id: number) => {
    const handle = timers.current.get(id);
    if (handle) {
      window.clearTimeout(handle);
      timers.current.delete(id);
    }
    setToasts((current) => current.filter((toast) => toast.id !== id));
  }, []);

  const push = useCallback(
    (toast: Omit<Toast, "id">) => {
      const id = ++toastId;
      const next: Toast = { ...toast, id };
      setToasts((current) => [...current, next]);
      const ttl = toast.variant === "error" ? 9000 : 4500;
      const handle = window.setTimeout(() => dismiss(id), ttl);
      timers.current.set(id, handle);
      return id;
    },
    [dismiss],
  );

  useEffect(
    () => () => {
      timers.current.forEach((handle) => window.clearTimeout(handle));
      timers.current.clear();
    },
    [],
  );

  return { toasts, push, dismiss };
}

export function useShortcutOverlay() {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.key === "?" && !isTyping(event.target)) {
        event.preventDefault();
        setOpen((current) => !current);
      } else if (event.key === "Escape") {
        setOpen(false);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);
  return { open, setOpen };
}

export function isTyping(target: EventTarget | null) {
  if (!target || !(target instanceof HTMLElement)) return false;
  return (
    target.tagName === "INPUT" ||
    target.tagName === "TEXTAREA" ||
    target.isContentEditable
  );
}
