/**
 * "Realtime task pipeline" visualisation for the login page.
 *
 * Five fixed stages — the same ones the dispatcher actually runs:
 * upload → split → AI → pack → annotate. An auto-advancing `activeIdx`
 * walks the highlight through them so the page feels alive.
 *
 * Stage 3 (AI) deliberately uses a cool accent (var(--cool)) instead of
 * the warm orange — gives the cold-call "this part is the autonomous
 * brain" a visual identity beat. Matches the prototype.
 *
 * Ported from `pipeline.jsx` in the design zip. The inline-style
 * approach mirrors the original — adopt-as-is keeps visual diffs
 * obvious if we ever go back to the mock to compare.
 */

import { useEffect, useState } from "react";

const PIPELINE_STAGES = [
  { id: "upload", label: "上传", desc: "压缩包入栈", code: "UPLOAD" },
  { id: "split", label: "切片", desc: "静音切分", code: "SPLIT" },
  { id: "ai", label: "AI 预处理", desc: "ASR · LLM", code: "INFER", cool: true },
  { id: "pack", label: "打包", desc: "结构化产物", code: "PACK" },
  { id: "label", label: "回流标注", desc: "下发工作台", code: "ANNOT" },
];

type StageId = (typeof PIPELINE_STAGES)[number]["id"];
type State = "done" | "active" | "todo";

function StageIcon({ id, size = 20 }: { id: StageId; size?: number }) {
  const stroke = "currentColor";
  const sw = 1.6;
  switch (id) {
    case "upload":
      return (
        <svg width={size} height={size} viewBox="0 0 24 24" fill="none">
          <path
            d="M12 16V5M12 5L7 10M12 5L17 10"
            stroke={stroke}
            strokeWidth={sw}
            strokeLinecap="round"
            strokeLinejoin="round"
          />
          <path
            d="M4 19h16"
            stroke={stroke}
            strokeWidth={sw}
            strokeLinecap="round"
          />
        </svg>
      );
    case "split":
      return (
        <svg width={size} height={size} viewBox="0 0 24 24" fill="none">
          <path
            d="M3 12h4M9 12h2M13 12h2M17 12h4"
            stroke={stroke}
            strokeWidth={sw}
            strokeLinecap="round"
          />
          <path
            d="M11 6v12M15 6v12"
            stroke={stroke}
            strokeWidth={sw}
            strokeLinecap="round"
            strokeDasharray="2 2"
          />
        </svg>
      );
    case "ai":
      return (
        <svg width={size} height={size} viewBox="0 0 24 24" fill="none">
          <circle cx="12" cy="12" r="3" stroke={stroke} strokeWidth={sw} />
          <circle
            cx="12"
            cy="12"
            r="8"
            stroke={stroke}
            strokeWidth={sw}
            strokeDasharray="3 3"
          />
          <circle cx="12" cy="4" r="1" fill={stroke} />
          <circle cx="20" cy="12" r="1" fill={stroke} />
          <circle cx="12" cy="20" r="1" fill={stroke} />
          <circle cx="4" cy="12" r="1" fill={stroke} />
        </svg>
      );
    case "pack":
      return (
        <svg width={size} height={size} viewBox="0 0 24 24" fill="none">
          <rect
            x="4"
            y="7"
            width="16"
            height="13"
            rx="1.5"
            stroke={stroke}
            strokeWidth={sw}
          />
          <path d="M4 11h16" stroke={stroke} strokeWidth={sw} />
          <path d="M9 4h6v3H9z" stroke={stroke} strokeWidth={sw} />
        </svg>
      );
    case "label":
      return (
        <svg width={size} height={size} viewBox="0 0 24 24" fill="none">
          <path
            d="M4 4h10l6 6v10H4z"
            stroke={stroke}
            strokeWidth={sw}
            strokeLinejoin="round"
          />
          <path
            d="M14 4v6h6"
            stroke={stroke}
            strokeWidth={sw}
            strokeLinejoin="round"
          />
          <path
            d="M8 14h7M8 17h5"
            stroke={stroke}
            strokeWidth={sw}
            strokeLinecap="round"
          />
        </svg>
      );
  }
}

