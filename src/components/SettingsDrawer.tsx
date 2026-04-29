import { useEffect, useState } from "react";
import {
  Activity,
  Bot,
  Cpu,
  HardDrive,
  Mic,
  Plus,
  RefreshCcw,
  Sparkles,
  Tags as TagsIcon,
  Trash2,
  X,
} from "lucide-react";
import type {
  AppSettings,
  DependencyStatus,
  InlineTagDef,
  SegmentTagDef,
} from "../types";
import {
  defaultEmotions,
  defaultInlineTags,
  defaultSegmentTags,
  whisperModels,
} from "../defaults";
import { REVIEW_ONLY } from "../env";
import { ipc } from "../lib";

type SettingsDrawerProps = {
  open: boolean;
  settings: AppSettings;
  onChange: (patch: Partial<AppSettings>) => void;
  onResetPrompt: () => void;
  onPurgeOrphanTags?: () => void;
  onClose: () => void;
};

function prefixPreview(prefix: string, relPath: string): string {
  const trimmed = (prefix ?? "").replace(/\/+$/, "");
  if (!trimmed) return `(留空)/${relPath}`;
  return `${trimmed}/${relPath}`;
}

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
          onChange({ ...tag, key: event.target.value.toUpperCase().slice(0, 1) })
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
          onChange({ ...tag, key: event.target.value.toUpperCase().slice(0, 1) })
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

