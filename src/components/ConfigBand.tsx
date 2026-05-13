import { useEffect, useMemo, useState } from "react";
import {
  Activity,
  Bot,
  ChevronDown,
  ChevronRight,
  Eraser,
  HardDrive,
  ShieldCheck,
  ListChecks,
  Mic,
  Plus,
  RefreshCcw,
  Ruler,
  Save,
  Scissors,
  Sliders,
  Sparkles,
  Tags as TagsIcon,
  Trash2,
  Wand2,
} from "lucide-react";
import type {
  AppSettings,
  CutConfig,
  CutMode,
  CutPresetDef,
  DependencyStatus,
  InlineTagDef,
  OllamaEndpointDef,
  SegmentTagDef,
  WhisperEndpointDef,
} from "../types";
import {
  defaultEmotions,
  defaultInlineTags,
  defaultSegmentTags,
  whisperModels,
} from "../defaults";
import { ipc } from "../lib";

function prefixPreview(prefix: string, relPath: string): string {
  const trimmed = (prefix ?? "").replace(/\/+$/, "");
  if (!trimmed) return `(留空)/${relPath}`;
  return `${trimmed}/${relPath}`;
}

/** Inline tag row — moved here verbatim from SettingsDrawer. */
function InlineTagRow({
  tag,
  onChange,
  onRemove,
}: {
  tag: InlineTagDef;
  onChange: (next: InlineTagDef) => void;
  onRemove: () => void;
}) {
  return (
    <>
      <input
        value={tag.label}
        onChange={(event) => onChange({ ...tag, label: event.target.value })}
        title="中文显示名"
      />
      <input
        value={tag.tag}
        onChange={(event) =>
          onChange({ ...tag, tag: event.target.value.toLowerCase() })
        }
        placeholder="laugh"
        title="英文标签名（写入 JSONL）"
      />
      <select
        value={tag.kind}
        onChange={(event) =>
          onChange({ ...tag, kind: event.target.value as InlineTagDef["kind"] })
        }
        title="paired = <tag>...</tag> 包文本；bracket = [tag] 离散事件"
      >
        <option value="paired">成对包裹</option>
        <option value="bracket">离散事件</option>
      </select>
      <input
        value={tag.key}
        maxLength={1}
        onChange={(event) =>
          onChange({
            ...tag,
            key: event.target.value.toUpperCase().slice(0, 1),
          })
        }
        placeholder="—"
        title="单字母快捷键（可空）"
        style={{ textAlign: "center" }}
      />
      <input
        value={tag.glyph ?? ""}
        maxLength={1}
        onChange={(event) =>
          onChange({ ...tag, glyph: event.target.value.slice(0, 1) })
        }
        placeholder="—"
        title="时间轴上离散事件显示的字符（仅 bracket 用）"
        style={{ textAlign: "center" }}
        disabled={tag.kind !== "bracket"}
      />
      <button
        className="btn-ghost btn-icon"
        onClick={onRemove}
        title="删除此标签"
      >
        <Trash2 size={12} />
      </button>
    </>
  );
}

/** Segment-level tag row — moved here verbatim from SettingsDrawer. */
function SegmentTagRow({
  tag,
  onChange,
  onRemove,
}: {
  tag: SegmentTagDef;
  onChange: (next: SegmentTagDef) => void;
  onRemove: () => void;
}) {
  return (
    <>
      <input
        value={tag.label}
        onChange={(event) => onChange({ ...tag, label: event.target.value })}
        title="中文显示名"
      />
      <input
        value={tag.value}
        onChange={(event) =>
          onChange({ ...tag, value: event.target.value.toLowerCase() })
        }
        placeholder="laugh"
        title="英文标签名（写入 JSONL）"
      />
      <input
        value={tag.key}
        maxLength={1}
        onChange={(event) =>
          onChange({
            ...tag,
            key: event.target.value.toUpperCase().slice(0, 1),
          })
        }
        placeholder="—"
        title="单字母快捷键（可空）"
        style={{ textAlign: "center" }}
      />
      <button
        className="btn-ghost btn-icon"
        onClick={onRemove}
        title="删除此标签"
      >
        <Trash2 size={12} />
      </button>
    </>
  );
}

// ---------------------------------------------------------------------------
// Inline editors for endpoint pools. Used by BOTH mode panels so the user
// configures Whisper / Ollama lists alongside the cut strategy that owns
// them, instead of hunting through Settings. Each row: checkbox (enabled
// switch) → url input → [optional model input for Ollama] → trash icon.
// ---------------------------------------------------------------------------

function WhisperEndpointEditor(props: {
  endpoints: WhisperEndpointDef[];
  onChange: (next: WhisperEndpointDef[]) => void;
  label: string;
  /** Short hint shown under the list. */
  hint?: string;
}) {
  const list = props.endpoints ?? [];
  return (
    <div className="endpoint-editor" style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <span className="config-param-label" style={{ marginBottom: 2 }}>{props.label}</span>
      {list.map((ep, idx) => {
        const enabled = ep.enabled !== false;
        return (
          <div
            key={idx}
            style={{
              display: "grid",
              gridTemplateColumns: "auto 1fr auto",
              gap: 6,
              alignItems: "center",
              opacity: enabled ? 1 : 0.55,
            }}
          >
            <input
              type="checkbox"
              checked={enabled}
              onChange={(e) =>
                props.onChange(
                  list.map((x, i) =>
                    i === idx ? { ...x, enabled: e.target.checked } : x,
                  ),
                )
              }
              title={enabled ? "启用中（点击禁用）" : "已禁用（点击启用）"}
            />
            <input
              value={ep.url}
              placeholder="http://100.64.0.x:9090"
              onChange={(e) =>
                props.onChange(
                  list.map((x, i) =>
                    i === idx ? { ...x, url: e.target.value } : x,
                  ),
                )
              }
            />
            <button
              className="btn-ghost btn-icon"
              onClick={() => props.onChange(list.filter((_, i) => i !== idx))}
              title="删除此端点"
            >
              <Trash2 size={14} />
            </button>
          </div>
        );
      })}
      <button
        className="btn-ghost"
        style={{ alignSelf: "flex-start" }}
        onClick={() => props.onChange([...list, { url: "" }])}
      >
        <Plus size={12} /> 添加 Whisper 端点
      </button>
      {props.hint && (
        <p className="help-tip" style={{ marginTop: 2, opacity: 0.7 }}>
          {props.hint}
        </p>
      )}
    </div>
  );
}