export function Pipeline() {
  const [activeIdx, setActiveIdx] = useState(2);
  // Advance one stage every 3.2s so the highlight is in motion but
  // each stage is readable. Matches the prototype's pacing.
  useEffect(() => {
    const t = window.setInterval(() => {
      setActiveIdx((i) => (i + 1) % PIPELINE_STAGES.length);
    }, 3200);
    return () => window.clearInterval(t);
  }, []);

  return (
    <div className="login-pipeline">
      <div className="login-pipeline-header">
        <div className="login-pipeline-header-label">
          <span style={{ color: "var(--accent)" }}>●</span>
          <span>实时任务流水线</span>
        </div>
        <div className="login-pipeline-header-meta">
          <span>TASK_ID</span>
          <span style={{ color: "var(--text-1)" }}>
            WSD-{new Date().toISOString().slice(0, 10)}-A7
          </span>
        </div>
      </div>

      <div className="login-pipeline-track">
        <div className="login-pipeline-connector" />
        <div
          className="login-pipeline-connector-active"
          style={{
            width: `calc(${
              (activeIdx / (PIPELINE_STAGES.length - 1)) * 100
            }% - 0px)`,
          }}
        />

        <ParticleLayer count={5} />

        {PIPELINE_STAGES.map((s, i) => {
          const state: State =
            i < activeIdx ? "done" : i === activeIdx ? "active" : "todo";
          return (
            <PipelineNode
              key={s.id}
              stage={s}
              state={state}
              idx={i}
            />
          );
        })}
      </div>
    </div>
  );
}

function PipelineNode({
  stage,
  state,
  idx,
}: {
  stage: (typeof PIPELINE_STAGES)[number];
  state: State;
  idx: number;
}) {
  const isAi = "cool" in stage && stage.cool;
  const ringColor =
    state === "done"
      ? "var(--accent)"
      : state === "active"
        ? isAi
          ? "var(--cool)"
          : "var(--accent)"
        : "var(--line)";
  const fillColor =
    state === "done"
      ? "var(--accent-soft)"
      : state === "active"
        ? isAi
          ? "var(--cool-soft)"
          : "var(--accent-soft)"
        : "transparent";
  const iconColor =
    state === "todo"
      ? "var(--text-3)"
      : state === "active"
        ? isAi
          ? "var(--cool)"
          : "var(--accent)"
        : "var(--accent)";
  const labelColor = state === "todo" ? "var(--text-3)" : "var(--text-0)";
  const codeColor = state === "active" ? "var(--accent)" : "var(--text-3)";

  return (
    <div className="login-pipeline-node">
      <div
        className="login-pipeline-node-ring"
        style={{
          borderColor: ringColor,
          background: fillColor,
          color: iconColor,
          boxShadow:
            state === "active"
              ? `0 0 0 6px ${
                  isAi
                    ? "oklch(0.78 0.10 220 / 0.10)"
                    : "oklch(0.80 0.14 75 / 0.10)"
                }`
              : "none",
        }}
      >
        <StageIcon id={stage.id} size={20} />
        {state === "active" && (
          <span
            className="login-pipeline-node-pulse"
            style={{ borderColor: ringColor }}
          />
        )}
      </div>

      <div
        className="login-pipeline-node-label"
        style={{ color: labelColor }}
      >
        {stage.label}
      </div>
      <div className="login-pipeline-node-desc">{stage.desc}</div>
      <div
        className="login-pipeline-node-code"
        style={{ color: codeColor }}
      >
        {String(idx + 1).padStart(2, "0")} · {stage.code}
      </div>
    </div>
  );
}

function ParticleLayer({ count }: { count: number }) {
  return (
    <div className="login-pipeline-particle-layer">
      {Array.from({ length: count }).map((_, i) => (
        <span
          key={i}
          className="login-pipeline-particle"
          style={{ animationDelay: `${i * (3.2 / count)}s` }}
        />
      ))}
    </div>
  );
}
