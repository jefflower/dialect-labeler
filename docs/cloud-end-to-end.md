# 云端整套流程使用手册

「上传 → 等处理 → 下载」三步流。上传方什么都不用装，浏览器就能跑完。

---

## 在线地址

- **入口**：<http://47.93.1.242:8080/>
- **下载页**：<http://47.93.1.242:8080/downloads>（Tauri 客户端）
- **API**：同上 `/api/*`

> 当前为 HTTP，没启用 HTTPS（阿里云这台 ECS 不开 80/443）。浏览器密码栏旁边会标
> 「不安全」是正常的——传文件不走外网密钥流，密码用了一次性账号。

---

## 第一次使用：注册 / 登录

1. 打开 <http://47.93.1.242:8080/> → 「注册」标签。
2. 邮箱、密码（≥ 8 位）→ 注册。
3. 然后右上「登录」回来。

> 现在 `ALLOW_OPEN_REGISTRATION=false`，注册需要管理员开放。要新账号告知 admin 临时改 env 或 admin 直接帮你创建。

---

## 上传任务

1. 准备好你的录音文件夹，目录结构和 Mode 1 标注客户端读取的一致：
   ```
   你的项目名/
   ├─ 发音人/
   │   ├─ XXX-发音人-自由对话-话题01.wav
   │   ├─ XXX-发音人-文本演绎-话题02.wav
   │   └─ ...
   └─ 陪聊人/
       ├─ XXX-陪聊人-自由对话-话题01.wav
       └─ ...
   ```
   把你的项目名文件夹压成一个 `.zip`。
2. 浏览器登录后 → 右上「+ 新建任务」。
3. **第一步**：选 **处理模式**：
   - **模式 1 · 方言**：按静音切，保留全部声学事件。适合方言 / 多人对话录音，产物是
     标准的标注客户端项目目录（含 `manifest.json` + `segments/` + 待人工标注 / 导出 JSONL）。
   - **模式 2 · 普通话语义**：LLM 决定切点，文本规范化，自动产出 `.xlsx`（4 列：源文件名 / 剪辑文件名 / 转写内容 / 问题记录）。**输入必须是普通话**。
4. 任务名、拖入 zip → **创建任务**。
5. 跳到任务详情页，自动刷新进度。

每个用户**同一时间只能有一个未关闭任务**。下载产物后必须点「关闭任务」才能上传下一个，
不然会 409（这是磁盘有限的限制——服务器只有 40 GB 空间）。

---

## 等待处理

任务详情页会显示：
- **状态**：pending → claimed → running → succeeded / failed
- **模式 chip**：你选的那个
- **进度条**：Worker 实时推送，每个阶段一个数字 + 中文标签：

```
5% 初始化  →  10% 解压输入  →  15% 扫描音频  →  25-45% 切割
→  50% 识别  →  85% 打包产物  →  95% 上传产物  →  100% 完成
```

每 3~5 秒自动刷新。如果进度卡住超过 5 分钟 + claim_expires_at 已过期，
说明 Worker 出问题或网卡了，任务会回到 pending 重试。重试 3 次以上自动转 `expired`。

处理速度参考：18 分钟原始音频（80 MB zip） ≈ **3~5 分钟**完成（4 节点 Whisper 并发 + Qwen 32B）。

---

## 下载产物

1. 状态变 `succeeded` 后，进度卡片消失，按钮「下载产物 zip」可点。
2. 浏览器开始下载 `任务名.zip`。
3. **重要**：下载完点「关闭任务」释放服务器存储 + 解锁你的下一次上传。
   - 「关闭」会立刻删除服务器上的输入 + 输出文件（你已经下载到本地了，不需要服务器再保留）
   - 任务记录还在历史列表里，但 zip 不可再下载

产物 zip 解压后是个**完整的标注项目目录**：
- `项目名/` ← 根
  - `manifest.json` — 角色映射 + 时长统计
  - `<原 wav>` ← 原文件全部保留
  - `segments/` ← 切好的片段 wav
  - 模式 2 额外有 `<源文件名>.xlsx` 在根目录

