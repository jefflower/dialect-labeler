import { useCallback, useEffect, useMemo, useState } from "react";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { Folder, Inbox } from "lucide-react";
import "./App.css";
import { Topbar } from "./components/Topbar";
import { SetupBand } from "./components/SetupBand";
import { ConfigBand } from "./components/ConfigBand";
import { MainView } from "./components/MainView";
import { AnnotationView } from "./components/AnnotationView";
import { ProgressPanel } from "./components/ProgressPanel";
import { SettingsDrawer } from "./components/SettingsDrawer";
import { ShortcutOverlay } from "./components/ShortcutOverlay";
import { ToastStack } from "./components/ToastStack";
import { defaultCutConfig, tagOptions } from "./defaults";
import { ipc, normalizeRecognizedText, waitForPaint, clamp } from "./lib";
import { isTyping, useSettings, useShortcutOverlay, useTheme, useToasts } from "./hooks";
import type {
  AudioFileInfo,
  CutConfig,
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
  const [autoCutAfterScan, setAutoCutAfterScan] = useState(true);
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

  // Keyboard shortcuts (J/K for nav, L/B/P/U/N for tags, Space for play/pause)
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (isTyping(event.target)) {
        if (event.code === "Space" && annotationSegment) {
          // allow space in textarea — don't intercept
        }
        return;
      }

      if (event.code === "Space") {
        event.preventDefault();
        togglePlayback();
        return;
      }

      const key = event.key.toUpperCase();
      const tag = tagOptions.find((item) => item.key === key);
      if (tag) {
        event.preventDefault();
        toggleTagOnActive(tag.value);
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
        showSuccess(
          "扫描并自动切割完成",
          `${result.audioFiles.length} 个音频，${allSegments.length} 段${
            autoRecognizeAfterCut ? `，识别 ${recognizedCount} 段` : ""
          }`,
        );
      } else {
        showSuccess(
          "扫描完成",
          `${result.audioFiles.length} 个音频`,
        );
      }
    } catch (error) {
      showError("扫描失败", error);
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
      setStatusMsg(
        `自动切割 ${index + 1}/${targetScan.audioFiles.length}：${audio.fileName}`,
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
      const created = await ipc.cutAudioFile({
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
        detail: `${audio.fileName} → ${created.length} 段`,
        current: index + 1,
        total: targetScan.audioFiles.length,
      });
      await waitForPaint();
    }
    return allSegments;
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
      showSuccess(
        "切割完成",
        `${allSegments.length} 段${
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
    options: { manageBusy: boolean; overwrite: boolean; label: string },
  ) {
    if (!scan || targetSegments.length === 0) {
      pushToast({ variant: "warning", title: "没有可识别的切割片段" });
      return 0;
    }
    const { manageBusy, overwrite, label } = options;
    if (manageBusy) setBusy(true);
    const batchSize = Math.max(1, settings.batchSize);
    let completed = 0;
    let recognizedCount = 0;
    setProgress({
      visible: true,
      label,
      detail: `准备识别 ${targetSegments.length} 段（${settings.whisperModel}）`,
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
        setSegments((current) =>
          current.map((segment) => {
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
      showError("识别失败", error);
      return 0;
    } finally {
      setProgress((current) => ({ ...current, visible: false }));
      if (manageBusy) setBusy(false);
    }
  }

  async function recognizeVisibleDraft() {
    const target = visibleSegments.length > 0 ? visibleSegments : segments;
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

  function updateSegment(id: string, patch: Partial<SegmentRecord>) {
    setSegments((current) =>
      current.map((segment) =>
        segment.id === id ? { ...segment, ...patch } : segment,
      ),
    );
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
          <ConfigBand
            config={config}
            onConfigChange={setConfig}
            autoRecognizeAfterCut={autoRecognizeAfterCut}
            onAutoRecognizeChange={setAutoRecognizeAfterCut}
            busy={busy}
            hasAudio={Boolean(scan && scan.audioFiles.length > 0)}
            hasSegments={segments.length > 0}
            llmEnabled={settings.useLlm}
            onCutAll={cutAll}
            onRecognizeVisible={recognizeVisibleDraft}
          />
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
