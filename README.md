# Dialect Labeler

本项目是一个基于 Tauri + Rust + React 的跨平台方言音频切割与标注客户端。

## 当前能力

- 可分别配置输入文件夹和输出文件夹，递归扫描输入目录里的 `wav/mp3/m4a/aac/flac/ogg/opus`。
- 扫描后默认自动根据 FFmpeg 静音检测切割音频，也可以调参后手动重新切割。
- 切割参数支持静音阈值、最短停顿、最短语音、前留空、后留空。
- 切割输出强制保存为无压缩 `PCM WAV`，不输出 MP3/AAC/OGG 等有损格式。
- 播放器为 24-bit WAV 自动生成无压缩 16-bit 预听副本，并通过 Rust 原生音频输出播放，最终切割文件不受影响。
- 本地编辑原文本、记音字、情感、笑声、呼吸、停顿、听不清、噪声和备注。
- 左侧选择原音频，中间只显示对应的切割片段；点击片段进入全窗体标注模式。
- 支持本地 `whisper` 生成识别初稿，可批量识别，也可在标注模式下单独识别当前片段。
- 保存本地工程文件：`输出文件夹/project.json`。
- 导出最终结果：自定义路径或默认 `输出文件夹/export.jsonl`。

## 开发运行

```bash
npm install
npm run tauri dev
```

## 验证

```bash
npm run build
cd src-tauri
cargo test
cargo check
cargo fmt --check
```
