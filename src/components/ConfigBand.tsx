import { useMemo } from "react";
import {
  ChevronDown,
  ChevronRight,
  Eraser,
  ShieldCheck,
  Languages,
  ListChecks,
  Plus,
  RefreshCcw,
  Ruler,
  Save,
  Scissors,
  Sliders,
  Sparkles,
  Trash2,
  Wand2,
} from "lucide-react";
import type {
  CutConfig,
  CutMode,
  CutPresetDef,
  OllamaEndpointDef,
  WhisperEndpointDef,
} from "../types";

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

  const setMode = (next: CutMode) => {
    props.onConfigChange({ ...props.config, mode: next });
  };
  const updateSemanticField = <K extends keyof CutConfig>(key: K, value: CutConfig[K]) => {
    props.onConfigChange({ ...props.config, [key]: value });
  };

  return (
    <section className="card">
      {/* Mode tabs. Tab bar at the top owns the full width of the card.
          Only the matching mode's panel renders below — flipping the
          tab swaps the visible panel instead of stacking two variants
          of the same form. `dialect` = historical silence-only cutter
          (长沙 / 台湾); `semantic` = Mandarin pipeline driven by
          录制数据剪辑转写规则（新）. Switching tabs does NOT migrate
          per-mode params; each mode owns its own knobs. */}
      <div
        className="config-mode-tabs"
        role="tablist"
        aria-label="切割模式"
        style={{
          display: "flex",
          gap: 0,
          borderBottom: "1px solid var(--border, #e2e8f0)",
          marginBottom: 12,
        }}
      >
        <button
          role="tab"
          aria-selected={currentMode === "dialect"}
          onClick={() => setMode("dialect")}
          className="config-mode-tab"
          style={{
            background: "transparent",
            border: "none",
            padding: "10px 18px",
            display: "inline-flex",
            alignItems: "center",
            gap: 6,
            cursor: "pointer",
            color:
              currentMode === "dialect"
                ? "var(--text-primary, #0f172a)"
                : "var(--text-secondary, #64748b)",
            fontWeight: currentMode === "dialect" ? 600 : 400,
            borderBottom:
              currentMode === "dialect"
                ? "2px solid var(--accent, #0d9488)"
                : "2px solid transparent",
            marginBottom: -1,
          }}
          title="按静音切；方言录音、保留全部声学事件，速度快、可重复"
        >
          <Scissors size={14} />
          模式 1 · 方言（按静音切）
        </button>
        <button
          role="tab"
          aria-selected={currentMode === "semantic"}
          onClick={() => setMode("semantic")}
          className="config-mode-tab"
          style={{
            background: "transparent",
            border: "none",
            padding: "10px 18px",
            display: "inline-flex",
            alignItems: "center",
            gap: 6,
            cursor: "pointer",
            color:
              currentMode === "semantic"
                ? "var(--text-primary, #0f172a)"
                : "var(--text-secondary, #64748b)",
            fontWeight: currentMode === "semantic" ? 600 : 400,
            borderBottom:
              currentMode === "semantic"
                ? "2px solid var(--accent, #0d9488)"
                : "2px solid transparent",
            marginBottom: -1,
          }}
          title="按语义切；普通话、ASR + Qwen 32K context 决定切点、规范化、出 xlsx"
        >
          <Languages size={14} />
          模式 2 · 普通话（语义切割）
        </button>
      </div>

      <div className="config-row" style={{ flexWrap: "wrap", rowGap: 6 }}>
        {currentMode === "semantic" ? (
          <details className="config-strategy">
            <summary
              className="config-strategy-trigger"
              title="展开模式 2 语义切割参数"
            >
              <span className="config-strategy-trigger-main">
                <Sliders size={14} />
                <span>语义参数</span>
                <span className="config-strategy-current">
                  {props.config.semanticEndpoint || "未配置 LLM 端点"}
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
                    title="≥32K 才能容纳 1 小时 ASR；默认 32768"
                  />
                </label>
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
                Mode 2 直接走「切」按钮——前端检测到 <code>mode = semantic</code>{" "}
                时改走 <code>run_semantic_pipeline</code> 命令；出 xlsx + 切片 WAV。
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
                hint="LLM 改写阶段会跨这些端点 work-stealing 并发。模型留空则使用 Settings 里配置的主模型。"
              />
            </div>
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
