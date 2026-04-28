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
npm run tauri dev
```

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