把整个目录用 **Tauri 客户端**（macOS / Windows）「打开文件夹」加载就能继续标注、导出 JSONL。Windows 端是只读审核版（不能再做切割 / 识别，因为 Whisper + Ollama 调用走不通），但能浏览、改 emoji 标签、导出 JSONL。

---

## 失败 / 重试

任务失败时详情页会显示「Worker 报错」红框 + error 文本。可选：

- **重试**：把任务放回 `pending`，下一个空闲 Worker 接单。
  - 系统已有 `attempts <= 3` 上限，第 4 次会直接 `expired`，避免坏数据卡死队列
- **删除**：彻底删任务（记录 + 文件全清）

---

## 当前部署架构

```
浏览器  ─HTTPS禁用，纯 HTTP─►  阿里云 ECS 47.93.1.242:8080
                                  │
                              FastAPI Dispatcher (docker)
                              ├ SQLite 元数据
                              └ 任务文件中转 (/srv/dispatcher/data/storage)
                                  ▲
                                  │ /api/worker/* (Worker 反向轮询)
                                  │
                              Mac Worker (这台开发机，tailnet 内)
                              ├ 调用 4× faster-whisper-server (.2/.4/.6/.11:9090)
                              └ 调用 Ollama qwen2.5:32b @ 100.64.0.4:11434
```

服务器只做**中转 + 任务调度**——不跑 Whisper / Ollama，所以 ECS 配置很轻量（2C / 4G 够用）。
Whisper + Ollama 全在 tailnet 内的 Mac 算力上，外网无法直连，安全。

---

## 运维：Mac Worker 常驻

### 已部署

Worker 已经在本机用 launchd LaunchAgent 跑起来了：
`~/Library/LaunchAgents/com.lanchs.dialectlabeler.worker.plist`。

开机自启，崩了 30 秒后自动重拉。Worker 账号 `worker-bot@example.com`
（password 在 plist 的 EnvironmentVariables 里，所以普通查看也能看到，记得不要把 plist 提交进 git）。

### 常用命令

```bash
# 查状态
launchctl print gui/$(id -u)/com.lanchs.dialectlabeler.worker | head -20

# 查日志
tail -f ~/Library/Logs/dialect-labeler-worker.err.log
tail -f ~/Library/Logs/dialect-labeler-worker.out.log

# 重启 Worker
launchctl kickstart -k gui/$(id -u)/com.lanchs.dialectlabeler.worker

# 临时停用 Worker（卸下）
launchctl bootout gui/$(id -u)/com.lanchs.dialectlabeler.worker

# 重新启用
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.lanchs.dialectlabeler.worker.plist
```

### LaunchAgent 配置说明

环境变量控制 Worker 怎么登录 + 用哪些 Whisper / Ollama 端点：

- `CLOUD_DISPATCHER_URL`：服务器地址（`http://47.93.1.242:8080`）
- `CLOUD_WORKER_EMAIL` / `CLOUD_WORKER_PASSWORD`：worker 账号
- `CLOUD_WORKER_CONFIG_JSON`：完整 worker 配置（Whisper / LLM 池）

> Worker 用的账号是专用 `worker-bot@example.com`，不是上传者用户账号。
> 这样不会和上传者的 single-task-per-user 限制打架。

### 验证 E2E

实地烟测两次都通过：

| 测试 | 输入 | 时长 | 结果 |
|---|---|---|---|
| smoke 1 | 2 个台湾话 WAV（18 min 音频，80 MB zip） | claim → complete = 10 min | succeeded，产物 140 MB |
| smoke 2 (路径修复后) | 1 个台湾话 WAV（9 min 音频，60 MB zip） | claim → complete = 12 min | succeeded，产物 115 MB，所有 `audio_file` 路径正确为 `./segments/...` |

阶段耗时分布（参考）：
- 下载输入：~3 min（阿里云 → tailnet Mac 200 KB/s）
- 解压 + 扫描：<5s
- 切割：<10s
- ASR（Whisper × 4 节点并发）：~80s / 66 段
- LLM polish（Qwen 32B × 4 并发）：~3-5 min / 66 段
- 打包 + zip：~30s
- 上传 + complete：~30s

瓶颈是 LLM polish 阶段（单端点 Qwen）。加端点或加并发可线性提速。
