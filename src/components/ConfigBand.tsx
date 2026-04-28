import { Scissors, Sparkles } from "lucide-react";
import type { CutConfig } from "../types";

type ConfigBandProps = {
  config: CutConfig;
  onConfigChange: (next: CutConfig) => void;
  autoRecognizeAfterCut: boolean;
  onAutoRecognizeChange: (value: boolean) => void;
  busy: boolean;
  hasAudio: boolean;
  hasSegments: boolean;
  llmEnabled: boolean;
  onCutAll: () => void;
  onRecognizeVisible: () => void;
};

const fields: { key: keyof CutConfig; label: string }[] = [
  { key: "silenceDb", label: "静音阈值 dB" },
  { key: "minSilenceMs", label: "最短停顿 ms" },
  { key: "minSegmentMs", label: "最短语音 ms" },
  { key: "preRollMs", label: "前留空 ms" },
  { key: "postRollMs", label: "后留空 ms" },
];

export function ConfigBand(props: ConfigBandProps) {
  return (
    <section className="card">
      <div className="config-row">
        {fields.map((field) => (
          <label key={field.key} className="number-field">
            <span>{field.label}</span>
            <input
              type="number"
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
        <span style={{ flex: 1 }} />
        <button
          onClick={props.onCutAll}
          disabled={props.busy || !props.hasAudio}
        >
          <Scissors size={14} />
          全部切割
        </button>
        <button
          className="btn-soft"
          onClick={props.onRecognizeVisible}
          disabled={props.busy || !props.hasSegments}
          title={
            props.llmEnabled
              ? "Whisper 识别后用 Ollama 改写为长沙方言记音字"
              : "仅运行 Whisper（在设置中启用 Ollama 后处理）"
          }
        >
          <Sparkles size={14} />
          {props.llmEnabled ? "识别 + 方言改写" : "Whisper 识别"}
        </button>
      </div>
    </section>
  );
}