function OllamaEndpointEditor(props: {
  endpoints: OllamaEndpointDef[];
  onChange: (next: OllamaEndpointDef[]) => void;
  label: string;
  modelPlaceholder: string;
  hint?: string;
}) {
  const list = props.endpoints ?? [];
  return (
    <div className="endpoint-editor" style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <span className="config-param-label" style={{ marginBottom: 2 }}>{props.label}</span>
      {list.map((ep, idx) => {
        const enabled = ep.enabled !== false;
        return (
          <div
            key={idx}
            style={{
              display: "grid",
              gridTemplateColumns: "auto 1.6fr 1fr auto",
              gap: 6,
              alignItems: "center",
              opacity: enabled ? 1 : 0.55,
            }}
          >
            <input
              type="checkbox"
              checked={enabled}
              onChange={(e) =>
                props.onChange(
                  list.map((x, i) =>
                    i === idx ? { ...x, enabled: e.target.checked } : x,
                  ),
                )
              }
            />
            <input
              value={ep.url}
              placeholder="http://100.64.0.x:11434"
              onChange={(e) =>
                props.onChange(
                  list.map((x, i) =>
                    i === idx ? { ...x, url: e.target.value } : x,
                  ),
                )
              }
            />
            <input
              value={ep.model ?? ""}
              placeholder={props.modelPlaceholder}
              onChange={(e) =>
                props.onChange(
                  list.map((x, i) =>
                    i === idx ? { ...x, model: e.target.value } : x,
                  ),
                )
              }
              title="留空则使用首个端点的模型作为默认"
            />
            <button
              className="btn-ghost btn-icon"
              onClick={() => props.onChange(list.filter((_, i) => i !== idx))}
              title="删除此端点"
            >
              <Trash2 size={14} />
            </button>
          </div>
        );
      })}
      <button
        className="btn-ghost"
        style={{ alignSelf: "flex-start" }}
        onClick={() =>
          props.onChange([
            ...list,
            { url: "", model: props.modelPlaceholder, enabled: true },
          ])
        }
      >
        <Plus size={12} /> 添加 LLM 端点
      </button>
      {props.hint && (
        <p className="help-tip" style={{ marginTop: 2, opacity: 0.7 }}>
          {props.hint}
        </p>
      )}
    </div>
  );
}

type ConfigBandProps = {
  config: CutConfig;
  onConfigChange: (next: CutConfig) => void;
  presets: CutPresetDef[];
  onPresetsChange: (next: CutPresetDef[]) => void;
  autoRecognizeAfterCut: boolean;
  onAutoRecognizeChange: (value: boolean) => void;
  busy: boolean;
  hasAudio: boolean;
  hasSegments: boolean;
  llmEnabled: boolean;
  pendingCount: number;
  cutValidationSummary: { checked: number; failed: number } | null;
  /** Mode 1's Whisper endpoint pool. Lives in AppSettings.whisperEndpoints
   *  but surfaced here so it sits next to the cut strategy it powers. */
  whisperEndpoints: WhisperEndpointDef[];
  onWhisperEndpointsChange: (next: WhisperEndpointDef[]) => void;
  /** Mode 1's Ollama LLM pool. Lives in AppSettings.ollamaExtraEndpoints. */
  ollamaExtraEndpoints: OllamaEndpointDef[];
  onOllamaExtraEndpointsChange: (next: OllamaEndpointDef[]) => void;
  /** Primary Ollama model — used as placeholder default for new endpoints. */
  ollamaModelDefault: string;
  /** Full settings + patch callback. Mode 1's pipeline-related settings
   *  (Whisper model, prompts, LLM polish) used to live in the global
   *  SettingsDrawer. Per design feedback we moved them down into the
   *  Mode 1 panel so the drawer keeps only project-wide knobs and the
   *  Mode panel owns everything its mode actually needs. We pass the
   *  whole settings object + a generic patcher rather than wiring up
   *  ~15 individual setters. */
  settings: AppSettings;
  onSettingsChange: (patch: Partial<AppSettings>) => void;
  /** "Restore default Ollama prompt" — same callback the drawer used,
   *  threaded down so the dialect-prompt textarea can reset itself. */
  onResetLlmPrompt: () => void;
  /** Optional housekeeping actions, previously buried in the drawer's
   *  tag dictionary section. Both are project-state mutators so they
   *  belong wherever the relevant config lives — i.e. the tag editor. */
  onPurgeOrphanTags?: () => void;
  onMigrateSegmentFilenames?: () => void;
  onCutAll: () => void;
  onCleanCutNoise: () => void;
  onRepairCutSilence: () => void;
  onValidateCuts: () => void;
  onRecognizeVisible: () => void;
  onRecognizeAllPending: () => void;
  onReRecognizeVisible: () => void;
  onRepolishVisible: () => void;
};

/** Numeric-valued keys of CutConfig — used to narrow the ParamField map
 *  so we can safely pass `props.config[field.key]` straight to a number
 *  input without TS widening to include the endpoint-pool array fields. */
type NumericCutConfigKey =
  | "silenceDb"
  | "minSilenceMs"
  | "minSegmentMs"
  | "preRollMs"
  | "postRollMs"
  | "maxSegmentMs";

type ParamField = {
  key: NumericCutConfigKey;
  label: string;
  unit: string;
  hint: string;
  min?: number;
  max?: number;
  step?: number;
};

