import { invoke } from "@tauri-apps/api/core";
import type {
  AudioFileInfo,
  BundleResult,
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
  /** Request the in-flight `recognize_segments` run to stop. Whisper /
   *  LLM workers exit at their next loop iteration; the in-flight task
   *  finishes naturally (5–10s) before exit. Already-completed segments
   *  remain cached/persisted, so you can re-launch later and resume. */
  cancelRecognize() {
    return invoke<void>("cancel_recognize");
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
  /** Change the playback rate. 1.0 = normal; common review-fast modes
   *  are 1.25 / 1.5. Backend clamps to [0.25, 4.0]. */
  setPlaybackSpeed(args: { speed: number }) {
    return invoke<PlaybackState>("set_playback_speed", args);
  },
  saveProjectFile(args: { projectDir: string; payload: ProjectFile }) {
    return invoke<string>("save_project_file", args);
  },
  backupProjectFile(args: { projectDir: string }) {
    return invoke<string | null>("backup_project_file", args);
  },
  /** Migrate already-cut segment WAVs to the demo `<base>_<NN>_<role>.wav`
   *  naming. Renames files on disk + returns updated SegmentRecords with
   *  new segmentPath/segmentFileName. Caller persists the result. */
  migrateSegmentFilenames(args: {
    projectDir: string;
    segments: SegmentRecord[];
  }) {
    return invoke<SegmentRecord[]>("migrate_segment_filenames", args);
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
  exportDatasetBundle(args: {
    bundleDir: string;
    segments: SegmentRecord[];
    options?: ExportOptions;
    includeSourceAudio?: boolean;
  }) {
    return invoke<BundleResult>("export_dataset_bundle", args);
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

/** Format a millisecond duration in `HH:MM:SS` (or `MM:SS` for clips
 *  under an hour). Used for total/aggregate timings where minutes
 *  alone aren't enough resolution. */
export function formatLongDuration(value?: number): string {
  if (!value || value <= 0) return "0:00";
  const totalSeconds = Math.floor(value / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  if (hours > 0) {
    return `${hours}:${pad(minutes)}:${pad(seconds)}`;
  }
  return `${minutes}:${pad(seconds)}`;
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
