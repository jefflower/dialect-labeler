import { useEffect, useMemo, useRef, useState } from "react";
import type { MouseEvent as ReactMouseEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  ArrowLeft,
  Download,
  FileJson,
  FolderOpen,
  Pause,
  Play,
  Save,
  Scissors,
  ScanSearch,
  Smile,
  Tags,
  Volume2,
  WandSparkles,
  Wind,
} from "lucide-react";
import "./App.css";

type ManifestRecord = {
  role: string;
  content: string;
  rawContent: string;
  audioFile?: string;
  emotion: string[];
  tags: string[];
};

type AudioFileInfo = {
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

type ProjectScan = {
  rootPath: string;
  projectDir: string;
  segmentsDir: string;
  audioFiles: AudioFileInfo[];
  manifestRecords: ManifestRecord[];
};

type CutConfig = {
  silenceDb: number;
  minSilenceMs: number;
  minSegmentMs: number;
  preRollMs: number;
  postRollMs: number;
};

type SegmentRecord = {
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

type ProjectFile = {
  version: number;
  savedAt: string;
  rootPath: string;
  projectDir: string;
  segmentsDir: string;
  config: CutConfig;
  audioFiles: AudioFileInfo[];
  manifestRecords: ManifestRecord[];
  segments: SegmentRecord[];
};

type ProgressState = {
  visible: boolean;
  label: string;
  detail: string;
  current: number;
  total: number;
  indeterminate?: boolean;
};

type PlaybackAudio = {
  path: string;
  durationMs?: number;
  isPreview: boolean;
};

type PlaybackState = {
  path?: string;
  positionMs: number;
  durationMs: number;
  isPlaying: boolean;
};

type RecognitionResult = {
  segmentId: string;
  text: string;
};

const defaultConfig: CutConfig = {
  silenceDb: -35,
  minSilenceMs: 450,
  minSegmentMs: 300,
  preRollMs: 100,
  postRollMs: 200,
};

const tagOptions = [
  { value: "laugh", label: "笑声", key: "L" },
  { value: "breath", label: "呼吸", key: "B" },
  { value: "pause", label: "停顿", key: "P" },
  { value: "unclear", label: "听不清", key: "U" },
  { value: "noise", label: "噪声", key: "N" },
];

const inlineTags = [
  { tag: "laugh", label: "笑声" },
  { tag: "breath", label: "呼吸" },
  { tag: "pause", label: "停顿" },
  { tag: "unclear", label: "听不清" },
];

const emotionOptions = ["中立", "开心", "惊讶", "疑问", "生气", "难过"];

const roleLabels: Record<string, string> = {
  user: "陪聊",
  assistant: "发音人",
  unknown: "未知",
};

const tagIcons: Record<string, typeof Smile> = {
  laugh: Smile,
  breath: Wind,
  pause: Tags,
  unclear: Tags,
};

function App() {
  const [folderPath, setFolderPath] = useState("");
  const [outputPath, setOutputPath] = useState("");
  const [exportJsonlPath, setExportJsonlPath] = useState("");
  const [scan, setScan] = useState<ProjectScan | null>(null);
  const [config, setConfig] = useState<CutConfig>(defaultConfig);
  const [segments, setSegments] = useState<SegmentRecord[]>([]);
  const [selectedAudioId, setSelectedAudioId] = useState("");
  const [selectedSegmentId, setSelectedSegmentId] = useState("");
  const [annotationSegmentId, setAnnotationSegmentId] = useState("");
  const [autoCutAfterScan, setAutoCutAfterScan] = useState(true);
  const [autoRecognizeAfterCut, setAutoRecognizeAfterCut] = useState(false);
  const [autoPlayOnReady, setAutoPlayOnReady] = useState(false);
  const [status, setStatus] = useState("请选择音频文件夹");
  const [progress, setProgress] = useState<ProgressState>({
    visible: false,
    label: "",
    detail: "",
    current: 0,
    total: 0,
  });
  const [playbackPath, setPlaybackPath] = useState("");
  const [playbackStatus, setPlaybackStatus] = useState("");
  const [playbackDurationMs, setPlaybackDurationMs] = useState(0);
  const [playbackCurrentMs, setPlaybackCurrentMs] = useState(0);
  const [isPlaying, setIsPlaying] = useState(false);
  const [busy, setBusy] = useState(false);
  const annotationTextRef = useRef<HTMLTextAreaElement | null>(null);

  const selectedAudio = useMemo(
    () => scan?.audioFiles.find((audio) => audio.id === selectedAudioId) ?? null,
    [scan, selectedAudioId],
  );

  const selectedSegment = useMemo(
    () => segments.find((segment) => segment.id === selectedSegmentId) ?? null,
    [segments, selectedSegmentId],
  );

  const annotationSegment = useMemo(
    () => segments.find((segment) => segment.id === annotationSegmentId) ?? null,
    [segments, annotationSegmentId],
  );

  const visibleSegments = useMemo(() => {
    if (!selectedAudio) return segments;
    return segments.filter((segment) => segment.sourcePath === selectedAudio.path);
  }, [segments, selectedAudio]);

  const playbackTargetPath = annotationSegment?.segmentPath ?? selectedAudio?.path ?? "";

  const activeDurationMs =
    playbackDurationMs ||
    (annotationSegment ? annotationSegment.durationMs : selectedAudio?.durationMs) ||
    0;

  const segmentStats = useMemo(() => {
    const done = visibleSegments.filter((segment) => segment.phoneticText.trim()).length;
    return { total: visibleSegments.length, done };
  }, [visibleSegments]);

  useEffect(() => {
    let cancelled = false;

    async function preparePlayback() {
      if (!playbackTargetPath || !scan) {
        setPlaybackPath("");
        setPlaybackStatus("");
        setPlaybackDurationMs(0);
        setPlaybackCurrentMs(0);
        setIsPlaying(false);
        return;
      }

      await invoke("stop_audio").catch(() => undefined);
      setIsPlaying(false);
      setPlaybackStatus("正在准备预听音频");
      setPlaybackCurrentMs(0);
      try {
        const playback = await invoke<PlaybackAudio>("prepare_playback_audio", {
          inputPath: playbackTargetPath,
          cacheDir: scan.projectDir,
        });
        if (cancelled) return;
        setPlaybackPath(playback.path);
        setPlaybackDurationMs(playback.durationMs ?? 0);
        setPlaybackStatus(playback.isPreview ? "已生成预听副本" : "");
        if (autoPlayOnReady) {
          setAutoPlayOnReady(false);
          const state = await invoke<PlaybackState>("play_audio", {
            path: playback.path,
            positionMs: 0,
          });
          if (!cancelled) {
            applyPlaybackState(state);
          }
        }
      } catch (error) {
        if (cancelled) return;
        setPlaybackPath("");
        setPlaybackStatus(`预听失败：${String(error)}`);
      }
    }

    void preparePlayback();
    return () => {
      cancelled = true;
    };
  }, [playbackTargetPath, scan?.projectDir]);

  useEffect(() => {
    if (!isPlaying) return;
    const timer = window.setInterval(() => {
      void invoke<PlaybackState>("audio_state")
        .then(applyPlaybackState)
        .catch((error) => setPlaybackStatus(`播放状态失败：${String(error)}`));
    }, 160);
    return () => window.clearInterval(timer);
  }, [isPlaying]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const isTyping =
        target?.tagName === "INPUT" ||
        target?.tagName === "TEXTAREA" ||
        target?.isContentEditable;

      if (annotationSegment && event.code === "Space") {
        event.preventDefault();
        togglePlayback();
        return;
      }

      if (isTyping || !selectedSegment) return;

      const key = event.key.toUpperCase();
      const tag = tagOptions.find((item) => item.key === key);
      if (tag) {
        event.preventDefault();
        toggleTag(tag.value);
        return;
      }

      if (event.code === "Space") {
        event.preventDefault();
        togglePlayback();
      } else if (key === "J") {
        event.preventDefault();
        moveSelection(1);
      } else if (key === "K") {
        event.preventDefault();
        moveSelection(-1);
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [annotationSegment, selectedSegment, visibleSegments, playbackPath, isPlaying, playbackCurrentMs]);

  async function chooseFolder() {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === "string") {
      setFolderPath(selected);
    }
  }

  async function chooseOutputFolder() {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === "string") {
      setOutputPath(selected);
    }
  }

  async function chooseExportJsonl() {
    const selected = await save({
      filters: [{ name: "JSONL", extensions: ["jsonl"] }],
      defaultPath: exportJsonlPath || "export.jsonl",
    });
    if (typeof selected === "string") {
      setExportJsonlPath(selected);
    }
  }

  async function scanFolder() {
    if (!folderPath.trim()) {
      setStatus("请先选择音频文件夹");
      return;
    }

    setBusy(true);
    setAnnotationSegmentId("");
    setSelectedSegmentId("");
    setStatus("正在扫描音频");
    setProgress({
      visible: true,
      label: "扫描音频",
      detail: "正在读取输入文件夹",
      current: 0,
      total: 0,
      indeterminate: true,
    });
    try {
      await waitForPaint();
      const result = await invoke<ProjectScan>("scan_project_folder", {
        folderPath,
        manifestPath: null,
        outputPath: outputPath.trim() || null,
      });
      setScan(result);
      setSelectedAudioId(result.audioFiles[0]?.id ?? "");
      if (autoCutAfterScan && result.audioFiles.length > 0) {
        const allSegments = await cutAudioBatch(result);
        setSegments(allSegments);
        const recognizedCount = autoRecognizeAfterCut
          ? await recognizeDraft(allSegments, {
              manageBusy: false,
              overwrite: false,
              label: "自动识别",
            })
          : 0;
        setStatus(
          `扫描并自动切割完成：${result.audioFiles.length} 个音频，${allSegments.length} 段 PCM WAV${
            autoRecognizeAfterCut ? `，识别 ${recognizedCount} 段` : ""
          }`,
        );
      } else {
        setStatus(`扫描完成：${result.audioFiles.length} 个音频`);
      }
    } catch (error) {
      setStatus(String(error));
    } finally {
      setProgress((current) => ({ ...current, visible: false }));
      setBusy(false);
    }
  }

  async function cutOne(audio: AudioFileInfo) {
    if (!scan) return;

    setBusy(true);
    selectAudio(audio);
    setStatus(`正在切割：${audio.fileName}`);
    setProgress({
      visible: true,
      label: "切割音频",
      detail: audio.fileName,
      current: 0,
      total: 1,
      indeterminate: true,
    });
    try {
      await waitForPaint();
      const created = await invoke<SegmentRecord[]>("cut_audio_file", {
        inputPath: audio.path,
        segmentsDir: scan.segmentsDir,
        config,
        role: audio.role ?? null,
        originalText: audio.matchedText ?? "",
        emotion: audio.matchedEmotion ?? [],
      });

      setSegments((current) => [
        ...current.filter((segment) => segment.sourcePath !== audio.path),
        ...created,
      ]);
      const recognizedCount = autoRecognizeAfterCut
        ? await recognizeDraft(created, {
            manageBusy: false,
            overwrite: false,
            label: "自动识别",
          })
        : 0;
      setStatus(
        `切割完成：${audio.fileName} -> ${created.length} 段 PCM WAV${
          autoRecognizeAfterCut ? `，识别 ${recognizedCount} 段` : ""
        }`,
      );
    } catch (error) {
      setStatus(String(error));
    } finally {
      setProgress((current) => ({ ...current, visible: false }));
      setBusy(false);
    }
  }

  async function cutAll() {
    if (!scan || scan.audioFiles.length === 0) return;

    setBusy(true);
    setAnnotationSegmentId("");
    setSelectedSegmentId("");
    try {
      const allSegments = await cutAudioBatch(scan);
      setSegments(allSegments);
      const recognizedCount = autoRecognizeAfterCut
        ? await recognizeDraft(allSegments, {
            manageBusy: false,
            overwrite: false,
            label: "自动识别",
          })
        : 0;
      setStatus(
        `批量切割完成：${allSegments.length} 段 PCM WAV${
          autoRecognizeAfterCut ? `，识别 ${recognizedCount} 段` : ""
        }`,
      );
    } catch (error) {
      setStatus(String(error));
    } finally {
      setProgress((current) => ({ ...current, visible: false }));
      setBusy(false);
    }
  }

  async function cutAudioBatch(targetScan: ProjectScan) {
    const allSegments: SegmentRecord[] = [];
    setSegments([]);
    setSelectedSegmentId("");
    setAnnotationSegmentId("");
    for (const [index, audio] of targetScan.audioFiles.entries()) {
      setSelectedAudioId(audio.id);
      setStatus(
        `正在自动切割 ${index + 1}/${targetScan.audioFiles.length}：${audio.fileName}`,
      );
      setProgress({
        visible: true,
        label: "自动切割",
        detail: audio.fileName,
        current: index,
        total: targetScan.audioFiles.length,
        indeterminate: true,
      });
      await waitForPaint();
      const created = await invoke<SegmentRecord[]>("cut_audio_file", {
        inputPath: audio.path,
        segmentsDir: targetScan.segmentsDir,
        config,
        role: audio.role ?? null,
        originalText: audio.matchedText ?? "",
        emotion: audio.matchedEmotion ?? [],
      });
      allSegments.push(...created);
      setSegments([...allSegments]);
      setProgress({
        visible: true,
        label: "自动切割",
        detail: `${audio.fileName} 已完成，生成 ${created.length} 段`,
        current: index + 1,
        total: targetScan.audioFiles.length,
      });
      await waitForPaint();
    }
    return allSegments;
  }

  async function saveProject() {
    if (!scan) {
      setStatus("请先扫描项目");
      return;
    }

    const payload: ProjectFile = {
      version: 1,
      savedAt: new Date().toISOString(),
      rootPath: scan.rootPath,
      projectDir: scan.projectDir,
      segmentsDir: scan.segmentsDir,
      config,
      audioFiles: scan.audioFiles,
      manifestRecords: scan.manifestRecords,
      segments,
    };

    setBusy(true);
    try {
      const path = await invoke<string>("save_project_file", {
        projectDir: scan.projectDir,
        payload,
      });
      setStatus(`已保存：${path}`);
    } catch (error) {
      setStatus(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function loadProject() {
    if (!scan) {
      setStatus("请先扫描项目");
      return;
    }

    setBusy(true);
    try {
      const project = await invoke<ProjectFile>("load_project_file", {
        projectDir: scan.projectDir,
      });
      setConfig(project.config ?? defaultConfig);
      setSegments(project.segments ?? []);
      setSelectedSegmentId("");
      setAnnotationSegmentId("");
      setStatus(`已打开保存：${project.segments?.length ?? 0} 段`);
    } catch (error) {
      setStatus(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function exportJsonl() {
    if (!scan) {
      setStatus("请先扫描项目");
      return;
    }

    setBusy(true);
    try {
      const path = await invoke<string>("export_segments_jsonl", {
        projectDir: scan.projectDir,
        outputPath: exportJsonlPath.trim() || null,
        segments,
      });
      if (!exportJsonlPath.trim()) {
        setExportJsonlPath(path);
      }
      setStatus(`已导出：${path}`);
    } catch (error) {
      setStatus(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function recognizeVisibleDraft() {
    const target = visibleSegments.length > 0 ? visibleSegments : segments;
    const recognizedCount = await recognizeDraft(target, {
      manageBusy: true,
      overwrite: false,
      label: "识别初稿",
    });
    if (recognizedCount > 0) {
      setStatus(`识别初稿完成：${recognizedCount} 段`);
    }
  }

  async function recognizeCurrentSegment() {
    if (!annotationSegment) return;
    const recognizedCount = await recognizeDraft([annotationSegment], {
      manageBusy: true,
      overwrite: true,
      label: "识别本段",
    });
    if (recognizedCount > 0) {
      setStatus(`识别本段完成：${annotationSegment.segmentFileName}`);
    }
  }

  async function recognizeDraft(
    targetSegments: SegmentRecord[],
    options: { manageBusy: boolean; overwrite: boolean; label: string },
  ) {
    if (!scan || targetSegments.length === 0) {
      setStatus("没有可识别的切割片段");
      return 0;
    }

    const { manageBusy, overwrite, label } = options;
    if (manageBusy) setBusy(true);
    const batchSize = 4;
    let completed = 0;
    let recognizedCount = 0;
    setProgress({
      visible: true,
      label,
      detail: `准备调用本地 whisper 识别 ${targetSegments.length} 段`,
      current: 0,
      total: targetSegments.length,
    });

    try {
      await waitForPaint();
      for (let index = 0; index < targetSegments.length; index += batchSize) {
        const batch = targetSegments.slice(index, index + batchSize);
        setProgress({
          visible: true,
          label,
          detail: `正在识别 ${completed + 1}-${completed + batch.length}/${targetSegments.length}`,
          current: completed,
          total: targetSegments.length,
        });
        await waitForPaint();

        const results = await invoke<RecognitionResult[]>("recognize_segments", {
          projectDir: scan.projectDir,
          segments: batch,
          model: "base",
        });
        const resultMap = new Map(
          results
            .map((item) => [item.segmentId, normalizeRecognizedText(item.text)] as const)
            .filter(([, text]) => text.trim()),
        );
        recognizedCount += resultMap.size;
        setSegments((current) =>
          current.map((segment) => {
            const text = resultMap.get(segment.id);
            if (!text) return segment;
            return {
              ...segment,
              originalText: overwrite ? text : segment.originalText || text,
              phoneticText: overwrite ? text : segment.phoneticText || text,
            };
          }),
        );
        completed += batch.length;
        setProgress({
          visible: true,
          label,
          detail: `已完成 ${completed}/${targetSegments.length}`,
          current: completed,
          total: targetSegments.length,
        });
        await waitForPaint();
      }
      return recognizedCount;
    } catch (error) {
      setStatus(`识别失败：${String(error)}`);
      return 0;
    } finally {
      setProgress((current) => ({ ...current, visible: false }));
      if (manageBusy) setBusy(false);
    }
  }

  function selectAudio(audio: AudioFileInfo) {
    setSelectedAudioId(audio.id);
    setSelectedSegmentId("");
    setAnnotationSegmentId("");
    setPlaybackCurrentMs(0);
  }

  function openAnnotation(segment: SegmentRecord, playNow = true) {
    const sourceAudio = scan?.audioFiles.find((audio) => audio.path === segment.sourcePath);
    if (sourceAudio) setSelectedAudioId(sourceAudio.id);
    const fallbackText =
      segment.phoneticText || segment.originalText || sourceAudio?.matchedText || "";
    if (fallbackText && (!segment.phoneticText || !segment.originalText)) {
      updateSegment(segment.id, {
        phoneticText: segment.phoneticText || fallbackText,
        originalText: segment.originalText || fallbackText,
      });
    }
    setSelectedSegmentId(segment.id);
    setAnnotationSegmentId(segment.id);
    setPlaybackCurrentMs(0);
    setAutoPlayOnReady(playNow);
  }

  function closeAnnotation() {
    setAnnotationSegmentId("");
    void invoke<PlaybackState>("stop_audio").then(applyPlaybackState).catch(() => {
      setIsPlaying(false);
      setPlaybackCurrentMs(0);
    });
  }

  function updateSegment(id: string, patch: Partial<SegmentRecord>) {
    setSegments((current) =>
      current.map((segment) =>
        segment.id === id ? { ...segment, ...patch } : segment,
      ),
    );
  }

  function toggleTag(tag: string) {
    if (!selectedSegment) return;
    const nextTags = selectedSegment.tags.includes(tag)
      ? selectedSegment.tags.filter((item) => item !== tag)
      : [...selectedSegment.tags, tag];
    updateSegment(selectedSegment.id, { tags: nextTags });
  }

  function moveSelection(direction: number) {
    if (!visibleSegments.length || !selectedSegmentId) return;
    const index = visibleSegments.findIndex((segment) => segment.id === selectedSegmentId);
    const next = Math.min(Math.max(index + direction, 0), visibleSegments.length - 1);
    openAnnotation(visibleSegments[next], false);
  }

  function applyPlaybackState(state: PlaybackState) {
    setPlaybackCurrentMs(state.positionMs);
    if (state.durationMs) setPlaybackDurationMs(state.durationMs);
    setIsPlaying(state.isPlaying);
  }

  function togglePlayback() {
    if (!playbackPath) return;
    if (isPlaying) {
      void invoke<PlaybackState>("pause_audio")
        .then(applyPlaybackState)
        .catch((error) => setPlaybackStatus(`暂停失败：${String(error)}`));
    } else {
      void invoke<PlaybackState>("play_audio", {
        path: playbackPath,
        positionMs: playbackCurrentMs,
      })
        .then(applyPlaybackState)
        .catch((error) => setPlaybackStatus(`播放失败：${String(error)}`));
    }
  }

  function seekToRatio(ratio: number) {
    if (!activeDurationMs) return;
    const positionMs = Math.max(0, Math.min(activeDurationMs, ratio * activeDurationMs));
    setPlaybackCurrentMs(positionMs);
    if (playbackPath && isPlaying) {
      void invoke<PlaybackState>("play_audio", {
        path: playbackPath,
        positionMs: Math.round(positionMs),
      })
        .then(applyPlaybackState)
        .catch((error) => setPlaybackStatus(`跳转失败：${String(error)}`));
    }
  }

  function updateEmotion(value: string) {
    if (!annotationSegment) return;
    const emotion = value
      .split(/[,\s，、]+/)
      .map((item) => item.trim())
      .filter(Boolean);
    updateSegment(annotationSegment.id, { emotion });
  }

  function updateAnnotationText(value: string) {
    if (!annotationSegment) return;
    updateSegment(annotationSegment.id, { phoneticText: value });
  }

  function insertInlineTag(tag: string) {
    const openTag = `<${tag}>`;
    const closeTag = `</${tag}>`;
    replaceAnnotationSelection(openTag, closeTag, true);
  }

  function addEmotionToSelection(emotion: string) {
    const openTag = `<emotion value="${emotion}">`;
    const closeTag = "</emotion>";
    replaceAnnotationSelection(openTag, closeTag, false);
    if (annotationSegment && !annotationSegment.emotion.includes(emotion)) {
      updateSegment(annotationSegment.id, {
        emotion: [...annotationSegment.emotion, emotion],
      });
    }
  }

  function replaceAnnotationSelection(
    openTag: string,
    closeTag: string,
    placeCaretInside: boolean,
  ) {
    const editor = annotationTextRef.current;
    if (!editor || !annotationSegment) return;
    const start = editor.selectionStart;
    const end = editor.selectionEnd;
    const value = annotationSegment.phoneticText;
    const selected = value.slice(start, end);
    const next = `${value.slice(0, start)}${openTag}${selected}${closeTag}${value.slice(end)}`;
    updateSegment(annotationSegment.id, { phoneticText: next });
    requestAnimationFrame(() => {
      editor.focus();
      const caret = placeCaretInside
        ? start + openTag.length
        : start + openTag.length + selected.length + closeTag.length;
      editor.setSelectionRange(caret, caret);
    });
  }

  if (annotationSegment) {
    return (
      <main className="annotation-shell">
        <header className="annotation-topbar">
          <button className="ghost-button" onClick={closeAnnotation}>
            <ArrowLeft size={16} />
            返回
          </button>
          <div>
            <h1>{annotationSegment.segmentFileName}</h1>
            <p>{playbackStatus || formatMsRange(annotationSegment.startMs, annotationSegment.endMs)}</p>
          </div>
          <div className="topbar-actions">
            <button onClick={recognizeCurrentSegment} disabled={busy || !scan}>
              <WandSparkles size={16} />
              识别本段
            </button>
            <button onClick={saveProject} disabled={busy || !scan}>
              <Save size={16} />
              保存
            </button>
            <button onClick={exportJsonl} disabled={busy || !scan || segments.length === 0}>
              <Download size={16} />
              导出 JSONL
            </button>
          </div>
        </header>

        <section className="annotation-player">
          <button className="play-button" onClick={togglePlayback} disabled={!playbackPath}>
            {isPlaying ? <Pause size={18} /> : <Play size={18} />}
            {isPlaying ? "暂停" : "播放"}
          </button>
          <AnnotationTimeline
            currentMs={playbackCurrentMs}
            durationMs={activeDurationMs}
            text={annotationSegment.phoneticText || annotationSegment.originalText}
            onSeek={seekToRatio}
          />
          <span className="time-readout">
            {formatClock(playbackCurrentMs)} / {formatClock(activeDurationMs)}
          </span>
        </section>

        {progress.visible && <ProgressPanel progress={progress} />}

        <section className="annotation-editor">
          <div className="annotation-meta">
            <label>
              <span>原文本</span>
              <textarea
                value={annotationSegment.originalText}
                onChange={(event) =>
                  updateSegment(annotationSegment.id, { originalText: event.target.value })
                }
              />
            </label>
            <label>
              <span>情感</span>
              <input
                value={annotationSegment.emotion.join("，")}
                onChange={(event) => updateEmotion(event.target.value)}
              />
            </label>
          </div>

          <div className="annotation-toolbar">
            {emotionOptions.map((emotion) => (
              <button key={emotion} onClick={() => addEmotionToSelection(emotion)}>
                <Tags size={15} />
                {emotion}
              </button>
            ))}
            {inlineTags.map((item) => {
              const Icon = tagIcons[item.tag] ?? Tags;
              return (
                <button key={item.tag} onClick={() => insertInlineTag(item.tag)}>
                  <Icon size={15} />
                  插入{item.label}
                </button>
              );
            })}
          </div>

          <textarea
            ref={annotationTextRef}
            className="annotation-textarea"
            value={annotationSegment.phoneticText}
            onChange={(event) => updateAnnotationText(event.target.value)}
          />

          <label className="annotation-notes">
            <span>备注</span>
            <textarea
              value={annotationSegment.notes}
              onChange={(event) =>
                updateSegment(annotationSegment.id, { notes: event.target.value })
              }
            />
          </label>
        </section>
      </main>
    );
  }

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand-block">
          <div className="brand-mark">湘</div>
          <div>
            <h1>长沙方言标注工作台</h1>
            <p>{status}</p>
          </div>
        </div>
        <div className="topbar-actions">
          <button onClick={saveProject} disabled={busy || !scan}>
            <Save size={16} />
            保存
          </button>
          <button onClick={loadProject} disabled={busy || !scan}>
            <FileJson size={16} />
            打开保存
          </button>
          <button onClick={exportJsonl} disabled={busy || !scan || segments.length === 0}>
            <Download size={16} />
            导出 JSONL
          </button>
        </div>
      </header>

      <section className="setup-band">
        <label>
          <span>输入文件夹</span>
          <div className="path-row">
            <input
              value={folderPath}
              onChange={(event) => setFolderPath(event.target.value)}
              placeholder="选择包含音频的文件夹"
            />
            <button onClick={chooseFolder} disabled={busy}>
              <FolderOpen size={16} />
              选择
            </button>
          </div>
        </label>
        <label>
          <span>输出文件夹</span>
          <div className="path-row">
            <input
              value={outputPath}
              onChange={(event) => setOutputPath(event.target.value)}
              placeholder="默认：输入文件夹/_dialect_labeler"
            />
            <button onClick={chooseOutputFolder} disabled={busy}>
              <FolderOpen size={16} />
              选择
            </button>
          </div>
        </label>
        <label>
          <span>导出 JSONL</span>
          <div className="path-row">
            <input
              value={exportJsonlPath}
              onChange={(event) => setExportJsonlPath(event.target.value)}
              placeholder="最终输出文件，默认：输出文件夹/export.jsonl"
            />
            <button onClick={chooseExportJsonl} disabled={busy}>
              <FileJson size={16} />
              选择
            </button>
          </div>
        </label>
        <label className="checkbox-field">
          <input
            type="checkbox"
            checked={autoCutAfterScan}
            onChange={(event) => setAutoCutAfterScan(event.target.checked)}
          />
          <span>扫描后自动切割</span>
        </label>
        <button className="primary" onClick={scanFolder} disabled={busy}>
          <ScanSearch size={16} />
          扫描
        </button>
      </section>

      <section className="config-band">
        <NumberField
          label="静音阈值 dB"
          value={config.silenceDb}
          onChange={(value) => setConfig({ ...config, silenceDb: value })}
        />
        <NumberField
          label="最短停顿 ms"
          value={config.minSilenceMs}
          onChange={(value) => setConfig({ ...config, minSilenceMs: value })}
        />
        <NumberField
          label="最短语音 ms"
          value={config.minSegmentMs}
          onChange={(value) => setConfig({ ...config, minSegmentMs: value })}
        />
        <NumberField
          label="前留空 ms"
          value={config.preRollMs}
          onChange={(value) => setConfig({ ...config, preRollMs: value })}
        />
        <NumberField
          label="后留空 ms"
          value={config.postRollMs}
          onChange={(value) => setConfig({ ...config, postRollMs: value })}
        />
        <label className="checkbox-field">
          <input
            type="checkbox"
            checked={autoRecognizeAfterCut}
            onChange={(event) => setAutoRecognizeAfterCut(event.target.checked)}
          />
          <span>切割后自动识别</span>
        </label>
        <button onClick={cutAll} disabled={busy || !scan || scan.audioFiles.length === 0}>
          <Scissors size={16} />
          全部切割
        </button>
        <button onClick={recognizeVisibleDraft} disabled={busy || segments.length === 0}>
          <WandSparkles size={16} />
          识别初稿
        </button>
      </section>

      {progress.visible && <ProgressPanel progress={progress} />}

      <section className="workspace-grid">
        <aside className="pane audio-pane">
          <div className="pane-title">
            <h2>原音频</h2>
            <span>{scan?.audioFiles.length ?? 0}</span>
          </div>
          <div className="audio-list">
            {scan?.audioFiles.map((audio) => (
              <div
                className={`audio-row ${audio.id === selectedAudioId ? "active" : ""}`}
                key={audio.id}
                onClick={() => selectAudio(audio)}
                role="button"
                tabIndex={0}
                title={audio.path}
              >
                <div>
                  <strong title={audio.fileName}>{audio.fileName}</strong>
                  <span>
                    {roleLabels[audio.role ?? "unknown"]} · {formatDuration(audio.durationMs)}
                  </span>
                </div>
                <small>
                  {audio.sampleRate ? `${audio.sampleRate}Hz` : "-"} ·{" "}
                  {audio.channels ? `${audio.channels}ch` : "-"}
                </small>
                <button
                  className="compact"
                  onClick={(event) => {
                    event.stopPropagation();
                    void cutOne(audio);
                  }}
                  disabled={busy}
                >
                  <Scissors size={14} />
                  切割
                </button>
              </div>
            ))}
          </div>
        </aside>

        <section className="pane center-pane">
          <div className="pane-title">
            <h2>切割片段</h2>
            <span>
              {segmentStats.done}/{segmentStats.total}
            </span>
          </div>

          <div className="source-player">
            <button
              className="transport-button"
              onClick={togglePlayback}
              disabled={!playbackPath}
              title={isPlaying ? "暂停原音频" : "播放原音频"}
              aria-label={isPlaying ? "暂停原音频" : "播放原音频"}
            >
              {isPlaying ? <Pause size={18} /> : <Volume2 size={18} />}
            </button>
            <div className="source-player-copy">
              <strong>{selectedAudio?.fileName ?? "未选择原音频"}</strong>
              <span>{playbackStatus || formatClock(activeDurationMs)}</span>
            </div>
            <div className="source-player-timeline">
              <MiniTimeline
                currentMs={playbackCurrentMs}
                durationMs={activeDurationMs}
                onSeek={seekToRatio}
              />
            </div>
            <span className="time-readout">
              {formatClock(playbackCurrentMs)} / {formatClock(activeDurationMs)}
            </span>
          </div>

          <div className="segment-list">
            {visibleSegments.map((segment) => (
              <button
                key={segment.id}
                className={`segment-row ${
                  segment.id === selectedSegmentId ? "active" : ""
                }`}
                onClick={() => openAnnotation(segment, true)}
                title={segment.segmentFileName}
              >
                <span>{segment.segmentFileName}</span>
                <strong>{formatMsRange(segment.startMs, segment.endMs)}</strong>
                <small>{segment.phoneticText || segment.originalText || "未标注"}</small>
              </button>
            ))}
          </div>
        </section>

        <section className="pane info-pane">
          <div className="pane-title">
            <h2>当前原文</h2>
            <span>{selectedAudio ? roleLabels[selectedAudio.role ?? "unknown"] : "-"}</span>
          </div>
          <div className="source-text">
            <p className={selectedAudio?.matchedText ? "" : "source-text-warning"}>
              {sourceTextMessage(scan, selectedAudio)}
            </p>
            <dl>
              <dt>识别状态</dt>
              <dd>{segmentStats.done}/{segmentStats.total}</dd>
              <dt>采样率</dt>
              <dd>{selectedAudio?.sampleRate ? `${selectedAudio.sampleRate} Hz` : "-"}</dd>
              <dt>声道</dt>
              <dd>{selectedAudio?.channels ?? "-"}</dd>
              <dt>编码</dt>
              <dd>{selectedAudio?.codecName ?? "-"}</dd>
              <dt>输出目录</dt>
              <dd>{scan?.projectDir ?? "-"}</dd>
            </dl>
          </div>
        </section>
      </section>
    </main>
  );
}

function AnnotationTimeline({
  currentMs,
  durationMs,
  text,
  onSeek,
}: {
  currentMs: number;
  durationMs: number;
  text: string;
  onSeek: (ratio: number) => void;
}) {
  const pixelsPerSecond = 120;
  const maxRowMs = 8000;
  const totalMs = Math.max(durationMs, 1000);
  const safeCurrentMs = Math.max(0, Math.min(currentMs, totalMs));
  const displayText = stripTags(text).trim();
  const chars = Array.from(displayText);
  const activeIndex = chars.length
    ? Math.min(chars.length - 1, Math.floor((safeCurrentMs / totalMs) * chars.length))
    : -1;
  const rows = buildTimelineRows(totalMs, chars, maxRowMs, pixelsPerSecond);

  function handleSeek(
    event: ReactMouseEvent<HTMLDivElement>,
    rowStartMs: number,
    rowEndMs: number,
  ) {
    const rect = event.currentTarget.getBoundingClientRect();
    const localRatio = Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width));
    const rowDurationMs = Math.max(rowEndMs - rowStartMs, 1);
    const nextMs = Math.min(totalMs, rowStartMs + localRatio * rowDurationMs);
    onSeek(nextMs / totalMs);
  }

  return (
    <div className="timeline-scroll">
      <div className="timeline-stack">
        {rows.map((row, rowIndex) => {
          const rowDurationMs = Math.max(row.endMs - row.startMs, 1);
          const playedRatio =
            safeCurrentMs <= row.startMs
              ? 0
              : safeCurrentMs >= row.endMs
                ? 1
                : (safeCurrentMs - row.startMs) / rowDurationMs;
          const showCursor =
            (safeCurrentMs >= row.startMs && safeCurrentMs < row.endMs) ||
            (rowIndex === rows.length - 1 && safeCurrentMs === totalMs);

          return (
            <div className="timeline-canvas" style={{ width: row.width }} key={row.startMs}>
              <div
                className="timeline-bar"
                onClick={(event) => handleSeek(event, row.startMs, row.endMs)}
              >
                <div className="timeline-played" style={{ width: `${playedRatio * 100}%` }} />
                {showCursor && (
                  <div className="timeline-cursor" style={{ left: `${playedRatio * 100}%` }} />
                )}
                {row.ticks.map((tick) => (
                  <span
                    key={tick.ms}
                    className="timeline-tick"
                    style={{ left: `${tick.ratio * 100}%` }}
                  >
                    {tick.label}
                  </span>
                ))}
              </div>
              <div className={chars.length ? "timeline-text" : "timeline-text empty"}>
                {chars.length ? (
                  row.chars.map((item) => (
                    <span
                      key={`${item.char}-${item.index}`}
                      className={item.index <= activeIndex ? "active" : ""}
                    >
                      {item.char}
                    </span>
                  ))
                ) : rowIndex === 0 ? (
                  <span className="timeline-placeholder">暂无默认文字，请在下方输入记音字</span>
                ) : null}
              </div>
            </div>
          );
        })}
        </div>
    </div>
  );
}

function MiniTimeline({
  currentMs,
  durationMs,
  onSeek,
}: {
  currentMs: number;
  durationMs: number;
  onSeek: (ratio: number) => void;
}) {
  const ratio = Math.max(0, Math.min(1, currentMs / Math.max(durationMs, 1)));

  function handleSeek(event: ReactMouseEvent<HTMLDivElement>) {
    const rect = event.currentTarget.getBoundingClientRect();
    onSeek((event.clientX - rect.left) / rect.width);
  }

  return (
    <div className="mini-timeline" onClick={handleSeek} role="progressbar">
      <div className="mini-timeline-fill" style={{ width: `${ratio * 100}%` }} />
    </div>
  );
}

function ProgressPanel({ progress }: { progress: ProgressState }) {
  const percent =
    progress.total > 0
      ? Math.min(100, Math.round((progress.current / progress.total) * 100))
      : 0;

  return (
    <section className="progress-panel">
      <div className="progress-copy">
        <strong>{progress.label}</strong>
        <span title={progress.detail}>{progress.detail}</span>
      </div>
      <div
        className={`progress-track ${progress.indeterminate ? "indeterminate" : ""}`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={percent}
        role="progressbar"
      >
        <div className="progress-fill" style={{ width: `${percent}%` }} />
      </div>
      <small>
        {progress.total > 0
          ? `${progress.current}/${progress.total} · ${percent}%`
          : "处理中"}
      </small>
    </section>
  );
}

function NumberField({
  label,
  value,
  onChange,
}: {
  label: string;
  value: number;
  onChange: (value: number) => void;
}) {
  return (
    <label className="number-field">
      <span>{label}</span>
      <input
        type="number"
        value={value}
        onChange={(event) => onChange(Number(event.target.value))}
      />
    </label>
  );
}

function waitForPaint() {
  return new Promise<void>((resolve) => {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => resolve());
    });
  });
}

function buildTimelineRows(
  durationMs: number,
  chars: string[],
  maxRowMs: number,
  pixelsPerSecond: number,
) {
  const charWidth = 30;
  const minRowWidth = 240;
  const rowCount = Math.max(1, Math.ceil(durationMs / maxRowMs));

  return Array.from({ length: rowCount }, (_, rowIndex) => {
    const startMs = rowIndex * maxRowMs;
    const endMs = Math.min(durationMs, startMs + maxRowMs);
    const rowDurationMs = Math.max(endMs - startMs, 1);
    const charStart = Math.floor((rowIndex / rowCount) * chars.length);
    const charEnd = Math.floor(((rowIndex + 1) / rowCount) * chars.length);
    const rowChars = chars.slice(charStart, charEnd).map((char, index) => ({
      char,
      index: charStart + index,
    }));
    const timeWidth = Math.round((rowDurationMs / 1000) * pixelsPerSecond);
    const textWidth = rowChars.length * charWidth;
    const width = Math.max(minRowWidth, timeWidth, textWidth);

    return {
      startMs,
      endMs,
      width,
      chars: rowChars,
      ticks: buildTimelineRowTicks(startMs, endMs),
    };
  });
}

function buildTimelineRowTicks(startMs: number, endMs: number) {
  const durationSeconds = Math.max((endMs - startMs) / 1000, 1);
  const step = durationSeconds > 6 ? 2 : 1;
  const firstSecond = Math.ceil(startMs / 1000 / step) * step;
  const lastSecond = Math.floor(endMs / 1000);
  const ticks = [];

  for (let second = firstSecond; second <= lastSecond; second += step) {
    const ms = second * 1000;
    ticks.push({
      ms,
      ratio: Math.max(0, Math.min(1, (ms - startMs) / Math.max(endMs - startMs, 1))),
      label: formatClock(ms),
    });
  }
  return ticks;
}

function stripTags(value: string) {
  return value.replace(/<[^>]+>/g, "");
}

function normalizeRecognizedText(value: string) {
  return value.replace(/\s+/g, "").trim();
}

function sourceTextMessage(scan: ProjectScan | null, audio: AudioFileInfo | null) {
  if (!scan) return "请先扫描音频文件夹";
  if (!audio) return "未选择原音频";
  if (audio.matchedText?.trim()) return audio.matchedText;
  return "当前原音频没有整段文字；切割后可点击识别初稿，或进入片段标注手动填写记音汉字";
}

function formatDuration(value?: number) {
  if (!value) return "-";
  return formatClock(value);
}

function formatClock(value?: number) {
  if (!value) return "0:00";
  const totalSeconds = Math.max(0, Math.floor(value / 1000));
  const min = Math.floor(totalSeconds / 60);
  const sec = String(totalSeconds % 60).padStart(2, "0");
  return `${min}:${sec}`;
}

function formatMsRange(start: number, end: number) {
  return `${(start / 1000).toFixed(2)}-${(end / 1000).toFixed(2)}s`;
}

export default App;
