import { useCallback, useEffect, useRef, useState } from "react";
import type { AppSettings, Theme, Toast } from "./types";
import { defaultAppSettings, SETTINGS_KEY } from "./defaults";

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
      return { ...defaultAppSettings, ...parsed };
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
