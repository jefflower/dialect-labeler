# 长沙方言标注工作台 (Dialect Labeler)

基于 **Tauri + Rust + React** 的跨平台方言音频切割与标注客户端，专为长沙话（湘语）数据集制作而设计。

## 核心能力

### 流水线
```
原始音频 → 静音切片 (FFmpeg)
        → Whisper 语音识别 (普通话初稿)
        → Ollama LLM 后处理 (长沙记音字 + 情绪 + 笑声/呼吸标签)
        → 人工校对
        → 导出 JSONL (与 demo 格式完全一致)
```

### 模型策略
- **Whisper** 负责"听音"：声学模型把音频转成普通话初稿。可在设置中选择 `tiny → large-v3` 任意尺寸；推荐 `large-v3`。
- **Ollama** 负责"改字"：本地大语言模型按"记音字优先"的原则把普通话改写为长沙方言常用字（港/噶/哒/咯/啵/嘞 等），并初步标注情绪与内联标签。推荐 `qwen2.5:32b`（中文最强）或 `qwen2.5:14b/7b`（更轻量）。
- **缓存**：识别结果按音频 hash 缓存到 `.asr/cache/`，重跑同一段不重新调用 Whisper。

### UI / UX
- 明 / 暗 / 跟随系统三种主题，色彩与字体可读性优先
- 三栏工作区：原音频列表 + 切割片段 + 当前文件元信息
- **真实波形可视化**（FFmpeg 解码采样，点击波形跳转）
- 全屏标注模式，含情绪 / 标签快捷按钮
- 拖拽文件夹到窗口即可加载
- `?` 键弹出快捷键面板
- 设置抽屉：模型选择、Prompt 编辑、System Prompt、Ollama 端点、依赖体检
- 实时通知（Toast）+ 顶栏状态指示

### 标注与导出
- 内联标签：`<laugh>`、`<breath>`、`<strong>`、`<pause/>`
- 段级标签：笑声 / 呼吸 / 停顿 / 听不清 / 噪声
- 情绪枚举：中立 / 开心 / 惊讶 / 疑问 / 生气 / 难过
- 自定义 System Prompt（如"长沙本地人，女性，25岁左右…"）
- 导出格式与 demo 一致：每行 `{"messages":[system, user, assistant]}`，按文件名前缀自动配对 user / assistant

## 安装依赖

```bash
# macOS
brew install ffmpeg
pip install openai-whisper

# Ollama (https://ollama.com)
brew install ollama
ollama serve &
ollama pull qwen2.5:32b   # 推荐；4.7GB / 7b 也可，9GB / 14b 折中
```

首次跑 `whisper` 会自动下载模型；large-v3 约 3GB。

## 开发

```bash
npm install
npm run tauri dev                       # 完整版（macOS / Linux 用）
VITE_REVIEW_ONLY=1 npm run tauri dev    # 审核版预览（隐藏所有模型 UI）
```

## 两个发行版本

| 版本 | 平台 | 包含 | 用途 |
|---|---|---|---|
| **完整版** | macOS / Linux | Whisper + Ollama 流水线、切割、识别、改写、AI 重标 | 数据生产 |
| **审核版** | **Windows** | 只有审核 / 调整 / 导出 | QA 人员审改已处理过的标注 |

**审核版**通过 GitHub Actions 自动打包发布（见 `.github/workflows/build-windows.yml`）。
推送到 `main` 触发构建并产出 `.msi` / `.exe` 工件；打 `v*` tag 自动起 Release 草稿附件。

### Windows 审核版工作流

1. 你（macOS 完整版）跑 Whisper + Ollama 处理完一批音频，得到 `_dialect_labeler/` 项目目录（含 `project.json`、`segments/`）
2. 用「📦 一键打包」导出 bundle（含 segments + source + JSONL + README）发给 QA
3. QA（Windows 审核版）：
   - 选择 bundle 目录作为「输入文件夹」
   - 同一路径作为「输出文件夹」（让审核版读到 `project.json` 自动恢复进度）
   - 浏览片段、调整字 / 标签 / 情感 / 备注
   - 「📥 导出 JSONL」或「📦 一键打包」回交

### Windows 审核版的运行依赖

