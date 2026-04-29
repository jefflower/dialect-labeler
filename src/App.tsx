import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { Folder, Inbox } from "lucide-react";
import "./App.css";
import { Topbar } from "./components/Topbar";
import { SetupBand } from "./components/SetupBand";
import { ConfigBand } from "./components/ConfigBand";
import { MainView } from "./components/MainView";
import { AnnotationView } from "./components/AnnotationView";
import type { SelectionRange } from "./components/AlignedTimeline";
import { ProgressPanel } from "./components/ProgressPanel";
import { SettingsDrawer } from "./components/SettingsDrawer";
import { ShortcutOverlay } from "./components/ShortcutOverlay";
import { ToastStack } from "./components/ToastStack";
import { defaultCutConfig } from "./defaults";
import { REVIEW_ONLY } from "./env";
import { ipc, normalizeRecognizedText, waitForPaint, clamp } from "./lib";
import { isTyping, useSettings, useShortcutOverlay, useTheme, useToasts } from "./hooks";
import type {
  AudioFileInfo,
  CutConfig,
  InlineTagDef,
  PlaybackAudio,
  PlaybackState,
  ProgressState,
  ProjectFile,
  ProjectScan,
  SegmentRecord,
} from "./types";

function App() {
  const { settings, update: updateSettings } = useSettings();
  useTheme(settings.theme);

  // Tag dictionaries are user-editable; expose them as derived state so any
  // place that used to import the static constant now reads the live array.
  const inlineTags = settings.inlineTags;
  const tagOptions = settings.segmentTags;
  const emotionOptions = settings.emotions;
  const inlineTagByKey = useMemo(
    () =>
      new Map<string, InlineTagDef>(
        inlineTags
          .filter((item) => item.key)
          .map((item) => [item.key.toUpperCase(), item]),
      ),
    [inlineTags],
  );

  const { toasts, push: pushToast, dismiss: dismissToast } = useToasts();
  const { open: shortcutOpen, setOpen: setShortcutOpen } = useShortcutOverlay();
  const [settingsOpen, setSettingsOpen] = useState(false);

  const [folderPath, setFolderPath] = useState("");
  const [outputPath, setOutputPath] = useState("");
  const [exportJsonlPath, setExportJsonlPath] = useState("");
  const [scan, setScan] = useState<ProjectScan | null>(null);
  const [config, setConfig] = useState<CutConfig>(defaultCutConfig);
  const [segments, setSegments] = useState<SegmentRecord[]>([]);
  const [selectedAudioId, setSelectedAudioId] = useState("");
  const [selectedSegmentId, setSelectedSegmentId] = useState("");
  const [annotationSegmentId, setAnnotationSegmentId] = useState("");
  const [autoCutAfterScan, setAutoCutAfterScan] = useState(!REVIEW_ONLY);
  const [autoRecognizeAfterCut, setAutoRecognizeAfterCut] = useState(false);
  const [autoPlayOnReady, setAutoPlayOnReady] = useState(false);
  const [status, setStatus] = useState("请选择音频文件夹开始");
  const [hasError, setHasError] = useState(false);
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
  const [dropping, setDropping] = useState(false);

  // Aligned-timeline selection lifted from AnnotationView so keyboard shortcuts
  // can act on it (wrap inline tags / insert breath / pause markers).
  const [timelineSelection, setTimelineSelection] =
    useState<SelectionRange | null>(null);

  // Per-segment undo/redo history. Snapshots are taken before each mutation.
  // Typing snapshots are debounced (~700ms) so Cmd+Z doesn't undo per-char.
  const undoRef = useRef<Map<string, SegmentRecord[]>>(new Map());
  const redoRef = useRef<Map<string, SegmentRecord[]>>(new Map());
  const lastSnapshotRef = useRef<{ id: string; time: number } | null>(null);

  const selectedAudio = useMemo(
    () => scan?.audioFiles.find((audio) => audio.id === selectedAudioId) ?? null,
    [scan, selectedAudioId],
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

  const setStatusMsg = useCallback((message: string, error = false) => {
    setStatus(message);
    setHasError(error);
  }, []);

  const showError = useCallback(
    (title: string, error: unknown) => {
      const detail = String(error);
      pushToast({ variant: "error", title, detail });
      setStatusMsg(`${title}：${detail}`, true);
    },
    [pushToast, setStatusMsg],
  );

  const showSuccess = useCallback(
    (title: string, detail?: string) => {
      pushToast({ variant: "success", title, detail });
      setStatusMsg(detail ? `${title}：${detail}` : title);
    },
    [pushToast, setStatusMsg],
  );

  // Load default LLM prompt once, store into settings if user has empty prompt.
  useEffect(() => {
    if (settings.llmPrompt.trim()) return;
    let cancelled = false;
    ipc
      .getDefaultLlmPrompt()
      .then((prompt) => {
        if (cancelled) return;
        if (!settings.llmPrompt.trim()) {
          updateSettings({ llmPrompt: prompt });
        }
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  const cycleTheme = useCallback(() => {
    const order = ["system", "light", "dark"] as const;
    const idx = order.indexOf(settings.theme);
    const next = order[(idx + 1) % order.length];
    updateSettings({ theme: next });
  }, [settings.theme, updateSettings]);

  const resetLlmPrompt = useCallback(async () => {
    try {
      const prompt = await ipc.getDefaultLlmPrompt();
      updateSettings({ llmPrompt: prompt });
      pushToast({ variant: "info", title: "已恢复默认 Prompt" });
    } catch (error) {
      showError("恢复默认 Prompt 失败", error);
    }
  }, [pushToast, showError, updateSettings]);

  // Debounced auto-save: persist project state ~2s after the last edit so the
  // user never loses annotation work even on a hard quit.
  useEffect(() => {
    if (!scan || segments.length === 0 || busy) return;
    const handle = window.setTimeout(() => {
      const payload: ProjectFile = {
        version: 2,
        savedAt: new Date().toISOString(),
        rootPath: scan.rootPath,
        projectDir: scan.projectDir,
        segmentsDir: scan.segmentsDir,
        config,
        audioFiles: scan.audioFiles,
        manifestRecords: scan.manifestRecords,
        segments,
        systemPrompt: settings.systemPrompt,
      };
      void ipc
        .saveProjectFile({ projectDir: scan.projectDir, payload })
        .catch(() => undefined);
    }, 2000);
    return () => window.clearTimeout(handle);
  }, [segments, config, settings.systemPrompt, scan, busy]);

  // Drag & drop folder support (Tauri v2 file drop event)
  useEffect(() => {
    let unlistenDrop: (() => void) | undefined;
    let unlistenHover: (() => void) | undefined;
    let unlistenCancel: (() => void) | undefined;

    void listen<{ paths: string[] }>("tauri://drag-drop", (event) => {
      const path = event.payload?.paths?.[0];
      if (path) {
        setFolderPath(path);
        setDropping(false);
        pushToast({
          variant: "info",
          title: "文件夹已选择",
          detail: path,
        });
      }
    }).then((fn) => {
      unlistenDrop = fn;
    });
    void listen("tauri://drag-enter", () => setDropping(true)).then((fn) => {
      unlistenHover = fn;
    });
    void listen("tauri://drag-leave", () => setDropping(false)).then((fn) => {
      unlistenCancel = fn;
    });

    return () => {
      unlistenDrop?.();
      unlistenHover?.();
      unlistenCancel?.();
    };
  }, [pushToast]);

  // Prepare playback when target path / scan changes
  useEffect(() => {
    let cancelled = false;
    async function prep() {
      if (!playbackTargetPath || !scan) {
        setPlaybackPath("");
        setPlaybackStatus("");
        setPlaybackDurationMs(0);
        setPlaybackCurrentMs(0);
        setIsPlaying(false);
        return;
      }
      await ipc.stopAudio().catch(() => undefined);
      setIsPlaying(false);
      setPlaybackStatus("正在准备预听音频");
      setPlaybackCurrentMs(0);
      try {
        const playback: PlaybackAudio = await ipc.preparePlaybackAudio({
          inputPath: playbackTargetPath,
          cacheDir: scan.projectDir,
        });
        if (cancelled) return;
        setPlaybackPath(playback.path);
        setPlaybackDurationMs(playback.durationMs ?? 0);
        setPlaybackStatus(playback.isPreview ? "已生成预听副本" : "");
        if (autoPlayOnReady) {
          setAutoPlayOnReady(false);
          const state = await ipc.playAudio({
            path: playback.path,
            positionMs: 0,
          });
          if (!cancelled) applyPlaybackState(state);
        }
      } catch (error) {
        if (cancelled) return;
        setPlaybackPath("");
        setPlaybackStatus(`预听失败：${String(error)}`);
      }
    }
    void prep();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [playbackTargetPath, scan?.projectDir]);

  // Poll playback while playing
  useEffect(() => {
    if (!isPlaying) return;
    const timer = window.setInterval(() => {
      void ipc.audioState().then(applyPlaybackState).catch(() => undefined);
    }, 160);
    return () => window.clearInterval(timer);
  }, [isPlaying]);

  // Context-aware keyboard handler. Behaviour summary:
  //   - Cmd/Ctrl + Z / Shift+Z       : undo / redo on current segment
  //   - Space                        : play / pause
  //   - J / K                        : next / prev segment
  //   - Esc                          : close annotation, or clear selection
  //   - L / S / U                    : wrap selection with paired tag (if a
  //                                    char selection exists in annotation
  //                                    mode); otherwise toggle segment tag
  //   - B / P                        : insert self-closing breath/pause
  //                                    marker at selection start
  //   - 1..6                         : set emotion (中立/开心/惊讶/疑问/生气/难过)
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      // Allow undo/redo even when typing in textarea/input — it's expected.
      const meta = event.metaKey || event.ctrlKey;
      if (meta && event.key.toLowerCase() === "z") {
        event.preventDefault();
        if (event.shiftKey) {
          redoCurrent();
        } else {
          undoCurrent();
        }
        return;
      }
      if (meta && event.key.toLowerCase() === "y") {
        event.preventDefault();
        redoCurrent();
        return;
      }

      if (isTyping(event.target)) {
        // Don't intercept regular typing — but still let Cmd+Enter pass to
        // the surrounding handler (none yet).
        return;
      }

      if (event.code === "Space") {
        event.preventDefault();
        togglePlayback();
        return;
      }

      if (event.code === "Escape") {
        if (timelineSelection) {
          setTimelineSelection(null);
        } else if (annotationSegment) {
          closeAnnotation();
        }
        return;
      }

      const key = event.key.toUpperCase();

      // Inline-tag insertion takes priority when we're in annotation mode,
      // because the tag keys serve dual duty (segment tag vs inline tag).
      if (annotationSegment) {
        const inlineMeta = inlineTagByKey.get(key);
        if (inlineMeta) {
          if (inlineMeta.kind === "bracket" || timelineSelection) {
            event.preventDefault();
            applyInlineTag(inlineMeta.tag, inlineMeta.kind);
            return;
          }
          // Fall through to segment tag toggle when no selection.
        }

        // 1-7: emotion shortcuts (single emotion replacement is friendlier
        // for keyboard-only annotation than additive toggling).
        const numKey = event.key;
        if (/^[1-7]$/.test(numKey)) {
          event.preventDefault();
          const emotion = emotionOptions[Number(numKey) - 1];
          if (annotationSegment && emotion) {
            updateSegment(
              annotationSegment.id,
              { emotion: [emotion] },
              { instant: true },
            );
          }
          return;
        }
      }

      // Segment-level tag toggle (works in both main view and annotation
      // when no inline selection is active).
      const segTag = tagOptions.find((item) => item.key === key);
      if (segTag) {
        event.preventDefault();
        toggleTagOnActive(segTag.value);
        return;
      }

      if (key === "J") {
        event.preventDefault();
        moveSelection(1);
      } else if (key === "K") {
        event.preventDefault();
        moveSelection(-1);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    annotationSegment,
    selectedSegmentId,
    visibleSegments,
    playbackPath,
    isPlaying,
    playbackCurrentMs,
    timelineSelection,
    segments,
  ]);

  function applyPlaybackState(state: PlaybackState) {
    setPlaybackCurrentMs(state.positionMs);
    if (state.durationMs) setPlaybackDurationMs(state.durationMs);
    setIsPlaying(state.isPlaying);
  }

  async function chooseFolder() {
    const selected = await openDialog({ directory: true, multiple: false });
    if (typeof selected === "string") setFolderPath(selected);
  }

  async function chooseOutputFolder() {
    const selected = await openDialog({ directory: true, multiple: false });
    if (typeof selected === "string") setOutputPath(selected);
  }

  async function chooseExportJsonl() {
    const selected = await saveDialog({
      filters: [{ name: "JSONL", extensions: ["jsonl"] }],
      defaultPath: exportJsonlPath || "export.jsonl",
    });
    if (typeof selected === "string") setExportJsonlPath(selected);
  }

  async function scanFolder() {
    if (!folderPath.trim()) {
      pushToast({ variant: "warning", title: "请先选择音频文件夹" });
      return;
    }
    setBusy(true);
    setAnnotationSegmentId("");
    setSelectedSegmentId("");
    setStatusMsg("正在扫描音频…");
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
      const result = await ipc.scanProjectFolder({
        folderPath,
        manifestPath: null,
        outputPath: outputPath.trim() || null,
      });
      setScan(result);
      setSelectedAudioId(result.audioFiles[0]?.id ?? "");

      // Hydrate from existing project.json so previous cut/recognize/annotate
      // work isn't lost when the user re-scans the same folder.
      const existing = result.existingProject ?? null;
      const restoredSegments: SegmentRecord[] = (existing?.segments ?? []).filter(
        (segment) =>
          // Drop stale entries whose audio file no longer maps to a scanned
          // source (manual file moves shouldn't keep showing dead segments).
          result.audioFiles.some((audio) => audio.path === segment.sourcePath),
      );
      setSegments(restoredSegments);
      if (existing?.config) setConfig(existing.config);
      if (existing?.systemPrompt) {
        updateSettings({ systemPrompt: existing.systemPrompt });
      }

      const cutSourcePaths = new Set(
        restoredSegments.map((segment) => segment.sourcePath),
      );
      const audioToCut = result.audioFiles.filter(
        (audio) => !cutSourcePaths.has(audio.path),
      );

      let cutCount = 0;
      let recognizedCount = 0;

      if (!REVIEW_ONLY && autoCutAfterScan && audioToCut.length > 0) {
        const newSegments = await cutAudioBatch(result, audioToCut);
        const merged = [...restoredSegments, ...newSegments];
        setSegments(merged);
        cutCount = newSegments.length;
        if (autoRecognizeAfterCut) {
          // Only recognize segments whose phonetic text is still empty so we
          // don't burn cycles re-running Whisper / Ollama on done work.
          const targets = merged.filter(
            (segment) => !segment.phoneticText.trim(),
          );
          if (targets.length > 0) {
            recognizedCount = await recognizeDraft(targets, {
              manageBusy: false,
              overwrite: false,
              label: "自动识别",
            });
          }
        }
      }

      const restoredCount = restoredSegments.length;
      const restoredDone = restoredSegments.filter((segment) =>
        segment.phoneticText.trim(),
      ).length;
      const detail = [
        `${result.audioFiles.length} 个音频`,
        existing
          ? `恢复进度：${restoredCount} 段（已标 ${restoredDone}）`
          : null,
        cutCount > 0 ? `新增切割 ${cutCount} 段` : null,
        recognizedCount > 0 ? `识别 ${recognizedCount} 段` : null,
      ]
        .filter(Boolean)
        .join("，");
      showSuccess(existing ? "扫描完成 · 已恢复进度" : "扫描完成", detail);
    } catch (error) {
      showError("扫描失败", error);
    } finally {
      setProgress((current) => ({ ...current, visible: false }));
      setBusy(false);
    }
  }

  async function cutAudioBatch(
    targetScan: ProjectScan,
    audioList?: AudioFileInfo[],
  ) {
    const sources = audioList ?? targetScan.audioFiles;
    const created: SegmentRecord[] = [];
    for (const [index, audio] of sources.entries()) {
      setSelectedAudioId(audio.id);
      setStatusMsg(`切割 ${index + 1}/${sources.length}：${audio.fileName}`);
      setProgress({
        visible: true,
        label: "自动切割",
        detail: audio.fileName,
        current: index,
        total: sources.length,
        indeterminate: true,
      });
      await waitForPaint();
      const newSegs = await ipc.cutAudioFile({
        inputPath: audio.path,
        segmentsDir: targetScan.segmentsDir,
        config,
        role: audio.role ?? null,
        originalText: audio.matchedText ?? "",
        emotion: audio.matchedEmotion ?? [],
      });
      created.push(...newSegs);
      setSegments((current) => [
        ...current.filter((segment) => segment.sourcePath !== audio.path),
        ...newSegs,
      ]);
      setProgress({
        visible: true,
        label: "自动切割",
        detail: `${audio.fileName} → ${newSegs.length} 段`,
        current: index + 1,
        total: sources.length,
      });
      await waitForPaint();
    }
    return created;
  }

  async function cutAll() {
    if (!scan || scan.audioFiles.length === 0) return;
    setBusy(true);
    setAnnotationSegmentId("");
    setSelectedSegmentId("");
    try {
      const cutSourcePaths = new Set(segments.map((segment) => segment.sourcePath));
      const audioToCut = scan.audioFiles.filter(
        (audio) => !cutSourcePaths.has(audio.path),
      );
      if (audioToCut.length === 0) {
        pushToast({
          variant: "info",
          title: "全部音频已切割",
          detail: "如需重切某条，点该音频右侧的「切割」按钮覆盖",
        });
        return;
      }
      const newSegments = await cutAudioBatch(scan, audioToCut);
      const recognizedCount = autoRecognizeAfterCut
        ? await recognizeDraft(
            newSegments.filter((segment) => !segment.phoneticText.trim()),
            {
              manageBusy: false,
              overwrite: false,
              label: "自动识别",
            },
          )
        : 0;
      showSuccess(
        "切割完成",
        `新增 ${newSegments.length} 段${
          autoRecognizeAfterCut ? `，识别 ${recognizedCount} 段` : ""
        }`,
      );
    } catch (error) {
      showError("切割失败", error);
    } finally {
      setProgress((current) => ({ ...current, visible: false }));
      setBusy(false);
    }
  }

  async function cutOne(audio: AudioFileInfo) {
    if (!scan) return;
    setBusy(true);
    selectAudio(audio);
    setStatusMsg(`正在切割：${audio.fileName}`);
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
      const created = await ipc.cutAudioFile({
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
      showSuccess(
        `切割：${audio.fileName}`,
        `${created.length} 段${
          autoRecognizeAfterCut ? `，识别 ${recognizedCount} 段` : ""
        }`,
      );
    } catch (error) {
      showError(`切割失败 ${audio.fileName}`, error);
    } finally {
      setProgress((current) => ({ ...current, visible: false }));
      setBusy(false);
    }
  }

  async function recognizeDraft(
    targetSegments: SegmentRecord[],
    options: {
      manageBusy: boolean;
      overwrite: boolean;
      label: string;
      overwriteCache?: boolean;
    },
  ) {
    if (!scan || targetSegments.length === 0) {
      pushToast({ variant: "warning", title: "没有可识别的切割片段" });
      return 0;
    }
    const { manageBusy, overwrite, label, overwriteCache } = options;
    if (manageBusy) setBusy(true);
    const batchSize = Math.max(1, settings.batchSize);
    let completed = 0;
    let recognizedCount = 0;
    setProgress({
      visible: true,
      label,
      detail: `准备识别 ${targetSegments.length} 段（${settings.whisperModel}${overwriteCache ? " · 跳过缓存" : ""}）`,
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
          detail: settings.useLlm
            ? `Whisper + Ollama 改写 ${completed + 1}-${completed + batch.length}/${targetSegments.length}`
            : `Whisper 识别 ${completed + 1}-${completed + batch.length}/${targetSegments.length}`,
          current: completed,
          total: targetSegments.length,
        });
        await waitForPaint();

        const results = await ipc.recognizeSegments({
          projectDir: scan.projectDir,
          segments: batch,
          options: {
            whisperModel: settings.whisperModel,
            initialPrompt: settings.whisperInitialPrompt,
            useCache: settings.useAsrCache,
            overwriteCache: overwriteCache ?? false,
            useLlm: settings.useLlm,
            ollamaUrl: settings.ollamaUrl,
            ollamaModel: settings.ollamaModel,
            llmPrompt: settings.llmPrompt,
          },
        });
        const resultMap = new Map(
          results
            .map((item) => [item.segmentId, item] as const)
            .filter(([, r]) => normalizeRecognizedText(r.text)),
        );
        recognizedCount += resultMap.size;

        // Update React state AND capture the merged snapshot so we can
        // persist it to disk on the same tick. Without this checkpoint a
        // crash / hot-reload mid-recognition loses every batch processed
        // since the last manual save (auto-save is suppressed by busy).
        let snapshot: SegmentRecord[] = [];
        setSegments((current) => {
          const updated = current.map((segment) => {
            const result = resultMap.get(segment.id);
            if (!result) return segment;
            const text = normalizeRecognizedText(result.text);
            const next: SegmentRecord = {
              ...segment,
              originalText: overwrite ? text : segment.originalText || text,
              phoneticText: overwrite ? text : segment.phoneticText || text,
            };
            if (result.emotion && (overwrite || segment.emotion.length === 0)) {
              next.emotion = [result.emotion];
            }
            if (result.tags?.length && (overwrite || segment.tags.length === 0)) {
              next.tags = result.tags;
            }
            return next;
          });
          snapshot = updated;
          return updated;
        });
        completed += batch.length;
        setProgress({
          visible: true,
          label,
          detail: `已完成 ${completed}/${targetSegments.length}`,
          current: completed,
          total: targetSegments.length,
        });
        // Fire-and-forget but await briefly so a fast loop doesn't pile up
        // pending writes; with project.json typically < 500KB this is well
        // under 50ms on local SSD.
        await saveProjectSnapshot(snapshot);
        await waitForPaint();
      }
      return recognizedCount;
    } catch (error) {
      showError("识别失败", error);
      return 0;
    } finally {
      setProgress((current) => ({ ...current, visible: false }));
      if (manageBusy) setBusy(false);
    }
  }

  async function recognizeVisibleDraft() {
    const pool = visibleSegments.length > 0 ? visibleSegments : segments;
    const target = pool.filter((segment) => !segment.phoneticText.trim());
    if (target.length === 0) {
      pushToast({
        variant: "info",
        title: "无待识别片段",
        detail: "当前范围内的所有片段都有文本了。需要重跑请用「重新识别」",
      });
      return;
    }
    const recognizedCount = await recognizeDraft(target, {
      manageBusy: true,
      overwrite: false,
      label: settings.useLlm ? "识别 + 方言改写" : "Whisper 识别",
    });
    if (recognizedCount > 0) {
      showSuccess(
        settings.useLlm ? "识别 + 改写完成" : "Whisper 识别完成",
        `${recognizedCount} 段`,
      );
    }
  }

  /**
   * Project-wide one-click: find every cut segment whose phoneticText is
   * still empty (regardless of which source audio is currently selected
   * in the left pane) and run the full Whisper + Ollama pipeline on them.
   */
  async function recognizeAllPending() {
    const target = segments.filter((segment) => !segment.phoneticText.trim());
    if (target.length === 0) {
      pushToast({
        variant: "info",
        title: "全部片段已识别",
        detail: `共 ${segments.length} 段，无待识别的。需要重跑用「重新识别」`,
      });
      return;
    }
    const total = segments.length;
    const done = total - target.length;
    pushToast({
      variant: "info",
      title: `开始跨音频批量识别`,
      detail: `项目共 ${total} 段，已识别 ${done} 段；本次处理剩余 ${target.length} 段`,
    });
    const recognizedCount = await recognizeDraft(target, {
      manageBusy: true,
      overwrite: false,
      label: settings.useLlm ? "全部待识别 + 方言改写" : "全部待识别（Whisper）",
    });
    if (recognizedCount > 0) {
      showSuccess(
        "全部待识别完成",
        `新增 ${recognizedCount} 段；项目累计 ${done + recognizedCount}/${total}`,
      );
    }
  }

  /**
   * Force-re-run recognition on every visible segment, overwriting any
   * existing text and bypassing the ASR cache so Whisper actually re-runs.
   * Useful after switching Whisper model or LLM prompt.
   */
  async function reRecognizeVisible() {
    const target = visibleSegments.length > 0 ? visibleSegments : segments;
    if (target.length === 0) return;
    const ok = window.confirm(
      `将对 ${target.length} 段强制重跑 Whisper${settings.useLlm ? " + Ollama 改写" : ""}，覆盖现有文本与情绪/标签。\n\n继续？`,
    );
    if (!ok) return;
    const count = await recognizeDraft(target, {
      manageBusy: true,
      overwrite: true,
      overwriteCache: true,
      label: "重新识别 + 改写",
    });
    if (count > 0) {
      showSuccess("重新识别完成", `${count} 段已覆盖`);
    }
  }

  /**
   * Batch LLM polish: re-run only the Ollama dialect rewrite on existing
   * phoneticText. Skips Whisper entirely. Useful after editing the LLM
   * prompt or switching Ollama model — much faster than full re-recognize.
   */
  async function repolishVisibleWithLlm() {
    if (!settings.useLlm) {
      pushToast({
        variant: "warning",
        title: "请先在设置抽屉里启用 Ollama 后处理",
      });
      return;
    }
    const pool = visibleSegments.length > 0 ? visibleSegments : segments;
    const target = pool.filter((segment) => segment.phoneticText.trim());
    if (target.length === 0) {
      pushToast({
        variant: "info",
        title: "没有可润色的片段",
        detail: "请先识别或手动写入文本",
      });
      return;
    }
    setBusy(true);
    setProgress({
      visible: true,
      label: "AI 重打标签",
      detail: `Ollama (${settings.ollamaModel}) 重新分析 ${target.length} 段`,
      current: 0,
      total: target.length,
    });
    let completed = 0;
    let updated = 0;
    let failed = 0;
    try {
      for (const segment of target) {
        setProgress({
          visible: true,
          label: "AI 重打标签",
          detail: `${completed + 1}/${target.length} · ${segment.segmentFileName}`,
          current: completed,
          total: target.length,
        });
        await waitForPaint();
        try {
          const result = await ipc.polishTextWithLlm({
            text: segment.phoneticText,
            role: segment.role,
            hint: segment.originalText,
            ollamaUrl: settings.ollamaUrl,
            ollamaModel: settings.ollamaModel,
            llmPrompt: settings.llmPrompt,
          });
          const patch: Partial<SegmentRecord> = {};
          const cleaned = normalizeRecognizedText(result.text);
          if (cleaned) patch.phoneticText = cleaned;
          if (result.emotion) patch.emotion = [result.emotion];
          if (result.tags?.length) patch.tags = result.tags;
          if (Object.keys(patch).length > 0) {
            updateSegment(segment.id, patch, { instant: true });
            updated += 1;
          }
        } catch {
          failed += 1;
        }
        completed += 1;
      }
      showSuccess(
        "AI 重打标签完成",
        `${updated} 段更新${failed > 0 ? `，${failed} 段失败` : ""}`,
      );
    } finally {
      setProgress((current) => ({ ...current, visible: false }));
      setBusy(false);
    }
  }

  async function recognizeCurrentSegment() {
    if (!annotationSegment) return;
    const recognizedCount = await recognizeDraft([annotationSegment], {
      manageBusy: true,
      overwrite: true,
      label: settings.useLlm ? "识别 + 改写本段" : "Whisper 识别本段",
    });
    if (recognizedCount > 0) {
      showSuccess("识别完成", annotationSegment.segmentFileName);
    }
  }

  async function polishCurrentSegmentOnly() {
    if (!annotationSegment) return;
    if (!settings.useLlm) {
      pushToast({ variant: "warning", title: "请先在设置中启用 Ollama 后处理" });
      return;
    }
    setBusy(true);
    setProgress({
      visible: true,
      label: "Ollama 润色",
      detail: annotationSegment.segmentFileName,
      current: 0,
      total: 1,
      indeterminate: true,
    });
    try {
      const result = await ipc.polishTextWithLlm({
        text: annotationSegment.phoneticText,
        role: annotationSegment.role,
        hint: annotationSegment.originalText,
        ollamaUrl: settings.ollamaUrl,
        ollamaModel: settings.ollamaModel,
        llmPrompt: settings.llmPrompt,
      });
      const patch: Partial<SegmentRecord> = {
        phoneticText: normalizeRecognizedText(result.text),
      };
      if (result.emotion) patch.emotion = [result.emotion];
      if (result.tags?.length) patch.tags = result.tags;
      updateSegment(annotationSegment.id, patch);
      showSuccess("已润色", result.text.slice(0, 24));
    } catch (error) {
      showError("Ollama 润色失败", error);
    } finally {
      setProgress((current) => ({ ...current, visible: false }));
      setBusy(false);
    }
  }

  /**
   * Best-effort persistence used as a fast checkpoint between recognition
   * batches. Does NOT toggle busy and never throws — if disk is full or the
   * file is locked, we just swallow it and let the next checkpoint try
   * again. Pass the snapshot explicitly so we can save state computed
   * inside a setSegments updater.
   */
  async function saveProjectSnapshot(snapshotSegments: SegmentRecord[]) {
    if (!scan) return;
    const payload: ProjectFile = {
      version: 2,
      savedAt: new Date().toISOString(),
      rootPath: scan.rootPath,
      projectDir: scan.projectDir,
      segmentsDir: scan.segmentsDir,
      config,
      audioFiles: scan.audioFiles,
      manifestRecords: scan.manifestRecords,
      segments: snapshotSegments,
      systemPrompt: settings.systemPrompt,
    };
    try {
      await ipc.saveProjectFile({ projectDir: scan.projectDir, payload });
    } catch {
      // Swallow — auto-save will retry once busy clears, and the next
      // recognition batch will overwrite this snapshot anyway.
    }
  }

  async function saveProject() {
    if (!scan) return;
    setBusy(true);
    try {
      const payload: ProjectFile = {
        version: 2,
        savedAt: new Date().toISOString(),
        rootPath: scan.rootPath,
        projectDir: scan.projectDir,
        segmentsDir: scan.segmentsDir,
        config,
        audioFiles: scan.audioFiles,
        manifestRecords: scan.manifestRecords,
        segments,
        systemPrompt: settings.systemPrompt,
      };
      const path = await ipc.saveProjectFile({
        projectDir: scan.projectDir,
        payload,
      });
      showSuccess("已保存", path);
    } catch (error) {
      showError("保存失败", error);
    } finally {
      setBusy(false);
    }
  }

  async function loadProject() {
    if (!scan) return;
    setBusy(true);
    try {
      const project = await ipc.loadProjectFile({ projectDir: scan.projectDir });
      setConfig(project.config ?? defaultCutConfig);
      setSegments(project.segments ?? []);
      setSelectedSegmentId("");
      setAnnotationSegmentId("");
      if (project.systemPrompt) {
        updateSettings({ systemPrompt: project.systemPrompt });
      }
      showSuccess("已打开项目存档", `${project.segments?.length ?? 0} 段`);
    } catch (error) {
      showError("打开失败", error);
    } finally {
      setBusy(false);
    }
  }

  async function exportJsonl() {
    if (!scan) return;
    setBusy(true);
    try {
      const path = await ipc.exportSegmentsJsonl({
        projectDir: scan.projectDir,
        outputPath: exportJsonlPath.trim() || null,
        segments,
        options: {
          systemPrompt: settings.systemPrompt,
          pairUserAssistant: settings.pairUserAssistant,
          useSourceAudioForUser: true,
          audioFilePrefix: settings.audioFilePrefix,
          inputRoot: scan.rootPath,
        },
      });
      if (!exportJsonlPath.trim()) setExportJsonlPath(path);
      showSuccess("已导出", path);
    } catch (error) {
      showError("导出失败", error);
    } finally {
      setBusy(false);
    }
  }

  async function exportBundle() {
    if (!scan) return;
    if (segments.length === 0) {
      pushToast({ variant: "warning", title: "没有可打包的切割片段" });
      return;
    }
    const target = await openDialog({
      directory: true,
      multiple: false,
      title: "选择打包目录（建议新建空目录或单独的输出位置）",
    });
    if (typeof target !== "string" || !target) return;

    setBusy(true);
    setProgress({
      visible: true,
      label: "一键打包",
      detail: `复制 ${segments.length} 段 + 源音频到 ${target}`,
      current: 0,
      total: segments.length,
      indeterminate: true,
    });
    try {
      await waitForPaint();
      const result = await ipc.exportDatasetBundle({
        bundleDir: target,
        segments,
        includeSourceAudio: true,
        options: {
          systemPrompt: settings.systemPrompt,
          pairUserAssistant: settings.pairUserAssistant,
          useSourceAudioForUser: true,
          audioFilePrefix: settings.audioFilePrefix,
        },
      });
      const sizeMb = (result.totalBytes / 1024 / 1024).toFixed(1);
      showSuccess(
        "打包完成",
        `${result.segmentCount} 段 · ${result.sourceAudioCount} 源音频 · ${sizeMb} MB`,
      );
      pushToast({
        variant: "info",
        title: "JSONL 已生成",
        detail: result.jsonlPath,
      });
    } catch (error) {
      showError("打包失败", error);
    } finally {
      setProgress((current) => ({ ...current, visible: false }));
      setBusy(false);
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
    void ipc.stopAudio().then(applyPlaybackState).catch(() => {
      setIsPlaying(false);
      setPlaybackCurrentMs(0);
    });
  }

  /**
   * Sweep every segment's phoneticText and strip any inline tag whose name
   * is NOT in the current `settings.inlineTags` dictionary. Useful after the
   * spec changes (e.g. when `<pause/>` was retired from the legal set but
   * already-recognised segments still carry it). Preserves the wrapped
   * content; only the tag tokens themselves are removed.
   */
  function purgeOrphanInlineTags() {
    if (!scan || segments.length === 0) {
      pushToast({ variant: "warning", title: "没有可扫描的片段" });
      return;
    }
    const legal = new Set(
      inlineTags.map((item) => item.tag.toLowerCase()),
    );
    const tagRe =
      /<\/?([a-zA-Z][a-zA-Z0-9-]*)\s*\/?>|\[([a-zA-Z][a-zA-Z0-9-]*)\]/g;

    let totalRemoved = 0;
    const seenOrphans = new Map<string, number>();
    const cleaned = segments.map((segment) => {
      const text = segment.phoneticText;
      if (!text) return segment;
      let perSegRemoved = 0;
      const next = text.replace(tagRe, (match, openClose, bracket) => {
        const name = (openClose ?? bracket ?? "").toLowerCase();
        if (legal.has(name)) return match;
        perSegRemoved += 1;
        seenOrphans.set(name, (seenOrphans.get(name) ?? 0) + 1);
        return "";
      });
      if (perSegRemoved === 0) return segment;
      totalRemoved += perSegRemoved;
      return { ...segment, phoneticText: next };
    });

    if (totalRemoved === 0) {
      pushToast({
        variant: "info",
        title: "没有发现废弃标签",
        detail: `所有 inline 标签都在当前字典里（共 ${legal.size} 个合法标签）`,
      });
      return;
    }

    const orphanList = [...seenOrphans.entries()]
      .map(([name, n]) => `${name}×${n}`)
      .join("、");
    if (
      !window.confirm(
        `将从 ${segments.length} 段中清除 ${totalRemoved} 处废弃 inline 标签：\n\n${orphanList}\n\n继续？（操作可撤销 ⌘Z）`,
      )
    ) {
      return;
    }

    setSegments(cleaned);
    void saveProjectSnapshot(cleaned);
    showSuccess("已清理废弃标签", `去除 ${totalRemoved} 处 · ${orphanList}`);
  }

  /** Push a snapshot of `prev` to the per-segment undo stack. */
  function pushHistory(prev: SegmentRecord, instant = false) {
    const now = Date.now();
    const last = lastSnapshotRef.current;
    if (!instant && last && last.id === prev.id && now - last.time < 700) {
      // Within the typing debounce window — skip pushing another snapshot.
      return;
    }
    lastSnapshotRef.current = { id: prev.id, time: now };
    const list = undoRef.current.get(prev.id) ?? [];
    list.push(prev);
    if (list.length > 100) list.shift();
    undoRef.current.set(prev.id, list);
    // Any new edit invalidates the redo stack for that segment.
    redoRef.current.set(prev.id, []);
  }

  function updateSegment(
    id: string,
    patch: Partial<SegmentRecord>,
    options?: { instant?: boolean },
  ) {
    setSegments((current) => {
      const prev = current.find((s) => s.id === id);
      if (prev) pushHistory(prev, options?.instant ?? false);
      return current.map((segment) =>
        segment.id === id ? { ...segment, ...patch } : segment,
      );
    });
  }

  function undoCurrent() {
    const targetId = annotationSegmentId || selectedSegmentId;
    if (!targetId) return false;
    const list = undoRef.current.get(targetId);
    if (!list || list.length === 0) return false;
    const prev = list.pop()!;
    setSegments((current) => {
      const cur = current.find((s) => s.id === targetId);
      if (cur) {
        const redoList = redoRef.current.get(targetId) ?? [];
        redoList.push(cur);
        redoRef.current.set(targetId, redoList);
      }
      return current.map((s) => (s.id === targetId ? prev : s));
    });
    pushToast({
      variant: "info",
      title: "已撤销",
      detail: targetId,
    });
    return true;
  }

  function redoCurrent() {
    const targetId = annotationSegmentId || selectedSegmentId;
    if (!targetId) return false;
    const list = redoRef.current.get(targetId);
    if (!list || list.length === 0) return false;
    const next = list.pop()!;
    setSegments((current) => {
      const cur = current.find((s) => s.id === targetId);
      if (cur) {
        const undoList = undoRef.current.get(targetId) ?? [];
        undoList.push(cur);
        undoRef.current.set(targetId, undoList);
      }
      return current.map((s) => (s.id === targetId ? next : s));
    });
    return true;
  }

  /**
   * Apply an inline tag to the current segment.
   *
   * Two forms — disambiguated by the `kind` argument or, if absent, inferred
   * from the metadata table:
   *
   * `paired`  — `<tag>...</tag>` wraps the active selection. Toggling: if
   *             the whole selection is already inside that tag, the tag is
   *             stripped instead. Without a selection, the call is a no-op
   *             with a toast nudge.
   *
   * `bracket` — `[tag]` is inserted as a discrete event marker at the
   *             selection start (or, if no selection, at the end of the
   *             text). Same shape as the spec's paralinguistic markers.
   */
  function applyInlineTag(tag: string, kind?: "paired" | "bracket") {
    const targetId = annotationSegmentId;
    if (!targetId) return false;
    const segment = segments.find((s) => s.id === targetId);
    if (!segment) return false;
    const text = segment.phoneticText;

    // Resolve effective kind: explicit > metadata lookup with selection
    // fallback for the dual-purpose laugh keybinding.
    let effectiveKind: "paired" | "bracket" | undefined = kind;
    if (!effectiveKind) {
      const matches = inlineTags.filter((item) => item.tag === tag);
      const paired = matches.find((m) => m.kind === "paired");
      const bracket = matches.find((m) => m.kind === "bracket");
      if (timelineSelection && paired) effectiveKind = "paired";
      else if (bracket) effectiveKind = "bracket";
      else if (paired) effectiveKind = "paired";
    }
    if (!effectiveKind) return false;

    if (effectiveKind === "bracket") {
      const pos = timelineSelection?.startRaw ?? text.length;
      const next = `${text.slice(0, pos)}[${tag}]${text.slice(pos)}`;
      updateSegment(targetId, { phoneticText: next }, { instant: true });
      return true;
    }

    // Paired path
    if (!timelineSelection) {
      pushToast({
        variant: "warning",
        title: `请先在波形下选中要标记的字`,
        detail: "拖选一段或单击一个字均可，再点这个按钮包裹",
      });
      return false;
    }

    const { startRaw, endRaw, commonTags } = timelineSelection;

    if (commonTags.includes(tag)) {
      // Toggle: strip the wrapping <tag>...</tag> closest to the selection.
      const open = `<${tag}>`;
      const close = `</${tag}>`;
      const before = text.slice(0, startRaw);
      const after = text.slice(endRaw);
      const lastOpen = before.lastIndexOf(open);
      const firstClose = after.indexOf(close);
      if (lastOpen === -1 || firstClose === -1) return false;
      const newBefore =
        before.slice(0, lastOpen) + before.slice(lastOpen + open.length);
      const newAfter =
        after.slice(0, firstClose) + after.slice(firstClose + close.length);
      updateSegment(
        targetId,
        {
          phoneticText: newBefore + text.slice(startRaw, endRaw) + newAfter,
        },
        { instant: true },
      );
      setTimelineSelection(null);
      return true;
    }

    const inner = text.slice(startRaw, endRaw);
    if (!inner) return false;
    const next = `${text.slice(0, startRaw)}<${tag}>${inner}</${tag}>${text.slice(endRaw)}`;
    updateSegment(targetId, { phoneticText: next }, { instant: true });
    setTimelineSelection(null);
    return true;
  }

  function toggleTagOnActive(tag: string) {
    const target = annotationSegment ?? segments.find((s) => s.id === selectedSegmentId);
    if (!target) return;
    const next = target.tags.includes(tag)
      ? target.tags.filter((t) => t !== tag)
      : [...target.tags, tag];
    updateSegment(target.id, { tags: next });
  }

  function moveSelection(direction: number) {
    if (!visibleSegments.length) return;
    const currentId = annotationSegmentId || selectedSegmentId;
    const idx = visibleSegments.findIndex((s) => s.id === currentId);
    const nextIdx = clamp(
      idx === -1 ? 0 : idx + direction,
      0,
      visibleSegments.length - 1,
    );
    const next = visibleSegments[nextIdx];
    openAnnotation(next, isPlaying);
  }

  function togglePlayback() {
    if (!playbackPath) return;
    if (isPlaying) {
      void ipc
        .pauseAudio()
        .then(applyPlaybackState)
        .catch((error) => setPlaybackStatus(`暂停失败：${String(error)}`));
    } else {
      void ipc
        .playAudio({ path: playbackPath, positionMs: playbackCurrentMs })
        .then(applyPlaybackState)
        .catch((error) => setPlaybackStatus(`播放失败：${String(error)}`));
    }
  }

  function seekToRatio(ratio: number) {
    if (!activeDurationMs) return;
    const positionMs = clamp(ratio * activeDurationMs, 0, activeDurationMs);
    setPlaybackCurrentMs(positionMs);
    if (playbackPath && isPlaying) {
      void ipc
        .playAudio({ path: playbackPath, positionMs: Math.round(positionMs) })
        .then(applyPlaybackState)
        .catch((error) => setPlaybackStatus(`跳转失败：${String(error)}`));
    }
  }

  if (annotationSegment) {
    return (
      <>
        <AnnotationView
          segment={annotationSegment}
          playbackPath={playbackPath}
          playbackStatus={playbackStatus}
          durationMs={activeDurationMs}
          currentMs={playbackCurrentMs}
          isPlaying={isPlaying}
          busy={busy}
          llmEnabled={settings.useLlm}
          progress={progress}
          selection={timelineSelection}
          inlineTags={inlineTags}
          segmentTags={tagOptions}
          emotions={emotionOptions}
          onSelectionChange={setTimelineSelection}
          onApplyInlineTag={applyInlineTag}
          onClose={closeAnnotation}
          onTogglePlay={togglePlayback}
          onSeekRatio={seekToRatio}
          onUpdate={(patch) => updateSegment(annotationSegment.id, patch)}
          onRecognizeOne={recognizeCurrentSegment}
          onPolishOnly={polishCurrentSegmentOnly}
          onSave={saveProject}
          onExport={exportJsonl}
          onPrev={() => moveSelection(-1)}
          onNext={() => moveSelection(1)}
        />
        <SettingsDrawer
          open={settingsOpen}
          settings={settings}
          onChange={updateSettings}
          onResetPrompt={resetLlmPrompt}
          onPurgeOrphanTags={purgeOrphanInlineTags}
          onClose={() => setSettingsOpen(false)}
        />
        <ShortcutOverlay
          open={shortcutOpen}
          onClose={() => setShortcutOpen(false)}
        />
        <ToastStack toasts={toasts} onDismiss={dismissToast} />
      </>
    );
  }

  return (
    <>
      <main className="app-shell">
        <Topbar
          status={status}
          busy={busy}
          hasError={hasError}
          hasProject={Boolean(scan)}
          hasSegments={segments.length > 0}
          theme={settings.theme}
          onCycleTheme={cycleTheme}
          onOpenSettings={() => setSettingsOpen(true)}
          onOpenShortcuts={() => setShortcutOpen(true)}
          onSave={saveProject}
          onLoad={loadProject}
          onExport={exportJsonl}
          onExportBundle={exportBundle}
        />
        <div className="app-body">
          <SetupBand
            folderPath={folderPath}
            outputPath={outputPath}
            exportJsonlPath={exportJsonlPath}
            autoCutAfterScan={autoCutAfterScan}
            busy={busy}
            onFolderPathChange={setFolderPath}
            onOutputPathChange={setOutputPath}
            onExportPathChange={setExportJsonlPath}
            onChooseFolder={chooseFolder}
            onChooseOutput={chooseOutputFolder}
            onChooseExport={chooseExportJsonl}
            onAutoCutChange={setAutoCutAfterScan}
            onScan={scanFolder}
          />
          {!REVIEW_ONLY && (
            <ConfigBand
              config={config}
              onConfigChange={setConfig}
              presets={settings.cutPresets}
              onPresetsChange={(next) => updateSettings({ cutPresets: next })}
              autoRecognizeAfterCut={autoRecognizeAfterCut}
              onAutoRecognizeChange={setAutoRecognizeAfterCut}
              busy={busy}
              hasAudio={Boolean(scan && scan.audioFiles.length > 0)}
              hasSegments={segments.length > 0}
              llmEnabled={settings.useLlm}
              pendingCount={
                segments.filter((s) => !s.phoneticText.trim()).length
              }
              onCutAll={cutAll}
              onRecognizeVisible={recognizeVisibleDraft}
              onRecognizeAllPending={recognizeAllPending}
              onReRecognizeVisible={reRecognizeVisible}
              onRepolishVisible={repolishVisibleWithLlm}
            />
          )}
          {progress.visible && <ProgressPanel progress={progress} />}
          <MainView
            scan={scan}
            segments={segments}
            selectedAudio={selectedAudio}
            selectedAudioId={selectedAudioId}
            selectedSegmentId={selectedSegmentId}
            visibleSegments={visibleSegments}
            busy={busy}
            isPlaying={isPlaying}
            playbackPath={playbackPath}
            playbackStatus={playbackStatus}
            playbackCurrentMs={playbackCurrentMs}
            playbackDurationMs={playbackDurationMs}
            onSelectAudio={selectAudio}
            onCutOne={cutOne}
            onSelectSegment={(s) => openAnnotation(s, true)}
            onTogglePlay={togglePlayback}
            onSeekRatio={seekToRatio}
          />
        </div>
      </main>
      {dropping && (
        <div className="dropzone-overlay">
          <div className="dropzone-card">
            <Inbox size={32} />
            <span>松手以加载该文件夹</span>
          </div>
        </div>
      )}
      <SettingsDrawer
        open={settingsOpen}
        settings={settings}
        onChange={updateSettings}
        onResetPrompt={resetLlmPrompt}
          onPurgeOrphanTags={purgeOrphanInlineTags}
        onClose={() => setSettingsOpen(false)}
      />
      <ShortcutOverlay
        open={shortcutOpen}
        onClose={() => setShortcutOpen(false)}
      />
      <ToastStack toasts={toasts} onDismiss={dismissToast} />
    </>
  );
}

export default App;

// suppress unused
void Folder;
