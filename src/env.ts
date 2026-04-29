/**
 * Build-time feature flags driven by Vite env vars.
 *
 * `REVIEW_ONLY` produces a slimmed-down build that hides every model-related
 * UI (Whisper, Ollama, recognition / repolish buttons, prompt editor,
 * dependency check, cut parameters). The reviewer just opens a folder that
 * was already processed on the macOS workstation, edits annotations, and
 * exports. Set via `VITE_REVIEW_ONLY=1` at build time (see the Windows GH
 * Action workflow).
 */
export const REVIEW_ONLY: boolean = import.meta.env.VITE_REVIEW_ONLY === "1";

/** Build channel label shown in the topbar — "审核版" in REVIEW mode. */
export const BUILD_CHANNEL: string = REVIEW_ONLY ? "审核版" : "完整版";
