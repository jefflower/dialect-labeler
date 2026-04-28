import type { AppSettings, CutConfig } from "./types";

export const defaultCutConfig: CutConfig = {
  silenceDb: -35,
  minSilenceMs: 450,
  minSegmentMs: 300,
  preRollMs: 100,
  postRollMs: 200,
};

export const defaultAppSettings: AppSettings = {
  theme: "system",
  whisperModel: "large-v3",
  whisperInitialPrompt:
    "以下是中文方言（长沙话）口语转写，请用汉字记录听到的字音，不要翻译。",
  useAsrCache: true,
  useLlm: true,
  ollamaUrl: "http://localhost:11434",
  ollamaModel: "qwen2.5:32b",
  llmPrompt: "",
  batchSize: 4,
  systemPrompt: "长沙本地人，女性，25岁左右，声音娇柔，声音清亮",
  pairUserAssistant: true,
  audioFilePrefix: "",
};

export const whisperModels = [
  { value: "tiny", label: "tiny", note: "≈75MB · 极快但很糙" },
  { value: "base", label: "base", note: "≈150MB · 普通话勉强" },
  { value: "small", label: "small", note: "≈500MB · 不错" },
  { value: "medium", label: "medium", note: "≈1.5GB · 推荐入门" },
  { value: "large-v3", label: "large-v3", note: "≈3GB · 推荐生产" },
  { value: "large-v2", label: "large-v2", note: "≈3GB" },
  { value: "large", label: "large", note: "≈3GB" },
];

export const tagOptions = [
  { value: "laugh", label: "笑声", key: "L" },
  { value: "breath", label: "呼吸", key: "B" },
  { value: "pause", label: "停顿", key: "P" },
  { value: "unclear", label: "听不清", key: "U" },
  { value: "noise", label: "噪声", key: "N" },
];

export const inlineTags = [
  { tag: "laugh", label: "笑声" },
  { tag: "breath", label: "呼吸" },
  { tag: "pause", label: "停顿" },
  { tag: "unclear", label: "听不清" },
];

export const emotionOptions = [
  "中立",
  "开心",
  "惊讶",
  "疑问",
  "生气",
  "难过",
];

export const roleLabels: Record<string, string> = {
  user: "陪聊",
  assistant: "发音人",
  unknown: "未知",
};

export const SETTINGS_KEY = "dialect-labeler/settings/v1";
