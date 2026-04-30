import { useMemo } from "react";
import {
  ChevronDown,
  ChevronRight,
  ListChecks,
  RefreshCcw,
  Save,
  Scissors,
  Sliders,
  Sparkles,
  Trash2,
  Wand2,
} from "lucide-react";
import type { CutConfig, CutPresetDef } from "../types";

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
  onCutAll: () => void;
  onRecognizeVisible: () => void;
  onRecognizeAllPending: () => void;
  onReRecognizeVisible: () => void;
  onRepolishVisible: () => void;
};

type ParamField = {
  key: keyof CutConfig;
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

  return (
    <section className="card">
      <div className="config-row">
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
          </div>
        </details>

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
