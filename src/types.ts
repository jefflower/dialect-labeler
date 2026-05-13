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
  topicId?: number;
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

/**
 * Cutting algorithm.
 *
 * `dialect` — physical silence detection. Fast, deterministic,
 * language-agnostic. Used by the 长沙 / 台湾 dialect workflows. Segment
 * boundaries are driven entirely by audio gaps so they can land
 * mid-thought when the speaker doesn't pause at sentence ends.
 *
 * `semantic` — Mandarin-only, LLM-assisted. Pre-cuts on silence, ASRs
 * every piece, then asks Qwen2.5 (must be on an endpoint with
 * num_ctx ≥32K) to merge the pieces into 6–90s segments that respect
 * complete semantic units. Implements the `录制数据剪辑转写规则（新）`
 * spec § 二·切句规则.
 */
export type CutMode = "dialect" | "semantic";

export type CutConfig = {
  silenceDb: number;
  minSilenceMs: number;
  minSegmentMs: number;
  preRollMs: number;
  postRollMs: number;
  /** Kept for project-file compatibility. The cutter no longer force-splits by length. */
  maxSegmentMs: number;

  // ---- Mode dispatch + semantic-mode-only knobs --------------------------
  // All optional in the type, so old project.json files (saved before
  // Mode 2) still deserialise. Rust side fills defaults with #[serde(default)].
  /** Cutting algorithm; defaults to `dialect`. */
  mode?: CutMode;
  /** Target avg loudness for preprocessing (ITU-R BS.1770). Spec § 七·1: -18 LUFS. */
  targetLoudnessLufs?: number;
  /** Min segment length, seconds. Spec § 二·一·1: 6s. */
  minSegmentS?: number;
  /** Max segment length, seconds. Spec § 二·一·1: 90s. */
  maxSegmentS?: number;
  /** Min head/tail silence pad, ms. Spec § 二·三·1: ≥150ms. */
  headTailSilenceMs?: number;
  /** Pinned Ollama endpoint for the long-context semantic-cut call.
   *  e.g. `http://100.64.0.4:11434`. Must be a box with ≥40GB RAM. */
  semanticEndpoint?: string;
  /** Model name on `semanticEndpoint`. Default `qwen2.5:32b`. */
  semanticModel?: string;
  /** `num_ctx` override sent with the semantic-cut LLM call. Default 32768. */
  semanticNumCtx?: number;

  /** Mode 2's own Ollama LLM pool. Independent from `AppSettings.ollamaExtraEndpoints`
   *  which is semantically owned by Mode 1. Pool is tried in declared
   *  order — first endpoint to return a valid + spec-compliant response
   *  wins. Empty list → falls back to the singular `semanticEndpoint` /
   *  `semanticModel` pair above (legacy form, pre-pool). */
  semanticOllamaEndpoints?: OllamaEndpointDef[];
  /** Mode 2's own Whisper HTTP pool. Independent from
   *  `AppSettings.whisperEndpoints` (Mode 1). Empty → fall back to the
   *  globals carried in RecognitionOptions. Each `WhisperEndpointDef`
   *  must point at a `faster-whisper-server` instance (or compatible
   *  `/transcribe` API). */
  semanticWhisperEndpoints?: WhisperEndpointDef[];
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

export type CutValidationResult = {
  segmentId: string;
  segmentPath: string;
  segmentFileName: string;
  ok: boolean;
  leadingSilenceMs: number;
  trailingSilenceMs: number;
  maxLeadingSilenceMs: number;
  maxTrailingSilenceMs: number;
  thresholdDb: number;
  allSilence: boolean;
  message?: string | null;
};

export type CleanCutNoiseResult = {
  processedCount: number;
  validation: CutValidationResult[];
};

export type RepairCutSilenceResult = {
  processedCount: number;
  updatedSegments: SegmentRecord[];
  validation: CutValidationResult[];
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
  /** Master switch. `false` keeps the entry in config but the dispatcher
   *  skips it (so you can stage an endpoint before its model finishes
   *  pulling, then flip it on). Default / undefined = enabled. */
  enabled?: boolean;
};

/** Remote whisper-server endpoint (see `services/whisper-server/`). */
export type WhisperEndpointDef = {
  url: string;
  /** Master switch — see OllamaEndpointDef.enabled. Default = enabled. */
  enabled?: boolean;
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
  /** Pool of remote `services/whisper-server` endpoints. When non-empty,
   *  the ASR phase POSTs each segment WAV to the pool instead of running
   *  the local `whisper` CLI. Same work-stealing scheduler as the LLM pool. */
  whisperEndpoints?: WhisperEndpointDef[];
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

/** Per-source-file report from `run_semantic_pipeline`. */
export type SemanticPipelineFileReport = {
  sourcePath: string;
  xlsxPath: string;
  segmentCount: number;
  /** Indices of segments where Phase 6 auto-QA flagged at least one issue. */
  qaFlaggedIndices: number[];
};

/** Result of one `run_semantic_pipeline` invocation. The pipeline
 *  processes every input even if some fail; failures land in `errors`
 *  while successes accumulate in `reports`. */
export type SemanticPipelineResult = {
  reports: SemanticPipelineFileReport[];
  errors: string[];
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

/**
 * Bundled set of dialect-specific prompts. One quick-fill for the three
 * fields below: `whisperInitialPrompt` (ASR hint), `llmPrompt` (Ollama
 * polish system text — empty falls back to backend built-in Changsha
 * prompt), `systemPrompt` (JSONL export header).
 *
 * Built-ins (`builtin: true`) can't be edited or deleted via UI; users
 * can clone via "另存为预设…" to make a custom variant.
 */
export type DialectProfile = {
  id: string;
  name: string;
  whisperInitialPrompt: string;
  llmPrompt: string;
  systemPrompt: string;
  builtin?: boolean;
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
  /** Remote whisper-server endpoints (faster-whisper HTTP, see
   *  `services/whisper-server/`). Non-empty → ASR runs in a HTTP work-stealing
   *  pool just like the LLM polish phase, with `whisperConcurrency`
   *  controlling pool size (default 2 × endpoint count). Empty list keeps
   *  the legacy local `whisper` CLI path. */
  whisperEndpoints: WhisperEndpointDef[];
  /** Built-in + user-saved dialect profiles. */
  dialectProfiles: DialectProfile[];
  /** Last-selected profile id (informational — actual prompts live in
   *  the three raw fields above, profile select just quick-fills them). */
  activeDialectProfileId?: string;
};