- **必须** 安装 [FFmpeg](https://ffmpeg.org/download.html) 并加入 PATH（用于波形解析与音频预听）
  - 推荐：`winget install Gyan.FFmpeg` 或下载 `ffmpeg.exe` 放进 `C:\Windows\System32`
- 不需要 Python / Whisper / Ollama / GPU

## 验证

```bash
npm run build              # 前端构建 + tsc
cd src-tauri
cargo test                 # 6 个单元测试
cargo check
cargo fmt --check
```

## 输出格式

每行一个 JSON 对象：

```json
{
  "messages": [
    {"role": "system", "content": "长沙本地人，女性，25岁左右..."},
    {"role": "user", "content": "...", "audio_file": "..."},
    {"role": "assistant", "content": ["..."], "audio_file": "...", "emotion": ["中立"]}
  ]
}
```

设置中可：
- 关闭 `pair_user_assistant` 改为每段一行
- 配置 `audio_file_prefix`（如 `oss://...`）替换本地路径

## 关键快捷键

| 快捷键 | 功能 |
|---|---|
| `Space` | 播放 / 暂停 |
| `J` / `K` | 下一段 / 上一段 |
| `L` / `B` / `P` / `U` / `N` | 切换 笑声 / 呼吸 / 停顿 / 听不清 / 噪声 |
| `?` | 弹出完整快捷键面板 |
| `Esc` | 关闭弹窗 / 退出标注 |

## 项目存储

- `输出文件夹/project.json` — 整个工程（音频列表 + 切割结果 + 标注 + 配置）
- `输出文件夹/segments/` — 切割后的 PCM WAV 文件
- `输出文件夹/.playback/` — 24/32-bit 音频的 16-bit 预听副本
- `输出文件夹/.asr/cache/` — Whisper 识别结果缓存

## 设计原则

- **记音字优先**：选择"发音接近"的汉字而非"语义对应"的汉字。例如长沙人把"讲"读成 gǎng，应写"港"。
- **本地优先**：所有处理在本机完成，不上传任何音频
- **可追溯**：每段保留 `start_ms` / `end_ms` / 原文件路径
- **可恢复**：设置存 `localStorage`，工程存 `project.json`，可随时从中断处继续

## 标注规范

### 切分
- 按逻辑切分，语义完整，**最长不超过 30 秒**（超过会高亮警告，建议手动拆分或调小切割参数）
- 磕巴的、读错的整段删除（不要)
- 一个单句的情绪和风格保持稳定，不要句间情绪突变
- 句间静音段最长不超过 0.4s
- 句首静音不超过 100ms，句尾静音不超过 200ms（默认切割参数已对齐）
- 首尾呼吸声删除，句中呼吸声保留
- 其他发音人无关的杂音清零（变静音）

### 命名规范

文件命名遵循 `<前缀>_<话题号>_<轮次>[_<子段>]_<角色>.wav`：

- 第 1 个数字：第几个话题/段文本
- 第 2 个数字：本话题内第几个话轮
- 第 3 个数字（仅发音人）：本轮内第几个音频（发音人说得多，切分后自然多于陪聊）

```
自由聊天_0001_01_陪聊.wav            自由聊天_0001_01_01_发音人.wav
                                     自由聊天_0001_01_02_发音人.wav
自由聊天_0001_02_陪聊.wav            自由聊天_0001_02_发音人.wav
自由聊天_0001_03_陪聊.wav            自由聊天_0001_03_发音人.wav
自由聊天_0001_04_陪聊.wav            自由聊天_0001_04_01_发音人.wav
                                     自由聊天_0001_04_02_发音人.wav
                                     自由聊天_0001_04_03_发音人.wav
```

前缀可选：`自由闲聊` / `文案演绎` / `长尾语料`。返修时在文件夹和 jsonl 里加 `_二次提交` 标识。

工具的 `pair_key` 算法会自动按这个命名规则把 `_发音人` / `_陪聊` 配对，导出 JSONL 时按 `{messages: [system, user, assistant]}` 嵌套。

### 转写规则
- **所听即所写**：使用中文简体，按音频实际念的字转写
- **数字一律用中文**（TN 规范）：错 `20%` → 对 `百分之二十`；错 `12:30` → 对 `十二点三十`
- **重复字保留**：发音是"我们去哪哪里啊"就转写两个"哪"，不能省成"哪里"
- **WER < 1%** 是质量目标
- **记音字优先**：发音人角色按方言发音选字（讲→港、骗/逗→策、那个→噶杂）

### 副语言事件标注

10 种合法标签：

**离散事件 `[xxx]`**（无内容）：
- `[laugh]` 笑声 / `[breath]` 呼吸 / `[cough]` 咳嗽 / `[clucking]` 咯咯笑 / `[hissing]` 嘘声 / `[sigh]` 叹气 / `[lipsmack]` 舔唇 / `[swallowing]` 吞口水

**包裹文本 `<xxx>...</xxx>`**：
- `<laugh>笑着说的字</laugh>` / `<strong>重读字</strong>`

约束：
- 多个副语言标签不要同时出现
- 笑声后紧接呼吸声 → 用 `[laugh]` 即可，不必再加 `[breath]`
- `[laugh]` 与 `<laugh>...</laugh>` 互斥

### 情感（句级别，必选 1 个）

`中立 / 开心 / 愤怒 / 悲伤 / 惊讶 / 恐惧 / 厌恶`

约束：
- 情感标签不涉及演绎身份（性格词不算情感）
- 一个单句的情感保持稳定