const fields: ParamField[] = [
  {
    key: "silenceDb",
    label: "静音阈值",
    unit: "dB",
    hint: "低于这个分贝视为静音。环境噪音大就调高（如 -28），录音很干净就调低（如 -45）",
    min: -60,
    max: -10,
    step: 1,
  },
  {
    key: "minSilenceMs",
    label: "最短停顿",
    unit: "ms",
    hint: "连续静音超过这个时长才算分段点。说话节奏快调小（200~400），慢调大（500~800）",
    min: 100,
    step: 50,
  },
  {
    key: "minSegmentMs",
    label: "最短语音",
    unit: "ms",
    hint: "切出来的段如果短于这个，会被丢弃。短促互动调小（150），完整句子调大（800+）",
    min: 100,
    step: 50,
  },
  {
    key: "preRollMs",
    label: "前留空",
    unit: "ms",
    hint: "段落开头多保留的静音长度。规范要求句首静音 ≤ 100ms",
    min: 0,
    max: 500,
    step: 20,
  },
  {
    key: "postRollMs",
    label: "后留空",
    unit: "ms",
    hint: "段落末尾多保留的静音长度。规范要求句尾静音 ≤ 200ms",
    min: 0,
    max: 800,
    step: 20,
  },
  {
    key: "maxSegmentMs",
    label: "最长段长",
    unit: "ms",
    hint:
      "硬上限：单段超过这个时长，cutter 把它等分成 N 块。0 = 不限。Whisper 在 ~30s 以上质量明显下降，建议 30000。",
    min: 0,
    max: 120000,
    step: 1000,
  },
];

/** Two CutConfig objects equal? Used to detect "current = preset". */
function configEqual(a: CutConfig, b: CutConfig) {
  return (
    a.silenceDb === b.silenceDb &&
    a.minSilenceMs === b.minSilenceMs &&
    a.minSegmentMs === b.minSegmentMs &&
    a.preRollMs === b.preRollMs &&
    a.postRollMs === b.postRollMs
  );
}

