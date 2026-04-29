import type {
  AppSettings,
  CutConfig,
  CutPresetDef,
  InlineTagDef,
  SegmentTagDef,
} from "./types";

export const defaultCutConfig: CutConfig = {
  silenceDb: -35,
  minSilenceMs: 450,
  minSegmentMs: 300,
  preRollMs: 100,
  postRollMs: 200,
  maxSegmentMs: 30_000,
};

/**
 * Built-in cut strategy presets. Each is tuned for a different recording
 * style. Marked `builtin: true` so the UI knows not to allow editing /
 * deleting them — users can clone them as a starting point for custom
 * presets instead.
 */
export const defaultCutPresets: CutPresetDef[] = [
  {
    name: "闲聊（标准）",
    builtin: true,
    hint: "对话场景默认；中等静音阈值 + 0.45s 停顿",
    config: defaultCutConfig,
  },
  {
    name: "演讲 / 朗读",
    builtin: true,
    hint: "句间停顿明显，最短语音 1s，最长 30s 强制拆",
    config: {
      silenceDb: -38,
      minSilenceMs: 600,
      minSegmentMs: 1000,
      preRollMs: 80,
      postRollMs: 200,
      maxSegmentMs: 30_000,
    },
  },
  {
    name: "快速对话",
    builtin: true,
    hint: "节奏快、停顿短；适合采访、相声、快速交流",
    config: {
      silenceDb: -32,
      minSilenceMs: 280,
      minSegmentMs: 200,
      preRollMs: 60,
      postRollMs: 140,
      maxSegmentMs: 15_000,
    },
  },
];

export const whisperModels = [
  { value: "tiny", label: "tiny", note: "≈75MB · 极快但很糙" },
  { value: "base", label: "base", note: "≈150MB · 普通话勉强" },
  { value: "small", label: "small", note: "≈500MB · 不错" },
  { value: "medium", label: "medium", note: "≈1.5GB · 折中" },
  { value: "large-v3-turbo", label: "large-v3-turbo", note: "≈1.5GB · 推荐 ⭐ 速度快 8 倍，质量近似 large-v3" },
  { value: "large-v3", label: "large-v3", note: "≈3GB · 质量最高，慢" },
  { value: "large-v2", label: "large-v2", note: "≈3GB" },
  { value: "large", label: "large", note: "≈3GB" },
];

/** Default 8 segment-level paralinguistic tags (subset of the inline bracket
 *  tags, used for the JSONL `tags` field). */
export const defaultSegmentTags: SegmentTagDef[] = [
  { value: "laugh", label: "笑声", key: "L" },
  { value: "breath", label: "呼吸", key: "B" },
  { value: "cough", label: "咳嗽", key: "C" },
  { value: "sigh", label: "叹气", key: "X" },
  { value: "hissing", label: "嘘声", key: "H" },
  { value: "lipsmack", label: "舔唇", key: "M" },
  { value: "swallowing", label: "吞口水", key: "W" },
  { value: "clucking", label: "咯咯笑", key: "" },
];

/**
 * Default inline tag dictionary.
 *
 * `paired` entries (laugh, strong) wrap a selection: `<tag>...</tag>`.
 * `bracket` entries are discrete event markers: `[tag]`.
 *
 * Per the labeling spec, the same English name (`laugh`) appears in both
 * forms — `<laugh>...</laugh>` for "笑着说" (text spoken while laughing)
 * and `[laugh]` for an isolated laugh event.
 */
export const defaultInlineTags: InlineTagDef[] = [
  // Paired wrappers
  { tag: "laugh", label: "笑着说", key: "L", kind: "paired", hint: "包裹一边笑一边说出的字" },
  { tag: "strong", label: "重读", key: "S", kind: "paired", hint: "包裹被重音强调的 1-3 字" },
  // Discrete bracket markers
  { tag: "laugh", label: "笑声", key: "G", kind: "bracket", glyph: "笑", hint: "插入 [laugh] 独立笑声标记" },
  { tag: "breath", label: "呼吸", key: "B", kind: "bracket", glyph: "气", hint: "插入 [breath] 呼吸标记" },
  { tag: "cough", label: "咳嗽", key: "C", kind: "bracket", glyph: "咳", hint: "插入 [cough] 咳嗽标记" },
  { tag: "sigh", label: "叹气", key: "X", kind: "bracket", glyph: "叹", hint: "插入 [sigh] 叹气标记" },
  { tag: "hissing", label: "嘘声", key: "H", kind: "bracket", glyph: "嘘", hint: "插入 [hissing] 嘘声标记" },
  { tag: "lipsmack", label: "舔唇", key: "M", kind: "bracket", glyph: "唇", hint: "插入 [lipsmack] 舔唇标记" },
  { tag: "swallowing", label: "吞口水", key: "W", kind: "bracket", glyph: "吞", hint: "插入 [swallowing] 吞口水标记" },
  { tag: "clucking", label: "咯咯笑", key: "", kind: "bracket", glyph: "咯", hint: "插入 [clucking] 咯咯笑标记" },
];

export const defaultEmotions = [
  "中立",
  "开心",
  "愤怒",
  "悲伤",
  "惊讶",
  "恐惧",
  "厌恶",
];

export const defaultAppSettings: AppSettings = {
  theme: "system",
  whisperModel: "large-v3-turbo",
  whisperInitialPrompt:
    "以下是中文方言（长沙话）口语转写，请用汉字记录听到的字音，不要翻译。",
  useAsrCache: true,
  useLlm: true,
  ollamaUrl: "http://localhost:11434",
  ollamaModel: "qwen2.5:32b",
  llmPrompt: "",
  systemPrompt: "长沙本地人，女性，25岁左右，声音娇柔，声音清亮",
  pairUserAssistant: true,
  audioFilePrefix: "",
  inlineTags: defaultInlineTags,
  segmentTags: defaultSegmentTags,
  emotions: defaultEmotions,
  cutPresets: defaultCutPresets,
  llmConcurrency: 2,
  whisperConcurrency: 1,
  ollamaExtraEndpoints: [],
};

export const roleLabels: Record<string, string> = {
  user: "陪聊",
  assistant: "发音人",
  unknown: "未知",
};

export const SETTINGS_KEY = "dialect-labeler/settings/v2";
