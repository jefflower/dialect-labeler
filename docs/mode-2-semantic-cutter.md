# 模式 2 · 普通话语义切割流水线

> 实现自 `录制数据剪辑转写规则（新）`。对方言版（模式 1）的物理静音切法是**完全独立**的另一条路径——按 tab 切换，参数不复用。

## 它做什么

输入：一个或多个 Mandarin 录音 WAV（自由话题 / 播客单播 / 播客双播 / 小说情景演绎 都行）。

输出（spec § 四·三 "按文件夹提交" 布局）：

```
<outDir>/
  <spec_stem>/
    <spec_stem>_000001.wav        ← 6-90s 完整语义单元 1
    <spec_stem>_000002.wav        ← 完整语义单元 2
    ...
    <spec_stem>.xlsx              ← 4 列 spec 表
    .semantic-pipeline/           ← 中间产物（normalised wav + fine pieces）
```

xlsx 列：`源文件名 / 剪辑文件名 / 转写内容 / 问题记录`。第 4 列由 Phase 6 auto-QA 写入，空 = 没问题；有内容 = 审核员需要看一眼。

## 流水线 8 个阶段

由 `run_semantic_pipeline` Tauri 命令（或 cloud worker 自动接管）逐文件顺序执行：

| Phase | 做什么 | 在哪 | 失败处理 |
|---|---|---|---|
| 2 | `ffmpeg loudnorm=I=-18:TP=-1.5:LRA=11` (+ 可选 `arnndn`) | 本地 ffmpeg | 报错中止 |
| 3 | 细预切（silence_db=-35, min_silence=200ms），输出 ~10-30s 小片 + 候选切点池 | 本地 ffmpeg | 报错中止 |
| 3b | 每个小片走 Whisper HTTP `/transcribe` 拿带时间戳的 ASR 文本 | Tailnet whisper-server 池 | 单片失败用空文本，整批继续 |
| 4 | Qwen2.5:32b（`num_ctx=32768`）一次性看完整段 ASR，输出合并后的 6-90s 段 | 必须配 `semanticEndpoint` | 报错中止 |
| 5 | 每个合并段单独再过一次 Qwen，做规范化（标点、数字汉化、`内个→那个`、`他/她/它` 消歧、儿化音、拖长音）| `semanticEndpoint` | 单段失败 fallback 到原始 ASR 文本 |
| 6 | auto-QA：长度 / 数字泄漏 / 英文标点 / 句末标点 / 长停顿密度 / `内个` 未替换 / 文本为空 | 纯本地 | 始终成功，问题进 `问题记录` 列 |
| 7 | 按 LLM 决定的切点从 normalised WAV 切出最终段，写 xlsx | 本地 | 报错中止 |

进度通过 `semantic:progress` Tauri 事件流送到前端，每个阶段开始时 emit 一次。前端 `App.tsx` 已经接好，进度条会显示「`<阶段中文名> · <源文件名>`」。

## 必要的基础设施

| 依赖 | 推荐 | 验证 |
|---|---|---|
| Whisper HTTP fleet | `100.64.0.{2,4,6,11}:9090` 跑 `faster-whisper-server large-v3-turbo` | `curl http://100.64.0.4:9090/health` |
| Qwen 32K-context LLM | **必须** `qwen2.5:32b` 或 `qwen3.5:122b` 装在 ≥40GB RAM 的节点（默认 huayu `100.64.0.4:11434`） | `curl -d '{"model":"qwen2.5:32b","prompt":"…","options":{"num_ctx":32768}}' …/api/generate` |
| ffmpeg | 任何 `>= 4.4` 都行（`loudnorm` filter 自 ffmpeg 4.0+） | `ffmpeg -version` |
| RNNoise 模型（可选） | 把 `cb.rnnn` 放到 `$RNNOISE_MODEL_PATH` 或 `./cb.rnnn` | 自动检测 |

**LLM 端点必须支持 `num_ctx=32K`**——1 小时录音 ASR ≈ 12-18K token，默认 Ollama `num_ctx=2048` 会**静默截断**，导致 LLM 只看到开头 1/8 然后乱给切点。

## 怎么用（GUI）

