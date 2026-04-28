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
};

export type CutConfig = {
  silenceDb: number;
  minSilenceMs: number;
  minSegmentMs: number;
  preRollMs: number;
  postRollMs: number;
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
};

export type RecognitionResult = {
  segmentId: string;
  text: string;
  rawText: string;
  polished: boolean;
  cached: boolean;
  emotion: string | null;
  tags: string[];
};

export type PolishedOutput = {
  text: string;
  emotion: string | null;
  tags: string[];
};

export type RecognitionOptions = {
  whisperModel?: string;
  useLlm?: boolean;
  ollamaUrl?: string;
  ollamaModel?: string;
  llmPrompt?: string;
  initialPrompt?: string;
  useCache?: boolean;
  overwriteCache?: boolean;
};

export type ExportOptions = {
  systemPrompt?: string;
  pairUserAssistant?: boolean;
  useSourceAudioForUser?: boolean;
  audioFilePrefix?: string;
  inputRoot?: string;
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

export type AppSettings = {
  theme: Theme;
  whisperModel: string;
  whisperInitialPrompt: string;
  useAsrCache: boolean;
  useLlm: boolean;
  ollamaUrl: string;
  ollamaModel: string;
  llmPrompt: string;
  batchSize: number;
  systemPrompt: string;
  pairUserAssistant: boolean;
  audioFilePrefix: string;
};
