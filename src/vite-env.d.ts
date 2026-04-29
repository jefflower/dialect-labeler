/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** "1" enables the slimmed reviewer build (no Whisper / Ollama UI). */
  readonly VITE_REVIEW_ONLY?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
