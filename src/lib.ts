import { invoke } from "@tauri-apps/api/core";
import type {
  AudioFileInfo,
  CutConfig,
  DependencyStatus,
  ExportOptions,
  PlaybackAudio,
  PlaybackState,
  PolishedOutput,
  ProjectFile,
  ProjectScan,
  RecognitionOptions,
  RecognitionResult,
  SegmentRecord,
} from "./types";

export const ipc = {
  scanProjectFolder(args: {
    folderPath: string;
    manifestPath?: string | null;
    outputPath?: string | null;
  }) {
    return invoke<ProjectScan>("scan_project_folder", args);
  },
  cutAudioFile(args: {
    inputPath: string;
    segmentsDir: string;
    config: CutConfig;
    role?: string | null;
    originalText?: string | null;
    emotion?: string[] | null;
  }) {
    return invoke<SegmentRecord[]>("cut_audio_file", args);
  },
  recognizeSegments(args: {
    projectDir: string;
    segments: SegmentRecord[];
    options?: RecognitionOptions;
  }) {
    return invoke<RecognitionResult[]>("recognize_segments", args);
  },
  polishTextWithLlm(args: {
    text: string;
    role?: string | null;
    hint?: string | null;
    ollamaUrl?: string | null;
    ollamaModel?: string | null;
    llmPrompt?: string | null;
  }) {
    return invoke<PolishedOutput>("polish_text_with_llm", args);
  },
  listOllamaModels(args: { url?: string | null }) {
    return invoke<string[]>("list_ollama_models", args);
  },
  checkDependencies(args: { ollamaUrl?: string | null }) {
    return invoke<DependencyStatus>("check_dependencies", args);
  },
  getDefaultLlmPrompt() {
    return invoke<string>("get_default_llm_prompt");
  },
  prepareWaveformPeaks(args: { inputPath: string; bucketCount?: number }) {
    return invoke<number[]>("read_waveform_peaks", args);
  },
  preparePlaybackAudio(args: { inputPath: string; cacheDir: string }) {
    return invoke<PlaybackAudio>("prepare_playback_audio", args);
  },
  playAudio(args: { path: string; positionMs: number }) {
    return invoke<PlaybackState>("play_audio", args);
  },
  pauseAudio() {
    return invoke<PlaybackState>("pause_audio");
  },
  stopAudio() {
    return invoke<PlaybackState>("stop_audio");
  },
  audioState() {
    return invoke<PlaybackState>("audio_state");
  },
  saveProjectFile(args: { projectDir: string; payload: ProjectFile }) {
    return invoke<string>("save_project_file", args);
  },
  loadProjectFile(args: { projectDir: string }) {
    return invoke<ProjectFile>("load_project_file", args);
  },
  exportSegmentsJsonl(args: {
    projectDir: string;
    outputPath?: string | null;
    segments: SegmentRecord[];
    options?: ExportOptions;
  }) {
    return invoke<string>("export_segments_jsonl", args);
  },
};

export function formatClock(value?: number): string {
  if (!value || value <= 0) return "0:00";
  const totalSeconds = Math.floor(value / 1000);
  const min = Math.floor(totalSeconds / 60);
  const sec = String(totalSeconds % 60).padStart(2, "0");
  return `${min}:${sec}`;
}

export function formatDuration(value?: number): string {
  if (!value) return "—";
  return formatClock(value);
}

export function formatMsRange(start: number, end: number): string {
  return `${(start / 1000).toFixed(2)}–${(end / 1000).toFixed(2)}s`;
}

export function formatBytes(value?: number): string {
  if (!value) return "—";
  if (value < 1024) return `${value}B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)}KB`;
  if (value < 1024 * 1024 * 1024) return `${(value / 1024 / 1024).toFixed(1)}MB`;
  return `${(value / 1024 / 1024 / 1024).toFixed(2)}GB`;
}

export function stripTags(value: string): string {
  return value.replace(/<[^>]+>/g, "");
}

export function normalizeRecognizedText(value: string): string {
  return value.replace(/\s+/g, "").trim();
}

export function audioBaseName(audio: AudioFileInfo): string {
  return audio.fileName.replace(/\.[^.]+$/, "");
}

export function waitForPaint(): Promise<void> {
  return new Promise((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
  });
}

export function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, value));
}

export function escapeRegex(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