1. 起客户端：`npm run tauri dev` 或装好的 macOS dmg
2. 「扫描项目目录」选含 WAV 的目录
3. ConfigBand 顶部切到 **「模式 2 · 普通话（语义切割）」** tab
4. 展开「语义参数」，填：
   - **LLM 端点**：`http://100.64.0.4:11434`（huayu）
   - **模型**：`qwen2.5:32b`
   - **num_ctx**：32768
   - 段长 / 响度 / 头尾静音用默认即可
5. 点「切」按钮——进度条会显示 7 个阶段，跑完后去 `<segmentsDir>/<spec_stem>/` 看 xlsx

如果 LLM 端点没填，会弹错提示，**不会**默默用默认 Ollama 池（那个 `num_ctx=2048` 必坏）。

## 怎么用（云端 worker，多用户）

云端上传方在 web SPA 上传 zip 之前没办法直接选 mode 2——上传时只带任务名 + 文件。**模式选择写在 worker 的 `WorkerConfig.cut.mode` 里**：

- 一台 macOS worker 配 `mode = dialect` → 接到的任务都按方言切
- 另一台 worker（或同一台切到另一个 worker 模式）配 `mode = semantic` → 接到的任务都按普通话切

也就是说，目前 dispatcher 不区分任务的 mode，**靠 worker 自己挑模式**。要让同一个 worker 既能跑方言又能跑普通话，需要后续扩 dispatcher schema 加 `task.mode` 字段——v1 不做。

## Spec 里**机器扛不动**的部分

人工审核必须看的 5-10% 案例：

| spec 条款 | 为什么自动化不了 |
|---|---|
| 频谱能量未自然衰减/被截断（§ 七·7-8）| 需要看 spectrogram 视觉判断 |
| 麦距变化导致的忽远忽近 / 变闷变亮 | 需要持续响度分析 + 主观 |
| 衣物摩擦 / 椅子挪动 / 翻页 / 喷麦 / 破音 | 声音事件分类模型，OSS 质量差 |
| 咳嗽 / 清嗓子 / 喷嚏 / 吸鼻子 / 倒吸气 / 喉咙有痰 | 同上 |
| 嘴瓢只发开头声音的字 | 需要 forced alignment + per-char confidence |
| 与 spk_B 重叠语音 | 说话人分离模型，质量差 |
| 同音字（五月 / 五岳 / 五乐）| LLM 会错 |
| 人名 / 地名 / 专有名词核实 | LLM 必错 |
| iZotope RX 级别口水音去除 | 手工活，spec 直接点名 RX |

Phase 6 auto-QA 把**它能机检的全部**异常都标进 `问题记录` 列。审核员只看「问题记录非空 + 听感不对」的段即可。预计将人工工作量从「逐段听」压缩到 **1/5 ~ 1/10**。

## 已知限制

- **首次跑会慢**：huayu Ollama 把 32B + 32K KV cache 加载进显存大约 1.7s（看磁盘速度），首段 LLM 调用因此会比后续慢 30-90s。
- **网络问题**：Tailnet 抖动时单个 Whisper 调用可能超时；目前会用空文本占位继续往下走，xlsx 里那段就会是空文本 + Phase 6 标 "文本为空"。
- **TXT/EXE 命名碰撞**：spec 没说重跑同一个目录怎么处理，目前会**覆盖**已有的 xlsx + WAV 文件。
- **小说情景演绎（旁白 vs 对话）**：spec § 五·剪辑规范 列了「不和谐吸气声切除 / 旁白快吸气剪掉 / 长静音不超 1000ms」一堆人工剪辑细则；这套规则**没自动做**，需要审核员后处理。

## 跑端到端烟测

跑这个 cargo 测试就能自动合成 Mandarin 样本 + 端到端验证整条管线：

```bash
SMOKE_WHISPER_URL=http://100.64.0.4:9090 \
SMOKE_OLLAMA_URL=http://100.64.0.4:11434 \
cargo test --manifest-path src-tauri/Cargo.toml --lib \
  smoke_e2e_semantic -- --nocapture
```

成功标志：终端打印每个阶段的 `[smoke]` 行，结束 `=== E2E SMOKE PASSED ===`。
