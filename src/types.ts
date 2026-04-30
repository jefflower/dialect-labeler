export type ManifestRecord = {
  role: string;
  content: string;
  rawContent: string;
  audioFile?: string;
  emotion: string[];
  tags: string[];
};

export type AudioFileInfo = {
  id: string;
  path: string;
  fileName: string;
  role?: string;
  targetFileNames?: string[];
  durationMs?: number;
  sampleRate?: number;
  channels?: number;
  codecName?: string;
  bitsPerSample?: number;
  matchedText?: string;
  matchedEmotion: string[];
};

export type ProjectScan = {
  rootPath: string;
  projectDir: string;
  segmentsDir: string;
  audioFiles: AudioFileInfo[];
  manifestRecords: ManifestRecord[];
  existingProject?: ProjectFile | null;
};

export type CutConfig = {
  silenceDb: number;
  minSilenceMs: number;
  minSegmentMs: number;
  preRollMs: number;
  postRollMs: number;
  /** Hard upper bound; 0 = no cap. Default 30000 ms aligns with spec. */
  maxSegmentMs: number;
};

export type CutPresetDef = {
  /** Display name. */
  name: string;
  /** True for the 3 built-in presets — UI prevents editing/deleting these. */
  builtin?: boolean;
  /** One-liner shown in the dropdown. */
  hint?: string;
  config: CutConfig;
};

export type SegmentRecord = {
  id: string;
  sourcePath: string;
  sourceFileName: string;
  segmentPath: string;
  segmentFileName: string;
  role?: string;
  startMs: number;
  endMs: number;
  durationMs: number;
  originalText: string;
  phoneticText: string;
  emotion: string[];
  tags: string[];
  notes: string;
  /** Last LLM endpoint that polished this segment — for visibility only. */
  lastPolishEndpoint?: string | null;
  lastPolishModel?: string | null;
};

export type RecognitionResult = {
  segmentId: string;
  text: string;
  rawText: string;
  polished: boolean;
  cached: boolean;
  emotion: string | null;
  tags: string[];
  polishEndpoint?: string | null;
  polishModel?: string | null;
};

export type PolishedOutput = {
  text: string;
  emotion: string | null;
  tags: string[];
};

export type OllamaEndpointDef = {
  url: string;
  /** Model name to use on this endpoint. Empty / undefined → fallback to
   *  the primary `ollamaModel`. */
  model?: string;
};

export type RecognitionOptions = {
  whisperModel?: string;
  useLlm?: boolean;
  /** Primary Ollama endpoint. */
  ollamaUrl?: string;
  /** Legacy URL-only extra endpoints — kept for backwards-compat. Prefer
   *  `ollamaExtraEndpoints` so each endpoint can declare its own model. */
  ollamaExtraUrls?: string[];
  ollamaExtraEndpoints?: OllamaEndpointDef[];
  ollamaModel?: string;
  llmPrompt?: string;
  initialPrompt?: string;
  useCache?: boolean;
  overwriteCache?: boolean;
  /** How many Ollama HTTP calls to fire in parallel across the pool. */
  llmConcurrency?: number;
  /** Number of concurrent Whisper processes. Default 1; raise to 2-3
   *  if you have GPU/RAM headroom and the LLM pool is starving for
   *  input. Each process loads its own model. */
  whisperConcurrency?: number;
};

export type ExportOptions = {
  systemPrompt?: string;
  pairUserAssistant?: boolean;
  useSourceAudioForUser?: boolean;
  audioFilePrefix?: string;
  inputRoot?: string;
};

export type BundleResult = {
  bundleDir: string;
  jsonlPath: string;
  segmentCount: number;
  sourceAudioCount: number;
  totalBytes: number;
};

export type DependencyStatus = {
  whisperOk: boolean;
  whisperPath?: string | null;
  whisperError?: string | null;
  ffmpegOk: boolean;
  ffmpegError?: string | null;
  ollamaOk: boolean;
  ollamaUrl: string;
  ollamaModels: string[];
  ollamaError?: string | null;
};

export type ProjectFile = {
  version: number;
  savedAt: string;
  rootPath: string;
  projectDir: string;
  segmentsDir: string;
  config: CutConfig;
  audioFiles: AudioFileInfo[];
  manifestRecords: ManifestRecord[];
  segments: SegmentRecord[];
  systemPrompt?: string;
};

export type PlaybackAudio = {
  path: string;
  durationMs?: number;
  isPreview: boolean;
};

export type PlaybackState = {
  path?: string;
  positionMs: number;
  durationMs: number;
  isPlaying: boolean;
};

export type ProgressState = {
  visible: boolean;
  label: string;
  detail: string;
  current: number;
  total: number;
  indeterminate?: boolean;
};

export type Toast = {
  id: number;
  variant: "info" | "success" | "warning" | "error";
  title: string;
  detail?: string;
};

export type Theme = "light" | "dark" | "system";

/**
 * Inline tag definition. `kind: "paired"` produces `<tag>...</tag>` wrapping
 * a selection. `kind: "bracket"` produces `[tag]` as a discrete event marker.
 */
export type InlineTagDef = {
  /** Tech name (lowercase ASCII), e.g. "laugh". */
  tag: string;
  /** Display label in Chinese, e.g. "笑着说". */
  label: string;
  /** Single-letter keyboard shortcut. Empty string = no shortcut. */
  key: string;
  /** Wrap behaviour. */
  kind: "paired" | "bracket";
  /** 1-char glyph shown in the AlignedTimeline for bracket markers. */
  glyph?: string;
  /** Tooltip / hint shown next to the toolbar button. */
  hint?: string;
};

/** Segment-level tag definition (written to JSONL `tags`). */
export type SegmentTagDef = {
  value: string;
  label: string;
  key: string;
};

export type AppSettings = {
  theme: Theme;
  whisperModel: string;
  whisperInitialPrompt: string;
  useAsrCache: boolean;
  useLlm: boolean;
  ollamaUrl: string;
  ollamaModel: string;
  llmPrompt: string;
  systemPrompt: string;
  pairUserAssistant: boolean;
  audioFilePrefix: string;
  /** User-editable tag dictionaries — initial values come from `defaults.ts`. */
  inlineTags: InlineTagDef[];
  segmentTags: SegmentTagDef[];
  emotions: string[];
  /** Cut strategy presets (built-in + user-saved). */
  cutPresets: CutPresetDef[];
  /** Parallel Ollama calls per batch (default 2). Set higher with more
   *  endpoints or with `OLLAMA_NUM_PARALLEL` configured on the server. */
  llmConcurrency: number;
  /** Concurrent Whisper processes (default 1). Each loads its own
   *  model — only raise if you have GPU/RAM headroom. */
  whisperConcurrency: number;
  /** Additional Ollama endpoints — primary `ollamaUrl` plus these are
   *  round-robined for the LLM polish step. Each entry may pin its own
   *  model (e.g. local 32b paired with a remote 122b). */
  ollamaExtraEndpoints: OllamaEndpointDef[];
};