export function ConfigBand(props: ConfigBandProps) {
  const matchedPreset = useMemo(
    () =>
      props.presets.find((p) => configEqual(p.config, props.config)) ?? null,
    [props.presets, props.config],
  );

  const currentMode: CutMode = props.config.mode ?? "dialect";

  const updateSemanticField = <K extends keyof CutConfig>(key: K, value: CutConfig[K]) => {
    props.onConfigChange({ ...props.config, [key]: value });
  };

  // Live list of Ollama models for the primary endpoint dropdown.
  // Moved out of SettingsDrawer along with the section that uses it.
  // Refresh fires on mount + whenever the user blurs the URL field.
  const [ollamaModels, setOllamaModels] = useState<string[]>([]);
  const [modelLoading, setModelLoading] = useState(false);
  const refreshOllamaModels = async () => {
    if (!props.settings.ollamaUrl) return;
    setModelLoading(true);
    try {
      const list = await ipc.listOllamaModels({
        url: props.settings.ollamaUrl,
      });
      setOllamaModels(list);
    } catch {
      setOllamaModels([]);
    } finally {
      setModelLoading(false);
    }
  };
  // Auto-refresh once on mount when in dialect mode (cheap; one GET to a
  // local server). We don't refresh on every URL keystroke — only on blur,
  // matching the previous SettingsDrawer behaviour.
  useEffect(() => {
    if (currentMode === "dialect") {
      void refreshOllamaModels();
      void refreshDependencies();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentMode === "dialect"]);

  // Dependency health — moved from SettingsDrawer along with the section.
  const [deps, setDeps] = useState<DependencyStatus | null>(null);
  const [depsLoading, setDepsLoading] = useState(false);
  const refreshDependencies = async () => {
    setDepsLoading(true);
    try {
      const status = await ipc.checkDependencies({
        ollamaUrl: props.settings.ollamaUrl,
      });
      setDeps(status);
    } catch {
      setDeps(null);
    } finally {
      setDepsLoading(false);
    }
  };

  return (
    <section className="card">
      {/* Mode is picked in the Topbar (right next to the title).
          ConfigBand just reads `config.mode` to decide which strategy
          panel + which endpoint pool editors to render. The tab bar
          used to live here, but that put the mode choice AFTER the
          import band, which misled users into importing first and
          cutting with the wrong mode. */}
      <div className="config-row" style={{ flexWrap: "wrap", rowGap: 6 }}>
        {currentMode === "semantic" ? (
          // Default open: Mode 2 is opt-in, and once you're in Mode 2
          // the whole point of the panel is to configure it (window,
          // prompt, Whisper/LLM pools). A collapsed-by-default details
          // makes the config look missing — confused users couldn't
          // find the prompt textarea + endpoint editors.
          <details className="config-strategy" open>
            <summary
              className="config-strategy-trigger"
              title="模式 2 配置：语义参数 + Whisper / LLM 端点 + 切点提示词"
            >
              <span className="config-strategy-trigger-main">
                <Sliders size={14} />
                <span>语义配置</span>
                <span className="config-strategy-current">
                  滑窗 ·切点提示词 · Whisper / LLM 端点
                </span>
              </span>
              <span className="config-strategy-chevron">
                <ChevronDown size={14} className="config-strategy-icon-open" />
                <ChevronRight size={14} className="config-strategy-icon-closed" />
              </span>
            </summary>
            <div className="config-strategy-body">
              <div className="config-strategy-params" style={{ gap: 10 }}>
                <label className="config-param">
                  <span className="config-param-label">最短段长 (秒)</span>
                  <input
                    type="number"
                    min={2}
                    max={30}
                    step={1}
                    value={props.config.minSegmentS ?? 6}
                    onChange={(e) =>
                      updateSemanticField(
                        "minSegmentS",
                        Math.max(2, Number(e.target.value) || 6),
                      )
                    }
                    title="规范 § 二·一·1：6 秒（弹性下限）"
                  />
                </label>
                <label className="config-param">
                  <span className="config-param-label">最长段长 (秒)</span>
                  <input
                    type="number"
                    min={30}
                    max={180}
                    step={5}
                    value={props.config.maxSegmentS ?? 90}
                    onChange={(e) =>
                      updateSemanticField(
                        "maxSegmentS",
                        Math.max(30, Number(e.target.value) || 90),
                      )
                    }
                    title="规范 § 二·一·1：90 秒上限"
                  />
                </label>
                <label className="config-param">
                  <span className="config-param-label">目标响度 (LUFS)</span>
                  <input
                    type="number"
                    min={-30}
                    max={-6}
                    step={1}
                    value={props.config.targetLoudnessLufs ?? -18}
                    onChange={(e) =>
                      updateSemanticField(
                        "targetLoudnessLufs",
                        Number(e.target.value) || -18,
                      )
                    }
                    title="规范 § 七·1：-18 LUFS (ITU-R BS.1770)"
                  />
                </label>
                <label className="config-param">
                  <span className="config-param-label">头尾静音 (ms)</span>
                  <input
                    type="number"
                    min={100}
                    max={500}
                    step={10}
                    value={props.config.headTailSilenceMs ?? 150}
                    onChange={(e) =>
                      updateSemanticField(
                        "headTailSilenceMs",
                        Math.max(100, Number(e.target.value) || 150),
                      )
                    }
                    title="规范 § 二·三·1：≥150ms 避免突然截断"
                  />
                </label>
              </div>
              <div
                className="config-strategy-params"
                style={{ gap: 10, marginTop: 8 }}
              >
                <label className="config-param">
                  <span className="config-param-label">滑窗大小 (秒)</span>
                  <input
                    type="number"
                    min={6}
                    max={300}
                    step={5}
                    value={
                      props.config.semanticWindowS ??
                      props.config.maxSegmentS ??
                      90
                    }
                    onChange={(e) =>
                      updateSemanticField(
                        "semanticWindowS",
                        Math.max(6, Number(e.target.value) || 90),
                      )
                    }
                    title="每次处理多少秒的窗口。等于最长段长时窗口=段；调大可以让 LLM 看到更多上下文再决定切点（最长段长仍受 max_segment_s 控制）"
                  />
                </label>
                <label className="config-param" style={{ flex: 1 }}>
                  <span className="config-param-label">num_ctx</span>
                  <input
                    type="number"
                    min={2048}
                    max={131072}
                    step={2048}
                    value={props.config.semanticNumCtx ?? 32768}
                    onChange={(e) =>
                      updateSemanticField(
                        "semanticNumCtx",
                        Math.max(2048, Number(e.target.value) || 32768),
                      )
                    }
                    title="LLM 上下文大小。滑窗模式下每次 prompt 都不大，2048-8192 通常足够"
                  />
                </label>
              </div>

              {/* Editable cut-decision prompt — per-language tuning lives here. */}
              <div style={{ marginTop: 10 }}>
                <span className="config-param-label" style={{ marginBottom: 2 }}>
                  Mode 2 · 切点提示词模板
                </span>
                <textarea
                  rows={8}
                  value={props.config.semanticCutPrompt ?? ""}
                  onChange={(e) =>
                    updateSemanticField("semanticCutPrompt", e.target.value)
                  }
                  placeholder="留空 → 使用内置普通话/长沙话默认。占位符: {transcript} {window_ms} {min_ms} {max_s} {sweet_lo} {sweet_hi}"
                  style={{ width: "100%", fontSize: 12, fontFamily: "monospace" }}
                />
                <p className="help-tip" style={{ marginTop: 4, opacity: 0.75 }}>
                  每次滑窗都用它问 LLM 「应该在多少 ms 处切第一刀」。换语言 / 方言时改这里。
                  支持占位符：<code>{`{transcript}`}</code>{" "}
                  <code>{`{window_ms}`}</code> <code>{`{min_ms}`}</code>{" "}
                  <code>{`{max_s}`}</code> <code>{`{sweet_lo}`}</code>{" "}
                  <code>{`{sweet_hi}`}</code>
                </p>
              </div>
              <div style={{ marginTop: 10 }}>
                <OllamaEndpointEditor
                  endpoints={props.config.semanticOllamaEndpoints ?? []}
                  onChange={(next) =>
                    updateSemanticField("semanticOllamaEndpoints", next)
                  }
                  label="Mode 2 · LLM 端点池（按顺序 failover）"
                  modelPlaceholder="qwen2.5:32b"
                  hint="Mode 2 专用，独立于 Mode 1 的 Ollama 池。每个端点必须支持 num_ctx ≥ 32K（建议 huayu 100.64.0.4:11434）。空 → 拒绝运行。"
                />
              </div>
              <div style={{ marginTop: 10 }}>
                <WhisperEndpointEditor
                  endpoints={props.config.semanticWhisperEndpoints ?? []}
                  onChange={(next) =>
                    updateSemanticField("semanticWhisperEndpoints", next)
                  }
                  label="Mode 2 · Whisper 端点池"
                  hint="Mode 2 专用，独立于 Mode 1 的 Whisper 池。空 → 退回 Mode 1 设置里的全局 Whisper 池或本机 CLI。"
                />
              </div>
              <p
                className="config-strategy-hint"
                style={{ marginTop: 8, opacity: 0.7 }}
              >
                算法：滑窗 → Whisper 转写 → LLM 单点决策 →
                继续。每个窗口的提示词由上面的「切点提示词模板」决定（留空 → 用内置普通话/长沙话默认）。产物是 Mode-1 兼容项目目录，下载后 Windows 客户端 → 「打开」 → 选 <code>project.json</code> 即可标注。
              </p>
            </div>
          </details>
        ) : null}

        {currentMode === "dialect" ? (
        <details className="config-strategy">
          <summary className="config-strategy-trigger" title="展开切割策略参数">
            <span className="config-strategy-trigger-main">
              <Sliders size={14} />
              <span>切割策略</span>
              <span className="config-strategy-current">
                {matchedPreset?.name ?? "自定义（未保存）"}
              </span>
            </span>
            <span className="config-strategy-chevron">
              <ChevronDown size={14} className="config-strategy-icon-open" />
              <ChevronRight size={14} className="config-strategy-icon-closed" />
            </span>
          </summary>
          <div className="config-strategy-body">
            <div className="config-strategy-toolbar">
              <select
                value={matchedPreset?.name ?? "__custom__"}
                onChange={(event) => {
                  const name = event.target.value;
                  const found = props.presets.find((p) => p.name === name);
                  if (found) props.onConfigChange({ ...found.config });
                }}
                title="选择内置或保存的策略；改任何参数会变成 自定义"
              >
                {props.presets.map((p) => (
                  <option key={p.name} value={p.name}>
                    {p.builtin ? "★ " : ""}{p.name}
                  </option>
                ))}
                {!matchedPreset && (
                  <option value="__custom__">⚙ 自定义（未保存）</option>
                )}
              </select>
              <button
                className="btn-ghost"
                onClick={(event) => {
                  event.stopPropagation();
                  event.preventDefault();
                  const name = window.prompt("保存为预设，输入名字：", "我的预设");
                  if (!name) return;
                  const cleaned = name.trim();
                  if (!cleaned) return;
                  const without = props.presets.filter(
                    (p) => p.name !== cleaned || p.builtin,
                  );
                  props.onPresetsChange([
                    ...without,
                    { name: cleaned, config: { ...props.config } },
                  ]);
                }}
                title="把当前参数另存为新预设"
              >
                <Save size={12} />
                保存
              </button>
              {matchedPreset && !matchedPreset.builtin && (
                <button
                  className="btn-ghost"
                  onClick={(event) => {
                    event.stopPropagation();
                    event.preventDefault();
                    if (!window.confirm(`删除预设「${matchedPreset.name}」？`)) return;
                    props.onPresetsChange(
                      props.presets.filter((p) => p.name !== matchedPreset.name),
                    );
                  }}
                  title="删除当前选中的自定义预设"
                >
                  <Trash2 size={12} />
                </button>
              )}
              {matchedPreset?.hint && (
                <span className="help-tip">
                  {matchedPreset.hint}
                </span>
              )}
            </div>
            <div className="config-fields">
              {fields.map((field) => (
                <label
                  key={field.key as string}
                  className="number-field"
                  title={field.hint}
                >
                  <span>
                    {field.label} <em style={{ opacity: 0.6 }}>{field.unit}</em>
                  </span>
                  <input
                    type="number"
                    min={field.min}
                    max={field.max}
                    step={field.step}
                    value={props.config[field.key]}
                    onChange={(event) =>
                      props.onConfigChange({
                        ...props.config,
                        [field.key]: Number(event.target.value),
                      })
                    }
                  />
                </label>
              ))}
            </div>
            {/* Endpoint pools owned by Mode 1. Moved here from
                SettingsDrawer per user request: keeping each mode's
                Whisper / LLM pool next to its strategy eliminates
                ambiguity about which pool actually drives the run.
                State still lives in AppSettings so it survives
                across projects. */}
            <div style={{ marginTop: 12 }}>
              <WhisperEndpointEditor
                endpoints={props.whisperEndpoints}
                onChange={props.onWhisperEndpointsChange}
                label="Mode 1 · Whisper 端点池（faster-whisper HTTP）"
                hint="启用后 ASR 把每段 wav POST 到端点池（参考 services/whisper-server/）。空 → 退回本机 whisper CLI。"
              />
            </div>
            <div style={{ marginTop: 10 }}>
              <OllamaEndpointEditor
                endpoints={props.ollamaExtraEndpoints}
                onChange={props.onOllamaExtraEndpointsChange}
                label="Mode 1 · LLM 端点池（按顺序 failover）"
                modelPlaceholder={props.ollamaModelDefault || "qwen2.5:32b"}
                hint="LLM 改写阶段会跨这些端点 work-stealing 并发。模型留空则使用主模型。"
              />
            </div>

            {/* ---------------------------------------------------------
                Below: the three Mode-1-scoped sections that used to live
                in the global SettingsDrawer. Each is a flat `<h4>` + a
                stack of drawer-row blocks (re-using existing CSS) so
                we don't nest <details> inside <details>.
            --------------------------------------------------------- */}

            {/* 方言预设 — switches whisper initial prompt + ollama prompt
                + system prompt in one click. Sits here because all three
                of those fields are Mode 1's responsibility. */}
            <h4>
              <Sparkles size={14} />
              方言预设
            </h4>
            <p>
              一个预设 = 一套 Whisper initial prompt + Ollama 改写 prompt + JSONL System
              prompt。换预设一次性填好三个字段；想改就改完后「另存为」。
            </p>
            <div className="drawer-row">
              <label>当前预设</label>
              <div style={{ display: "flex", gap: 6, flex: 1 }}>
                <select
                  value={props.settings.activeDialectProfileId ?? ""}
                  onChange={(event) => {
                    const id = event.target.value;
                    const profile = (props.settings.dialectProfiles ?? []).find(
                      (p) => p.id === id,
                    );
                    if (!profile) return;
                    props.onSettingsChange({
                      activeDialectProfileId: id,
                      whisperInitialPrompt: profile.whisperInitialPrompt,
                      llmPrompt: profile.llmPrompt,
                      systemPrompt: profile.systemPrompt,
                    });
                  }}
                  style={{ flex: 1 }}
                >
                  {(props.settings.dialectProfiles ?? []).map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                      {p.builtin ? "" : "（自定义）"}
                    </option>
                  ))}
                </select>
                <button
                  className="btn-ghost"
                  title="把当前三个 prompt 字段保存为一个新预设"
                  onClick={() => {
                    const name = window
                      .prompt("新预设名称：", "我的预设")
                      ?.trim();
                    if (!name) return;
                    const id = `custom-${Date.now()}`;
                    props.onSettingsChange({
                      dialectProfiles: [
                        ...(props.settings.dialectProfiles ?? []),
                        {
                          id,
                          name,
                          whisperInitialPrompt: props.settings.whisperInitialPrompt,
                          llmPrompt: props.settings.llmPrompt,
                          systemPrompt: props.settings.systemPrompt,
                        },
                      ],
                      activeDialectProfileId: id,
                    });
                  }}
                >
                  <Save size={12} /> 另存为…
                </button>
                {(() => {
                  const active = (props.settings.dialectProfiles ?? []).find(
                    (p) => p.id === props.settings.activeDialectProfileId,
                  );
                  if (!active || active.builtin) return null;
                  return (
                    <button
                      className="btn-ghost btn-icon"
                      title="删除此自定义预设"
                      onClick={() => {
                        if (!window.confirm(`删除预设「${active.name}」？`))
                          return;
                        const remaining = (
                          props.settings.dialectProfiles ?? []
                        ).filter((p) => p.id !== active.id);
                        props.onSettingsChange({
                          dialectProfiles: remaining,
                          activeDialectProfileId:
                            remaining.find((p) => p.builtin)?.id ?? remaining[0]?.id,
                        });
                      }}
                    >
                      <Trash2 size={12} />
                    </button>
                  );
                })()}
              </div>
            </div>

            {/* Whisper recognition settings. Endpoint pool already
                rendered above. */}
            <h4>
              <Mic size={14} />
              Whisper 识别
            </h4>
            <p>
              纯文本 LLM 不能听音，必须先用 Whisper 把声波转成普通话初稿。模型越大越准、越慢。
            </p>
            <div className="drawer-row">
              <label>模型</label>
              <select
                value={props.settings.whisperModel}
                onChange={(event) =>
                  props.onSettingsChange({ whisperModel: event.target.value })
                }
                style={{ flex: 1 }}
              >
                {whisperModels.map((m) => (
                  <option key={m.value} value={m.value}>
                    {m.label} — {m.note}
                  </option>
                ))}
              </select>
            </div>
            <div className="drawer-row stack">
              <label>Initial prompt（提示音色 / 语种）</label>
              <textarea
                value={props.settings.whisperInitialPrompt}
                onChange={(event) =>
                  props.onSettingsChange({
                    whisperInitialPrompt: event.target.value,
                  })
                }
                rows={3}
              />
            </div>
            <div className="drawer-row">
              <label>结果缓存</label>
              <label className="checkbox-field">
                <input
                  type="checkbox"
                  checked={props.settings.useAsrCache}
                  onChange={(event) =>
                    props.onSettingsChange({ useAsrCache: event.target.checked })
                  }
                />
                <span>命中相同音频时跳过重跑（按文件 hash 缓存）</span>
              </label>
            </div>
            <div className="drawer-row">
              <label>并发进程数</label>
              <input
                type="number"
                min={1}
                max={16}
                value={props.settings.whisperConcurrency ?? 1}
                onChange={(event) =>
                  props.onSettingsChange({
                    whisperConcurrency: Math.max(1, Number(event.target.value)),
                  })
                }
                title="单机模式 = 同时跑的 whisper CLI 进程数。分布式模式 = 同时发出的 HTTP 请求数，建议 = 端点数 × 2"
              />
            </div>

            {/* Ollama polish settings. Endpoint pool already rendered above. */}
            <h4>
              <Wand2 size={14} />
              Ollama 改写
            </h4>
            <p>
              对每段 Whisper 初稿调用 Ollama，按「记音字优先」改写为方言字，并初步标注情绪 / 笑声 / 呼吸。
            </p>
            <div className="drawer-row">
              <label>启用 LLM 改写</label>
              <label className="checkbox-field">
                <input
                  type="checkbox"
                  checked={props.settings.useLlm}
                  onChange={(event) =>
                    props.onSettingsChange({ useLlm: event.target.checked })
                  }
                />
                <span>识别后自动调用 Ollama</span>
              </label>
            </div>
            <div className="drawer-row">
              <label>主端点</label>
              <input
                value={props.settings.ollamaUrl}
                onChange={(event) =>
                  props.onSettingsChange({ ollamaUrl: event.target.value })
                }
                onBlur={refreshOllamaModels}
                placeholder="http://localhost:11434"
              />
            </div>
            <div className="drawer-row">
              <label>主模型</label>
              <div style={{ display: "flex", gap: 6, flex: 1 }}>
                <select
                  value={props.settings.ollamaModel}
                  onChange={(event) =>
                    props.onSettingsChange({ ollamaModel: event.target.value })
                  }
                  style={{ flex: 1 }}
                >
                  {ollamaModels.length === 0 && (
                    <option value={props.settings.ollamaModel}>
                      {props.settings.ollamaModel || "未发现可用模型"}
                    </option>
                  )}
                  {ollamaModels.map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
                </select>
                <button
                  className="btn-ghost"
                  onClick={refreshOllamaModels}
                  disabled={modelLoading}
                  title="刷新模型列表"
                >
                  <RefreshCcw size={12} />
                </button>
              </div>
            </div>
            <div className="drawer-row">
              <label>并发请求数</label>
              <input
                type="number"
                min={1}
                max={16}
                value={props.settings.llmConcurrency ?? 2}
                onChange={(event) =>
                  props.onSettingsChange({
                    llmConcurrency: Math.max(1, Number(event.target.value)),
                  })
                }
                title="一批内最多同时发出的 Ollama 请求数。建议 = 端点数 × 2"
              />
            </div>
            <div className="drawer-row stack">
              <label>方言改写 Prompt</label>
              <textarea
                value={props.settings.llmPrompt}
                onChange={(event) =>
                  props.onSettingsChange({ llmPrompt: event.target.value })
                }
                rows={8}
                placeholder="留空则使用内置的长沙话默认 Prompt"
              />
              <button
                className="btn-ghost"
                onClick={props.onResetLlmPrompt}
                style={{ alignSelf: "flex-start", marginTop: 4 }}
              >
                <RefreshCcw size={12} /> 恢复默认 Prompt
              </button>
            </div>

            {/* ---------- 项目级 (导出配置) ----------
                Per design feedback: Mode 1's JSONL export shape differs
                from Mode 2's xlsx, so these knobs (System prompt /
                user-assistant pairing / OSS prefix) are Mode 1 owned. */}
            <h4>
              <Bot size={14} />
              导出 · JSONL
            </h4>
            <div className="drawer-row stack">
              <label>System Prompt（出现在导出 JSONL 顶部）</label>
              <textarea
                value={props.settings.systemPrompt}
                onChange={(event) =>
                  props.onSettingsChange({ systemPrompt: event.target.value })
                }
                rows={3}
                placeholder="例：长沙本地人，女性，25岁左右，声音娇柔，声音清亮"
              />
            </div>
            <div className="drawer-row">
              <label>配对 user+assistant</label>
              <label className="checkbox-field">
                <input
                  type="checkbox"
                  checked={props.settings.pairUserAssistant}
                  onChange={(event) =>
                    props.onSettingsChange({
                      pairUserAssistant: event.target.checked,
                    })
                  }
                />
                <span>导出时按文件名前缀配对（demo 格式）</span>
              </label>
            </div>
            <div className="drawer-row stack">
              <label>OSS 路径前缀（设一次以后不用改）</label>
              <input
                value={props.settings.audioFilePrefix}
                onChange={(event) =>
                  props.onSettingsChange({
                    audioFilePrefix: event.target.value,
                  })
                }
                placeholder="例：oss://aigc-audio-ext/label/multimodal_audio/studio_recording_dataset_方言"
              />
              <p className="help-tip">
                导出 JSONL 时 <code>audio_file</code> 以这个前缀 + 相对路径拼接。例：
                <br />
                <code>
                  {prefixPreview(
                    props.settings.audioFilePrefix,
                    "梁才-方言/NO21/WAV/自由演绎_0001_02_陪聊.wav",
                  )}
                </code>
                <br />
                工具不会真的上传文件，<strong>你需要自己把本地切片传到这个 OSS 位置</strong>。
              </p>
            </div>

            {/* ---------- 标签字典 (含情感) ----------
                Mode 1 annotation uses these dictionaries; Mode 2's xlsx
                doesn't carry tags or emotion columns. */}
            <h4>
              <TagsIcon size={14} />
              标签字典
            </h4>
            <p>
              用户可动态维护。修改后立即在标注页生效；如需让 LLM 也按新标签输出，请相应更新方言改写 Prompt。
            </p>

            <div className="tag-editor">
              <div className="tag-editor-head">
                <strong>内联标签（10 种默认）</strong>
                <button
                  className="btn-ghost"
                  onClick={() =>
                    props.onSettingsChange({
                      inlineTags: defaultInlineTags.slice(),
                    })
                  }
                  title="还原为默认标签字典"
                >
                  <RefreshCcw size={12} /> 恢复默认
                </button>
              </div>
              <div className="tag-editor-grid tag-editor-grid-inline">
                <span>中文</span>
                <span>英文</span>
                <span>类型</span>
                <span>键</span>
                <span>字</span>
                <span></span>
                {props.settings.inlineTags.map((item, idx) => (
                  <InlineTagRow
                    key={idx}
                    tag={item}
                    onChange={(next) =>
                      props.onSettingsChange({
                        inlineTags: props.settings.inlineTags.map((t, i) =>
                          i === idx ? next : t,
                        ),
                      })
                    }
                    onRemove={() =>
                      props.onSettingsChange({
                        inlineTags: props.settings.inlineTags.filter(
                          (_, i) => i !== idx,
                        ),
                      })
                    }
                  />
                ))}
              </div>
              <button
                className="btn-ghost"
                style={{ marginTop: 8, alignSelf: "flex-start" }}
                onClick={() =>
                  props.onSettingsChange({
                    inlineTags: [
                      ...props.settings.inlineTags,
                      {
                        tag: "newtag",
                        label: "新标签",
                        key: "",
                        kind: "bracket",
                        glyph: "新",
                        hint: "",
                      },
                    ],
                  })
                }
              >
                <Plus size={12} /> 新增内联标签
              </button>
            </div>

            <div className="tag-editor">
              <div className="tag-editor-head">
                <strong>段级标签（写入 JSONL `tags`）</strong>
                <button
                  className="btn-ghost"
                  onClick={() =>
                    props.onSettingsChange({
                      segmentTags: defaultSegmentTags.slice(),
                    })
                  }
                >
                  <RefreshCcw size={12} /> 恢复默认
                </button>
              </div>
              <div className="tag-editor-grid tag-editor-grid-segment">
                <span>中文</span>
                <span>英文</span>
                <span>键</span>
                <span></span>
                {props.settings.segmentTags.map((item, idx) => (
                  <SegmentTagRow
                    key={idx}
                    tag={item}
                    onChange={(next) =>
                      props.onSettingsChange({
                        segmentTags: props.settings.segmentTags.map((t, i) =>
                          i === idx ? next : t,
                        ),
                      })
                    }
                    onRemove={() =>
                      props.onSettingsChange({
                        segmentTags: props.settings.segmentTags.filter(
                          (_, i) => i !== idx,
                        ),
                      })
                    }
                  />
                ))}
              </div>
              <button
                className="btn-ghost"
                style={{ marginTop: 8, alignSelf: "flex-start" }}
                onClick={() =>
                  props.onSettingsChange({
                    segmentTags: [
                      ...props.settings.segmentTags,
                      { value: "newtag", label: "新标签", key: "" },
                    ],
                  })
                }
              >
                <Plus size={12} /> 新增段级标签
              </button>
            </div>

            <div className="tag-editor">
              <div className="tag-editor-head">
                <strong>情感字典</strong>
                <button
                  className="btn-ghost"
                  onClick={() =>
                    props.onSettingsChange({
                      emotions: defaultEmotions.slice(),
                    })
                  }
                >
                  <RefreshCcw size={12} /> 恢复默认
                </button>
              </div>
              <div className="emotion-chip-editor">
                {props.settings.emotions.map((emo, idx) => (
                  <span key={idx} className="tag-chip emotion-chip-edit">
                    <input
                      value={emo}
                      onChange={(event) =>
                        props.onSettingsChange({
                          emotions: props.settings.emotions.map((e, i) =>
                            i === idx ? event.target.value : e,
                          ),
                        })
                      }
                    />
                    <button
                      className="btn-ghost btn-icon"
                      onClick={() =>
                        props.onSettingsChange({
                          emotions: props.settings.emotions.filter(
                            (_, i) => i !== idx,
                          ),
                        })
                      }
                      title="删除"
                    >
                      <Trash2 size={12} />
                    </button>
                  </span>
                ))}
                <button
                  className="btn-ghost"
                  onClick={() =>
                    props.onSettingsChange({
                      emotions: [...props.settings.emotions, "新情感"],
                    })
                  }
                >
                  <Plus size={12} /> 新增
                </button>
              </div>
              <p className="help-tip">
                标注页 1-{props.settings.emotions.length} 数字键依次对应这些情感（最多 9 个）。
              </p>
            </div>

            {props.onPurgeOrphanTags && (
              <div className="tag-editor">
                <div className="tag-editor-head">
                  <strong>数据清理</strong>
                </div>
                <p className="section-hint" style={{ marginTop: 0 }}>
                  扫所有片段的 phoneticText，删除不在字典里的 inline 标签
                  （如旧版残留的 <code>&lt;pause/&gt;</code>），保留被包裹的字。
                </p>
                <button
                  className="btn-ghost"
                  onClick={props.onPurgeOrphanTags}
                  style={{ alignSelf: "flex-start" }}
                >
                  <Trash2 size={14} />
                  清理废弃 inline 标签
                </button>
                {props.onMigrateSegmentFilenames && (
                  <>
                    <p
                      className="section-hint"
                      style={{ marginTop: 12 }}
                    >
                      把已切好的片段重命名为 demo 格式{" "}
                      <code>&lt;base&gt;_&lt;NN&gt;_&lt;role&gt;.wav</code>。
                      会同时更新磁盘文件名和 <code>project.json</code>，已识别 / 已改写状态保留。
                    </p>
                    <button
                      className="btn-ghost"
                      onClick={props.onMigrateSegmentFilenames}
                      style={{ alignSelf: "flex-start" }}
                    >
                      <Trash2 size={14} />
                      迁移到 demo 命名
                    </button>
                  </>
                )}
              </div>
            )}

            {/* ---------- 依赖体检 ---------- */}
            <h4>
              <Activity size={14} />
              依赖体检
            </h4>
            <div className="dependency-row">
              <span className="name">FFmpeg</span>
              <span className={`status ${deps?.ffmpegOk ? "ok" : "error"}`}>
                {depsLoading
                  ? "检测中"
                  : deps?.ffmpegOk
                    ? "可用"
                    : "缺失"}
              </span>
              <span className="detail">
                {deps?.ffmpegError || "用于切割与波形解析"}
              </span>
            </div>
            <div className="dependency-row">
              <span className="name">Whisper</span>
              <span className={`status ${deps?.whisperOk ? "ok" : "error"}`}>
                {depsLoading
                  ? "检测中"
                  : deps?.whisperOk
                    ? "可用"
                    : "缺失"}
              </span>
              <span className="detail">
                {deps?.whisperPath ||
                  deps?.whisperError ||
                  "pip install openai-whisper"}
              </span>
            </div>
            <div className="dependency-row">
              <span className="name">Ollama</span>
              <span className={`status ${deps?.ollamaOk ? "ok" : "error"}`}>
                {depsLoading
                  ? "检测中"
                  : deps?.ollamaOk
                    ? "已连接"
                    : "未连接"}
              </span>
              <span className="detail">
                {deps?.ollamaError ||
                  `${deps?.ollamaModels?.length ?? 0} 个模型可用`}
              </span>
            </div>
            <button
              className="btn-ghost"
              onClick={refreshDependencies}
              disabled={depsLoading}
              style={{ alignSelf: "flex-start", marginTop: 8 }}
            >
              <HardDrive size={14} />
              重新检测
            </button>
          </div>
        </details>
        ) : null}

        <span style={{ flex: 1 }} />

        <label className="checkbox-field">
          <input
            type="checkbox"
            checked={props.autoRecognizeAfterCut}
            onChange={(event) =>
              props.onAutoRecognizeChange(event.target.checked)
            }
          />
          <span>切割后自动识别</span>
        </label>
        <button
          onClick={props.onCutAll}
          disabled={props.busy || !props.hasAudio}
        >
          <Scissors size={14} />
          全部切割
        </button>
        <button
          onClick={props.onCleanCutNoise}
          disabled={props.busy || !props.hasSegments}
          title="按当前静音阈值，把所有已切割 WAV 开头/结尾低于阈值的底噪写成 0；不重新切割、不改变片段长度"
        >
          <Eraser size={14} />
          清除底噪
        </button>
        <button
          onClick={props.onRepairCutSilence}
          disabled={props.busy || !props.hasSegments}
          title="按当前前/后留空和静音阈值，重新裁掉已切片多余静音并清除边界底噪；会更新片段时长"
        >
          <Ruler size={14} />
          修复留空
        </button>
        <button
          onClick={props.onValidateCuts}
          disabled={props.busy || !props.hasSegments}
          title="按当前切割策略检测所有已切割片段的前/后留空和最短语音，异常片段会标红"
        >
          <ShieldCheck size={14} />
          检测切片
          {props.cutValidationSummary && (
            <span
              className={`button-pill ${
                props.cutValidationSummary.failed > 0 ? "danger" : "success"
              }`}
            >
              {props.cutValidationSummary.failed > 0
                ? `${props.cutValidationSummary.failed} 异常`
                : "通过"}
            </span>
          )}
        </button>
        <button
          onClick={props.onRecognizeVisible}
          disabled={props.busy || !props.hasSegments}
          title={
            props.llmEnabled
              ? "仅当前选中音频范围内：Whisper 识别 → Ollama 改写"
              : "仅运行 Whisper（在设置中启用 Ollama 后处理）"
          }
        >
          <Sparkles size={14} />
          {props.llmEnabled ? "本音频识别" : "Whisper 识别"}
        </button>
        <button
          className="btn-primary"
          onClick={props.onRecognizeAllPending}
          disabled={props.busy || !props.hasSegments || props.pendingCount === 0}
          title={
            props.pendingCount > 0
              ? `跨所有音频，一次性识别全部 ${props.pendingCount} 段未处理片段`
              : "项目所有片段已识别"
          }
        >
          <ListChecks size={14} />
          全部待识别
          {props.pendingCount > 0 && (
            <span
              style={{
                marginLeft: 6,
                padding: "0 6px",
                borderRadius: 999,
                background: "rgba(255,255,255,0.22)",
                fontSize: 11,
                fontVariantNumeric: "tabular-nums",
              }}
            >
              {props.pendingCount}
            </span>
          )}
        </button>
        <button
          onClick={props.onReRecognizeVisible}
          disabled={props.busy || !props.hasSegments}
          title="对当前选中音频范围内全部片段强制重跑 Whisper + Ollama，跳过缓存、覆盖现有文本"
        >
          <RefreshCcw size={14} />
          重新识别
        </button>
        <button
          onClick={props.onRepolishVisible}
          disabled={props.busy || !props.hasSegments || !props.llmEnabled}
          title="只对现有文本重新调用 Ollama，自动加 笑声/重音/听不清/呼吸/停顿 标签 + 情绪。比重新识别快得多。"
        >
          <Wand2 size={14} />
          AI 重打标签
        </button>
      </div>
    </section>
  );
}