export function SettingsDrawer({
  open,
  settings,
  onChange,
  onResetPrompt,
  onPurgeOrphanTags,
  onClose,
}: SettingsDrawerProps) {
  const [models, setModels] = useState<string[]>([]);
  const [modelLoading, setModelLoading] = useState(false);
  const [deps, setDeps] = useState<DependencyStatus | null>(null);
  const [depsLoading, setDepsLoading] = useState(false);

  useEffect(() => {
    if (!open) return;
    void refreshModels();
    void refreshDependencies();
  }, [open]);

  async function refreshModels() {
    setModelLoading(true);
    try {
      const list = await ipc.listOllamaModels({ url: settings.ollamaUrl });
      setModels(list);
    } catch {
      setModels([]);
    } finally {
      setModelLoading(false);
    }
  }

  async function refreshDependencies() {
    setDepsLoading(true);
    try {
      const status = await ipc.checkDependencies({
        ollamaUrl: settings.ollamaUrl,
      });
      setDeps(status);
    } catch (error) {
      setDeps({
        whisperOk: false,
        whisperPath: null,
        whisperError: String(error),
        ffmpegOk: false,
        ffmpegError: null,
        ollamaOk: false,
        ollamaUrl: settings.ollamaUrl,
        ollamaModels: [],
        ollamaError: null,
      });
    } finally {
      setDepsLoading(false);
    }
  }

  return (
    <>
      <div
        className={`drawer-overlay ${open ? "open" : ""}`}
        onClick={onClose}
      />
      <aside className={`drawer ${open ? "open" : ""}`} aria-hidden={!open}>
        <header className="drawer-head">
          <h2>设置</h2>
          <button className="btn-ghost btn-icon" onClick={onClose} aria-label="关闭设置">
            <X size={16} />
          </button>
        </header>
        <div className="drawer-body">
          <section className="drawer-section">
            <h3>
              <Cpu size={14} />
              主题
            </h3>
            <div className="drawer-row">
              <label>外观</label>
              <select
                value={settings.theme}
                onChange={(event) =>
                  onChange({ theme: event.target.value as AppSettings["theme"] })
                }
              >
                <option value="system">跟随系统</option>
                <option value="light">浅色</option>
                <option value="dark">深色</option>
              </select>
            </div>
          </section>

          {!REVIEW_ONLY && <section className="drawer-section">
            <h3>
              <Mic size={14} />
              Whisper（语音→普通话）
            </h3>
            <p className="section-hint">
              纯文本 LLM 不能听音，必须先用 Whisper 把声波转成普通话初稿。模型越大越准，但越慢、越占内存。
            </p>
            <div className="drawer-row">
              <label>模型</label>
              <select
                value={settings.whisperModel}
                onChange={(event) => onChange({ whisperModel: event.target.value })}
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
                value={settings.whisperInitialPrompt}
                onChange={(event) =>
                  onChange({ whisperInitialPrompt: event.target.value })
                }
                rows={3}
              />
            </div>
            <div className="drawer-row">
              <label>结果缓存</label>
              <label className="checkbox-field">
                <input
                  type="checkbox"
                  checked={settings.useAsrCache}
                  onChange={(event) =>
                    onChange({ useAsrCache: event.target.checked })
                  }
                />
                <span>命中相同音频时跳过重跑（按文件 hash 缓存）</span>
              </label>
            </div>
          </section>}

          {!REVIEW_ONLY && <section className="drawer-section">
            <h3>
              <Sparkles size={14} />
              Ollama 后处理（普通话→长沙记音字）
            </h3>
            <p className="section-hint">
              对每段 Whisper 初稿调用本地 Ollama，按"记音字优先"原则改写为长沙方言字，并初步标注情绪与笑声/呼吸。
            </p>
            <div className="drawer-row">
              <label>启用 LLM 改写</label>
              <label className="checkbox-field">
                <input
                  type="checkbox"
                  checked={settings.useLlm}
                  onChange={(event) => onChange({ useLlm: event.target.checked })}
                />
                <span>识别后自动调用 Ollama</span>
              </label>
            </div>
            <div className="drawer-row">
              <label>Ollama 端点</label>
              <input
                value={settings.ollamaUrl}
                onChange={(event) => onChange({ ollamaUrl: event.target.value })}
                onBlur={refreshModels}
                placeholder="http://localhost:11434"
              />
            </div>
            <div className="drawer-row">
              <label>Ollama 模型</label>
              <div style={{ display: "flex", gap: 6 }}>
                <select
                  value={settings.ollamaModel}
                  onChange={(event) =>
                    onChange({ ollamaModel: event.target.value })
                  }
                  style={{ flex: 1 }}
                >
                  {models.length === 0 && (
                    <option value={settings.ollamaModel}>
                      {settings.ollamaModel || "未发现可用模型"}
                    </option>
                  )}
                  {models.map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
                </select>
                <button
                  className="btn-ghost"
                  onClick={refreshModels}
                  disabled={modelLoading}
                  title="刷新模型列表"
                >
                  <RefreshCcw size={14} />
                </button>
              </div>
            </div>
            <p className="help-tip">
              推荐 <code>qwen2.5:32b</code>（质量好但大），或更轻量的{" "}
              <code>qwen2.5:14b</code> / <code>qwen2.5:7b</code>。安装：
              <code>ollama pull qwen2.5:32b</code>
            </p>
            <div className="drawer-row stack">
              <label>方言改写 Prompt</label>
              <textarea
                value={settings.llmPrompt}
                onChange={(event) => onChange({ llmPrompt: event.target.value })}
                rows={10}
                placeholder="留空则使用内置的长沙话默认 Prompt"
              />
              <button className="btn-ghost" onClick={onResetPrompt} style={{ alignSelf: "flex-start" }}>
                <RefreshCcw size={14} />
                恢复默认 Prompt
              </button>
            </div>
          </section>}

          <section className="drawer-section">
            <h3>
              <Bot size={14} />
              项目级
            </h3>
            <div className="drawer-row stack">
              <label>System Prompt（出现在导出 JSONL 顶部）</label>
              <textarea
                value={settings.systemPrompt}
                onChange={(event) =>
                  onChange({ systemPrompt: event.target.value })
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
                  checked={settings.pairUserAssistant}
                  onChange={(event) =>
                    onChange({ pairUserAssistant: event.target.checked })
                  }
                />
                <span>导出时按文件名前缀配对（demo 格式）</span>
              </label>
            </div>
            <div className="drawer-row stack">
              <label>OSS 路径前缀（设一次以后不用改）</label>
              <input
                value={settings.audioFilePrefix}
                onChange={(event) =>
                  onChange({ audioFilePrefix: event.target.value })
                }
                placeholder="例：oss://aigc-audio-ext/label/multimodal_audio/studio_recording_dataset_方言"
              />
              <p className="help-tip">
                导出 JSONL 时 <code>audio_file</code> 字段会以这个前缀 +
                "相对于输入根目录的路径" 拼接。例如：
                <br />
                <code>
                  {prefixPreview(
                    settings.audioFilePrefix,
                    "梁才-方言/NO21/WAV/自由演绎_0001_02_陪聊.wav",
                  )}
                </code>
                <br />
                工具不会真的上传文件，<strong>你需要自己把本地切片传到这个 OSS 位置</strong>。
                建议：传到 OSS 时保留这里的相对结构（例如把整个 <code>segments/</code> 目录上传到对应 prefix 下）。
              </p>
            </div>
            {!REVIEW_ONLY && (
              <div className="drawer-row">
                <label>识别批大小</label>
                <input
                  type="number"
                  min={1}
                  max={64}
                  value={settings.batchSize}
                  onChange={(event) =>
                    onChange({ batchSize: Math.max(1, Number(event.target.value)) })
                  }
                />
              </div>
            )}
          </section>

          <section className="drawer-section">
            <h3>
              <TagsIcon size={14} />
              标签字典
            </h3>
            <p className="section-hint">
              用户可动态维护。修改后立即在标注页生效；如需让 LLM 也按新标签输出，请相应更新方言改写 Prompt。
            </p>

            <div className="tag-editor">
              <div className="tag-editor-head">
                <strong>内联标签（10 种默认）</strong>
                <button
                  className="btn-ghost"
                  onClick={() =>
                    onChange({ inlineTags: defaultInlineTags.slice() })
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
                {settings.inlineTags.map((item, idx) => (
                  <InlineTagRow
                    key={idx}
                    tag={item}
                    onChange={(next) =>
                      onChange({
                        inlineTags: settings.inlineTags.map((t, i) =>
                          i === idx ? next : t,
                        ),
                      })
                    }
                    onRemove={() =>
                      onChange({
                        inlineTags: settings.inlineTags.filter(
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
                  onChange({
                    inlineTags: [
                      ...settings.inlineTags,
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
                    onChange({ segmentTags: defaultSegmentTags.slice() })
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
                {settings.segmentTags.map((item, idx) => (
                  <SegmentTagRow
                    key={idx}
                    tag={item}
                    onChange={(next) =>
                      onChange({
                        segmentTags: settings.segmentTags.map((t, i) =>
                          i === idx ? next : t,
                        ),
                      })
                    }
                    onRemove={() =>
                      onChange({
                        segmentTags: settings.segmentTags.filter(
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
                  onChange({
                    segmentTags: [
                      ...settings.segmentTags,
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
                    onChange({ emotions: defaultEmotions.slice() })
                  }
                >
                  <RefreshCcw size={12} /> 恢复默认
                </button>
              </div>
              <div className="emotion-chip-editor">
                {settings.emotions.map((emo, idx) => (
                  <span key={idx} className="tag-chip emotion-chip-edit">
                    <input
                      value={emo}
                      onChange={(event) =>
                        onChange({
                          emotions: settings.emotions.map((e, i) =>
                            i === idx ? event.target.value : e,
                          ),
                        })
                      }
                    />
                    <button
                      className="btn-ghost btn-icon"
                      onClick={() =>
                        onChange({
                          emotions: settings.emotions.filter(
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
                    onChange({
                      emotions: [...settings.emotions, "新情感"],
                    })
                  }
                >
                  <Plus size={12} /> 新增
                </button>
              </div>
              <p className="help-tip">
                标注页 1-{settings.emotions.length} 数字键依次对应这些情感（最多 9 个）。
              </p>
            </div>

            {onPurgeOrphanTags && (
              <div className="tag-editor">
                <div className="tag-editor-head">
                  <strong>数据清理</strong>
                </div>
                <p className="section-hint" style={{ marginTop: 0 }}>
                  扫所有片段的 phoneticText，删除不在以上字典里的 inline 标签
                  （如旧版残留的 <code>&lt;pause/&gt;</code>），保留被包裹的字。
                </p>
                <button
                  className="btn-ghost"
                  onClick={onPurgeOrphanTags}
                  style={{ alignSelf: "flex-start" }}
                >
                  <Trash2 size={14} />
                  清理废弃 inline 标签
                </button>
              </div>
            )}
          </section>

          {!REVIEW_ONLY && <section className="drawer-section">
            <h3>
              <Activity size={14} />
              依赖体检
            </h3>
            <div className="dependency-row">
              <span className="name">FFmpeg</span>
              <span
                className={`status ${deps?.ffmpegOk ? "ok" : "error"}`}
              >
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
              <span
                className={`status ${deps?.whisperOk ? "ok" : "error"}`}
              >
                {depsLoading
                  ? "检测中"
                  : deps?.whisperOk
                    ? "可用"
                    : "缺失"}
              </span>
              <span className="detail">
                {deps?.whisperPath || deps?.whisperError ||
                  "pip install openai-whisper"}
              </span>
            </div>
            <div className="dependency-row">
              <span className="name">Ollama</span>
              <span
                className={`status ${deps?.ollamaOk ? "ok" : "error"}`}
              >
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
              style={{ alignSelf: "flex-start" }}
            >
              <HardDrive size={14} />
              重新检测
            </button>
          </section>}
        </div>
        <footer className="drawer-foot">
          <span style={{ fontSize: 12, color: "var(--text-tertiary)" }}>
            设置自动保存到本地浏览器存储
          </span>
          <button className="btn-primary" onClick={onClose}>
            完成
          </button>
        </footer>
      </aside>
    </>
  );
}
