import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
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
  // Reviewer-friendly fast modes: 1x default, 1.25x for catch-the-detail
  // listens, 1.5x for skim. Persists across the session, applies to
  // every play() call, and propagates live to the running sink.
  const [playbackSpeed, setPlaybackSpeed] = useState(1);
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
  // Set while a cancellable long-running task is active (currently only
  // `recognizeDraft`). When non-null, ProgressPanel renders a "停止" button
  // that calls this. Cleared in the task's `finally` block.
  const cancelHandlerRef = useRef<(() => void) | null>(null);
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
  // Safety guard: explicitly skip when segments is empty. Saving an empty
  // segments[] over a non-empty disk file is a destructive op (we hit this
  // bug already once — wiped 1212 segments). The explicit "保存" button is
  // the only path that can write empty segments, and we add user
  // confirmation there too if needed.
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
      // Match by absolute path first; fall back to filename so the segments
      // survive path differences (canonicalization, moved input folder,
      // cross-platform handoff, etc.). Re-anchor the matched segment's
      // sourcePath to the current scan's audio.path so all subsequent
      // operations (LLM polish, export) work without further path drift.
      const existing = result.existingProject ?? null;
      const audioByAbsPath = new Map(
        result.audioFiles.map((audio) => [audio.path, audio]),
      );
      const audioByFileName = new Map(
        result.audioFiles.map((audio) => [audio.fileName, audio]),
      );
      const rawSegments: SegmentRecord[] = existing?.segments ?? [];
      let droppedCount = 0;
      const restoredSegments: SegmentRecord[] = rawSegments
        .map((segment) => {
          let audio = audioByAbsPath.get(segment.sourcePath);
          if (!audio) {
            // Fallback to file name match — handles cases where the path
            // was canonicalized differently or the input folder was moved.
            audio = audioByFileName.get(segment.sourceFileName);
          }
          if (!audio) {
            droppedCount += 1;
            return null;
          }
          // Re-anchor to the current scan's path so downstream is consistent.
          if (audio.path !== segment.sourcePath) {
            return { ...segment, sourcePath: audio.path };
          }
          return segment;
        })
        .filter((segment): segment is SegmentRecord => segment !== null);

      // Safety guard: refuse to overlay an empty segments array on top of a
      // file that had non-empty segments. This was the bug class where one
      // bad scan wiped 1212 segments from disk.
      if (rawSegments.length > 0 && restoredSegments.length === 0) {
        showError(
          "已恢复的片段为零（可能路径不匹配）",
          `磁盘上有 ${rawSegments.length} 段但都对不上当前音频文件名。已自动备份 project.json 到 .backups/，然后保留磁盘原状不覆盖`,
        );
        // Backup AND skip setSegments — leave React state empty but don't
        // auto-save (the busy state we'll set + the empty-array detection
        // in our save layer will block the destructive overwrite).
        await ipc
          .backupProjectFile({ projectDir: result.projectDir })
          .catch(() => undefined);
        // Continue scanning but DO NOT touch segments state on disk.
        return;
      }

      if (droppedCount > 0) {
        pushToast({
          variant: "warning",
          title: `已忽略 ${droppedCount} 段失效片段`,
          detail: "这些段的源音频在当前文件夹找不到（可能被删除或重命名）",
        });
      }
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
      setBusy(true);
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
    // Guard: refuse to silently overwrite recognised segments. The user MUST
    // see a warning showing how much annotation work is about to be lost.
    const existing = segments.filter(
      (segment) => segment.sourcePath === audio.path,
    );
    const recognised = existing.filter((segment) =>
      segment.phoneticText.trim(),
    );
    if (recognised.length > 0) {
      const ok = window.confirm(
        `⚠️ 重切将丢失 ${recognised.length}/${existing.length} 段已识别/已标注内容（${audio.fileName}）。\n\n建议先「保存」做备份。\n\n继续重切吗？`,
      );
      if (!ok) return;
    }
    // Best-effort backup of the current project.json before the destructive
    // cut, so the user can hand-recover at worst by restoring the .bak file.
    if (existing.length > 0) {
      void backupProjectJson().catch(() => undefined);
    }
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
    let recognizedCount = 0;

    // Wire the cancel button to the backend's `cancel_recognize` command.
    // Workers check the flag at every loop iteration and exit cleanly;
    // the in-flight Whisper subprocess / LLM call finishes naturally
    // (5–10s typical) before the run returns. Already-cached ASR + any
    // already-polished segments are persisted, so a re-launch resumes
    // from where we stopped.
    let cancelRequested = false;
    cancelHandlerRef.current = () => {
      cancelRequested = true;
      void ipc.cancelRecognize().catch(() => {});
      setProgress((current) => ({
        ...current,
        detail: "正在停止…当前任务跑完后退出（约 5–10 秒）",
      }));
    };

    // Two-phase pipeline: Phase 1 runs Whisper for every uncached segment;
    // Phase 2 runs Ollama polish for every segment that hasn't been
    // polished yet (frontend filters by `emotion.length === 0`). Each
    // phase has its own progress counter and emits a `phase_done` event
    // at the boundary so the bar resets between phases.
    setProgress({
      visible: true,
      label,
      detail: `准备识别 ${targetSegments.length} 段（${settings.whisperModel}${overwriteCache ? " · 跳过缓存" : ""}）`,
      current: 0,
      total: targetSegments.length,
    });

    type SegmentEvent = {
      segmentId: string;
      // "asr"          — Whisper produced raw普通话 text for this segment
      // "polish_ok"    — Ollama polish succeeded
      // "polish_fail"  — Ollama polish failed (Whisper text preserved)
      // "phase_done"   — phase boundary; `text` carries which phase ended
      //                  ("asr" or "polish") so the progress bar can flip
      //                  the label and reset its counter.
      phase: "asr" | "polish_ok" | "polish_fail" | "phase_done";
      text: string;
      emotion: string | null;
      tags: string[];
      polishEndpoint?: string | null;
      polishModel?: string | null;
      cached: boolean;
      completed: number;
      total: number;
    };

    const polishStats = { ok: 0, fail: 0 };
    const lastSnapshotRef: { current: SegmentRecord[] | null } = { current: null };
    let unlisten: (() => void) | null = null;
    let saveTimer: number | null = null;

    const scheduleCheckpointSave = () => {
      if (saveTimer != null) window.clearTimeout(saveTimer);
      saveTimer = window.setTimeout(() => {
        if (lastSnapshotRef.current) {
          void saveProjectSnapshot(lastSnapshotRef.current);
        }
      }, 1500);
    };

    try {
      unlisten = await listen<SegmentEvent>(
        "recognize:segment_done",
        (event) => {
          const ev = event.payload;
          if (ev.phase === "polish_ok") polishStats.ok += 1;
          if (ev.phase === "polish_fail") polishStats.fail += 1;

          // Phase boundary — flip the label and reset the bar so the
          // user sees a fresh 0/N for the next phase. ev.text says which
          // phase just ended ("asr" or "polish").
          if (ev.phase === "phase_done") {
            if (ev.text === "asr") {
              setProgress({
                visible: true,
                label,
                detail: `Whisper 阶段完成 · 共 ${ev.completed} 段`,
                current: 0,
                total: 0,
                indeterminate: true,
              });
            }
            return;
          }

          setSegments((current) => {
            const updated = current.map((segment) => {
              if (segment.id !== ev.segmentId) return segment;
              const text = normalizeRecognizedText(ev.text);
              const next: SegmentRecord = {
                ...segment,
                originalText: overwrite ? text : segment.originalText || text,
                phoneticText: overwrite ? text : segment.phoneticText || text,
              };
              if (
                ev.phase === "polish_ok" &&
                ev.emotion &&
                (overwrite || segment.emotion.length === 0)
              ) {
                next.emotion = [ev.emotion];
              }
              if (
                ev.phase === "polish_ok" &&
                ev.tags?.length &&
                (overwrite || segment.tags.length === 0)
              ) {
                next.tags = ev.tags;
              }
              if (ev.polishEndpoint) next.lastPolishEndpoint = ev.polishEndpoint;
              if (ev.polishModel) next.lastPolishModel = ev.polishModel;
              return next;
            });
            lastSnapshotRef.current = updated;
            return updated;
          });

          // Pick the per-phase label. With the two-phase backend each
          // phase has its own counter, so all three event types can drive
          // the progress bar directly via ev.completed/ev.total.
          const phaseLabel =
            ev.phase === "asr"
              ? "Whisper 识别"
              : ev.phase === "polish_ok"
                ? "AI 改写 ✓"
                : "AI 改写 ⚠";
          recognizedCount = ev.completed;
          setProgress({
            visible: true,
            label,
            detail: `${phaseLabel} · ${ev.segmentId.split("_").slice(-1)[0]} · ${ev.completed}/${ev.total}`,
            current: ev.completed,
            total: ev.total,
          });
          scheduleCheckpointSave();
        },
      );

      await ipc.recognizeSegments({
        projectDir: scan.projectDir,
        segments: targetSegments,
        options: {
          whisperModel: settings.whisperModel,
          initialPrompt: settings.whisperInitialPrompt,
          useCache: settings.useAsrCache,
          overwriteCache: overwriteCache ?? false,
          useLlm: settings.useLlm,
          ollamaUrl: settings.ollamaUrl,
          ollamaExtraEndpoints: settings.ollamaExtraEndpoints,
          ollamaModel: settings.ollamaModel,
          llmPrompt: settings.llmPrompt,
          llmConcurrency: settings.llmConcurrency,
          whisperConcurrency: settings.whisperConcurrency,
        },
      });

      // Final flush — the debounced save may still be pending.
      if (saveTimer != null) window.clearTimeout(saveTimer);
      if (lastSnapshotRef.current) {
        await saveProjectSnapshot(lastSnapshotRef.current);
      }

      if (settings.useLlm && polishStats.fail > 0) {
        pushToast({
          variant: "warning",
          title: `${polishStats.ok}/${polishStats.ok + polishStats.fail} 段 LLM 成功`,
          detail: `${polishStats.fail} 段仅 Whisper 文本（端点失败）`,
        });
      }
      return recognizedCount;
    } catch (error) {
      showError("识别失败", error);
      return 0;
    } finally {
      if (saveTimer != null) window.clearTimeout(saveTimer);
      if (unlisten) unlisten();
      cancelHandlerRef.current = null;
      setProgress((current) => ({ ...current, visible: false }));
      if (manageBusy) setBusy(false);
      if (cancelRequested) {
        pushToast({
          variant: "info",
          title: "已停止识别",
          detail: "已完成的段落已缓存。下次点「全部待识别」会从中断处接着跑。",
        });
      }
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
   * Project-wide one-click: find every cut segment that hasn't completed
   * the full Whisper + Ollama pipeline and resume it.
   *
   * Pending criterion (any of):
   *   - `phoneticText` empty       → Whisper not done
   *   - `emotion` empty + LLM on   → polish not done (skipped via cancel
   *                                  or LLM was off when ASR ran)
   *
   * The backend handles each of these independently — Phase 1 skips
   * cache-hit segments, Phase 2 skips segments that already have an
   * emotion tag (unless overwriteCache is set). So sending a segment
   * that's only missing one phase is cheap.
   */
  async function recognizeAllPending() {
    const llmOn = settings.useLlm;
    const target = segments.filter((segment) => {
      const asrDone = segment.phoneticText.trim().length > 0;
      const polishDone = segment.emotion.length > 0;
      if (!asrDone) return true;
      if (llmOn && !polishDone) return true;
      return false;
    });
    if (target.length === 0) {
      pushToast({
        variant: "info",
        title: "全部片段已识别",
        detail: `共 ${segments.length} 段，无待处理的。需要重跑用「重新识别」`,
      });
      return;
    }
    const total = segments.length;
    const done = total - target.length;
    pushToast({
      variant: "info",
      title: `开始跨音频批量识别`,
      detail: `项目共 ${total} 段，已完成 ${done} 段；本次处理剩余 ${target.length} 段`,
    });
    const recognizedCount = await recognizeDraft(target, {
      manageBusy: true,
      overwrite: false,
      label: llmOn ? "全部待识别 + 方言改写" : "全部待识别（Whisper）",
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
   * Snapshot the current project.json to `<projectDir>/.backups/project.<ts>.json.bak`
   * before a destructive op (re-cut, force-recognize). Best-effort only.
   */
  async function backupProjectJson() {
    if (!scan) return;
    try {
      const dst = await ipc.backupProjectFile({ projectDir: scan.projectDir });
      if (dst) {
        pushToast({
          variant: "info",
          title: "已备份 project.json",
          detail: dst,
        });
      }
    } catch {
      // not fatal; user already saw the destructive-op confirm dialog
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
      // Auto-reveal in Finder/Explorer — saves the user the dance of
      // copy-pasting the path into a file manager. Failures are silent
      // (the toast already shows the path).
      void revealItemInDir(path).catch(() => {});
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
      // Same auto-reveal — the bundle directory is the more useful
      // target here, but revealing the JSONL inside it shows both at
      // once on most file managers.
      void revealItemInDir(result.jsonlPath).catch(() => {});
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

  /**
   * Migrate already-cut segment WAVs to the demo
   * `<base>_<NN>_<role>.wav` naming convention. Renames files on disk +
   * updates segmentPath/segmentFileName in project.json. Existing ASR
   * cache and emotion/tags state are preserved (only file names change).
   */
  async function migrateSegmentFilenames() {
    if (!scan || segments.length === 0) {
      pushToast({ variant: "warning", title: "没有可迁移的片段" });
      return;
    }
    if (
      !window.confirm(
        `将按 demo 命名规则重命名 ${segments.length} 个切割文件：\n\n` +
          `<base>_<NN>_<role>.wav\n\n` +
          `已识别 / 已改写状态保留。继续？`,
      )
    ) {
      return;
    }
    setBusy(true);
    try {
      const updated = await ipc.migrateSegmentFilenames({
        projectDir: scan.projectDir,
        segments,
      });
      setSegments(updated);
      await saveProjectSnapshot(updated);
      const renamed = updated.filter(
        (s, i) => s.segmentFileName !== segments[i].segmentFileName,
      ).length;
      showSuccess(
        "命名迁移完成",
        renamed === 0
          ? "所有片段已经是 demo 格式，无需迁移"
          : `${renamed}/${segments.length} 段已重命名`,
      );
    } catch (error) {
      showError("命名迁移失败", error);
    } finally {
      setBusy(false);
    }
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
      // After a clip plays through, currentMs sits at duration. Tapping
      // play again should restart from 0, not "play" 0 ms of audio.
      const atEnd =
        activeDurationMs > 0 && playbackCurrentMs >= activeDurationMs - 5;
      // Floor + Math.round here is defensive: seekToRatio stores a float
      // into playbackCurrentMs, and the Tauri command expects u64. Sending
      // a float triggers `invalid type: floating point ... expected u64`.
      const positionMs = atEnd ? 0 : Math.round(playbackCurrentMs);
      if (atEnd) setPlaybackCurrentMs(0);
      void ipc
        .playAudio({ path: playbackPath, positionMs })
        .then(applyPlaybackState)
        .catch((error) => setPlaybackStatus(`播放失败：${String(error)}`));
    }
  }

  function changePlaybackSpeed(speed: number) {
    setPlaybackSpeed(speed);
    // Apply to the live sink immediately. Backend will rebase its
    // wall-clock baseline so position math stays correct mid-playback.
    void ipc
      .setPlaybackSpeed({ speed })
      .then(applyPlaybackState)
      .catch((error) => setPlaybackStatus(`变速失败：${String(error)}`));
  }

  function seekToRatio(ratio: number) {
    if (!activeDurationMs) return;
    // Round at the source so playbackCurrentMs is always an integer —
    // anything that reads it later (togglePlayback, the IPC layer) can
    // pass it straight through without re-rounding.
    const positionMs = Math.round(
      clamp(ratio * activeDurationMs, 0, activeDurationMs),
    );
    setPlaybackCurrentMs(positionMs);
    if (playbackPath && isPlaying) {
      void ipc
        .playAudio({ path: playbackPath, positionMs })
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
          playbackSpeed={playbackSpeed}
          onSelectionChange={setTimelineSelection}
          onApplyInlineTag={applyInlineTag}
          onClose={closeAnnotation}
          onTogglePlay={togglePlayback}
          onSeekRatio={seekToRatio}
          onPlaybackSpeedChange={changePlaybackSpeed}
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
          onMigrateSegmentFilenames={migrateSegmentFilenames}
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
                segments.filter((s) => {
                  const asrDone = s.phoneticText.trim().length > 0;
                  const polishDone = s.emotion.length > 0;
                  if (!asrDone) return true;
                  if (settings.useLlm && !polishDone) return true;
                  return false;
                }).length
              }
              onCutAll={cutAll}
              onRecognizeVisible={recognizeVisibleDraft}
              onRecognizeAllPending={recognizeAllPending}
              onReRecognizeVisible={reRecognizeVisible}
              onRepolishVisible={repolishVisibleWithLlm}
            />
          )}
          {progress.visible && (
            <ProgressPanel
              progress={progress}
              onCancel={
                cancelHandlerRef.current
                  ? () => cancelHandlerRef.current?.()
                  : undefined
              }
            />
          )}
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
          onMigrateSegmentFilenames={migrateSegmentFilenames}
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
