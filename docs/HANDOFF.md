# 项目交接文档 — 方言标注工作台 (Dialect Labeler)

写给下一个会话 / 下一位开发者。当前状态、关键设计决策、可继续做的事都在这里。

最后更新：2026-05-14（凌晨），identifier 重构 + 手机号注册 + admin 用户名简化之后

---

## 1. 一句话现状

桌面端（Tauri / Mac + Windows reviewer）+ 云端调度器（FastAPI 在阿里云 ECS）+ Mac Worker（launchd）三件套都跑着，**两个切割模式都能从浏览器上传 → 自动处理 → 下载产物 → Windows 标注员开工**。Mode 1（按静音切方言）+ Mode 2（LLM 滑窗按语义切普通话/长沙话）都已端到端验证。

## 2. 部署清单（活着的东西）

| 角色 | 位置 | 进程/容器 | 重启方法 |
|---|---|---|---|
| 调度器 Web + API | `http://47.93.1.242:8080` | `docker compose up -d` 在 `/srv/dispatcher` | `ssh root@47.93.1.242` 然后 `cd /srv/dispatcher && docker compose restart` |
| Mac Worker | 本机 (开发 Mac) | launchd LaunchAgent | `launchctl kickstart -k gui/$(id -u)/com.lanchs.dialectlabeler.worker` |
| Whisper 池 | 4 节点 tailnet | `services/whisper-server/`（之前部署） | 各自 ssh 进去 `systemctl restart` |
| Ollama (huayu) | tailnet 100.64.0.4 | 已装 qwen2.5:32b + qwen3.5:122b | 略 |

凭据（**2026-05-14 之后**）：
- Dispatcher admin: `admin` / `Admin2026!`（旧 email `admin@example.com` 已由 migration 改成 `admin`）
- Mac Worker bot: `worker-bot@example.com` / `Wb-uS3rT0ken_2026_X`（identifier 保留 email 字符串形式 —— launchd plist 没改）
- 测试上传者: `uploader@example.com` / `Up10ad-T3st-2026`（identifier 保留 email 字符串形式）
- 服务器 SSH: `ssh root@47.93.1.242`（密钥认证，已配）

**注意（2026-05-13/14 起）**：
- 自助注册 (`POST /api/auth/register`) 强制**手机号格式**（11 位 1 开头），不再接受邮箱
- 注册后默认进入「待管理员审核」状态。第一个注册者（bootstrap admin）自动批准，之后所有新注册者必须 admin 审核才能登录
- 审核端点：`POST /api/users/{id}/approve`（admin only）
- 前端 `/admin/users` 页面有「批准」按钮，stats 端点会返回 `pending_user_count`
- 服务器 `.env` 设了 `ALLOW_OPEN_REGISTRATION=true`，未来想关闭直接改 .env + `docker compose up -d --force-recreate`
- 详见 §6.10 / §6.14

## 3. 端到端流程图

```
 浏览器                     阿里云 ECS 47.93.1.242:8080
 ┌─────┐  HTTP        ┌────────────────────────────────┐
 │ Web │ ───────────► │ FastAPI Dispatcher (docker)    │
 │ SPA │   POST       │ ├ SQLite tasks + users + releases │
 │     │   /api/tasks │ ├ /srv/dispatcher/data/storage    │
 └─────┘              │ └ Caddy reverse-proxy 8080→9000  │
                      └────────────────────────────────┘
                              ▲    │
                       claim  │    │ download input zip
                       hb     │    │ upload output zip
                       prog   │    │
                              │    ▼
                          Mac Worker (tailnet 100.64.0.7)
                          ├ launchd LaunchAgent
                          ├ npm run tauri dev (Rust binary)
                          ├ 调度 Whisper × 4 (.2/.4/.6/.11:9090)
                          └ 调度 Ollama qwen2.5:32b @ huayu:11434
```

上传者: 浏览器 → 注册/登录 → 「+ 新建任务」 → 选 Mode 1 或 Mode 2 → 拖入 zip → 等进度条到 100% → 点「下载产物 zip」 → 把整个解压目录发给 Windows 标注员。

Windows 标注员: 装 Tauri 客户端 review 版 → 「打开」 → 选 `project.json` → 编辑文字/标签/情绪 → 「导出 JSONL」。

## 4. 两个切割模式

### Mode 1 — 方言 / 按静音切（旧的工作流）
- 算法：`cut_audio_file_impl`（在 `src-tauri/src/lib.rs`），按 dB 阈值检测静音边界，切分音频
- 用 Whisper 转写每段，用 Ollama 把普通话转写改写成方言记音字（如长沙话）
- 产物：项目目录 + segments/ + manifest.json + 可一键打包成 OSS-ready 的 export bundle
- 适用：双人方言对话录音、文件夹结构按角色分（发音人/陪聊人）

### Mode 2 — 语义 / 滑窗切（新的工作流）
- 算法：`process_single_semantic_file`（在 `src-tauri/src/lib.rs`，Phase 8 章节），**滑动窗口 + 一窗一刀**：
  ```
  cursor = 0
  while cursor < total_ms:
      1. 取 audio[cursor, cursor + window_size_s] 作为窗口
      2. Whisper 整段转写 → segments[]
      3. 提示词 + Whisper segments 喂给 Qwen → 返回 cut_ms
      4. 在 cursor + cut_ms 切下来一段
      5. cursor += cut_ms
  ```
- 关键代码：
  - `process_single_semantic_file` — 滑窗主循环
  - `llm_pick_one_cut` — 单点决策 LLM 调用
  - `transcribe_remote_with_segments` — Whisper /transcribe + 解析 segments[]
  - `DEFAULT_SEMANTIC_CUT_PROMPT` — 普通话/长沙话默认提示词，规范化规则烤进去
- 产物：Mode-1 兼容的项目目录（source/ + segments/ + project.json），Windows 标注员可直接打开

### 自动复用模型配置
- 用户在 Tauri 客户端 Mode 1 面板里配的 Whisper / Ollama 端点池，Mode 2 自动复用（`process_single_semantic_file` 里有 fallback 逻辑）
- 云端 Worker 同样：`cloud.rs` 里 Mode 2 分支如果 cut_config 缺 Mode 2 端点会自动用 worker_config 的 Mode 1 端点

## 5. 代码地图

```
dialect-labeler/
├── src/                       # Tauri 前端 (React + TS)
│   ├── App.tsx                # 主组件、Mode 切换、Cloud autostart 钩子
│   ├── components/
│   │   ├── Topbar.tsx         # 顶栏 + mode tabs (Mode 1 / Mode 2)
│   │   ├── ConfigBand.tsx     # 主面板（每个 mode 一个折叠子面板）
│   │   ├── SettingsDrawer.tsx # 只剩主题切换
│   │   ├── CloudPane.tsx      # 云端 Worker 控制台 (modal)
│   │   └── MainView.tsx       # 标注界面
│   ├── types.ts               # CutConfig / AppSettings / SegmentRecord 等
│   ├── lib.ts                 # IPC 包装 + 共享类型
│   └── env.ts                 # REVIEW_ONLY / HIDE_PROCESSING_UI 标志
│
├── src-tauri/                 # Tauri 后端 (Rust)
│   ├── src/lib.rs             # 共享基础设施 (~9K 行) — audio I/O / scan /
│   │                          # recognize / export / Tauri commands。
│   │                          # 注意：Mode 1/2 的切割算法已移出（2026-05-14）
│   ├── src/cloud.rs           # 云端 Worker 流水线 + trait dispatch 到 modes
│   ├── src/modes/             # ⭐ 每个 cutting mode 一个完整文件
│   │   ├── mod.rs             #   - Mode trait + ModeRunCtx + lookup() 注册表
│   │   ├── common.rs          #   - SegmentQaFlag enum（mode 共享 schema 唯一处）
│   │   ├── silence.rs         #   ⭐ Mode 1 完整切割算法 + dispatch (~1316 行)
│   │   │                      #     cut_audio_file_dialect_impl / detect_silence /
│   │   │                      #     build_segment_ranges / trim/gate_head_tail /
│   │   │                      #     effective_silence_db / analyze_noise_floor_db
│   │   │                      #     SilenceEvent enum 等
│   │   └── semantic.rs        #   ⭐ Mode 2 完整切割算法 + dispatch (~1175 行)
│   │                          #     run_semantic_pipeline_impl /
│   │                          #     process_single_semantic_file /
│   │                          #     llm_pick_one_cut / transcribe_remote_with_segments
│   │                          #     DEFAULT_*_PROMPT / detect_qa_flags
│   ├── tauri.conf.json
│   └── Cargo.toml
│
├── services/dispatcher/       # 服务器端 FastAPI
│   ├── app/
│   │   ├── main.py
│   │   ├── models.py          # Task/User/Release SQLAlchemy
│   │   ├── schemas.py         # Pydantic
│   │   ├── routes/
│   │   │   ├── tasks.py       # /api/tasks (create / list / detail / download / close / retry / delete)
│   │   │   ├── worker.py      # /api/worker/{claim,heartbeat,progress,output,complete,fail}
│   │   │   ├── auth.py        # /api/auth/{register,login}
│   │   │   ├── users.py       # /api/users (admin)
│   │   │   ├── admin.py       # /api/admin/stats
│   │   │   └── releases.py    # /api/releases (Tauri build uploads)
│   │   ├── db.py              # SQLAlchemy + inline migrations
│   │   ├── auth.py            # JWT helpers + password hash
│   │   └── ...
│   ├── tests/                 # 75 个 pytest
│   ├── web/                   # ⭐ SPA (React + TS + Vite)
│   │   └── src/pages/
│   │       ├── LoginPage.tsx
│   │       ├── TasksPage.tsx
│   │       ├── TaskDetailPage.tsx
│   │       ├── UploadModal.tsx # 含 mode picker + 上传格式说明
│   │       ├── DownloadsPage.tsx
│   │       └── Admin*.tsx
│   ├── Dockerfile             # 多阶段 (node + python)
│   └── docker-compose.yml
│
├── scripts/
│   └── mode2_local_cut.py     # ⚠️ 原型，非生产路径（Rust 滑窗代替了它）
│
├── docs/
│   ├── HANDOFF.md             # ← 你正在看的
│   ├── cloud-end-to-end.md    # 用户手册 + 运维 cheat sheet
│   └── mode-2-semantic-cutter.md  # ⚠️ 描述旧 Mode 2 spec，部分过时
│
└── ~/Library/LaunchAgents/com.lanchs.dialectlabeler.worker.plist
    # Mac Worker 配置，含 CLOUD_WORKER_CONFIG_JSON (Whisper + Ollama 端点)
```

## 6. 重要设计决策（不要乱动）

### 6.1 Mode 选择上浮到 Topbar
原来 mode 选择在切割策略折叠里，用户经常没选 mode 就开始切，导致结果跟预期不一致。现在 Topbar 标题右侧两个 tab 强制 first-decision。

### 6.2 SettingsDrawer 只剩主题
之前 SettingsDrawer 装了所有运行配置（Whisper / Ollama / 方言预设 / 标签字典 / 依赖体检 / 项目级 / OSS prefix）。这些都是 **Mode 1 特定**（Mode 2 的导出格式不一样），所以全部移到 Mode 1 面板里。SettingsDrawer 只留真正应用级的主题切换。

### 6.3 Mode 2 输出 Mode-1 兼容项目目录（不是 xlsx）
旧 Mode 2 spec 要求出 xlsx。后来用户调整需求 → "只切，不规范化、不 QA，输出和 Mode 1 一致让 Windows 直接标注"。`run_semantic_pipeline_impl` 输出 `source/ + segments/ + project.json`，xlsx 相关代码全部 dead-code 化了（见 §10）。

### 6.4 Mode 2 滑窗 vs 全局合并
最初实现的是 "pre-cut everything → LLM 合并 → xlsx" 一次性方法。问题：
- LLM context 太大（311 段一锅塞进去 → num_ctx 压力）
- 候选切点池验证太严（LLM 偶尔 round 几 ms → 整批拒绝）
- 单次失败整任务失败

按用户建议换成 **滑窗**：每窗只问 LLM 一刀，prompt 几百 token，可在任何 Ollama 模型上跑。死了的 v1 代码标了 `#[allow(dead_code)]` 留作参考，可以删（见 §10）。

### 6.5 Tailnet 网络 + 系统 HTTP_PROXY 坑
开发机有 Clash/Surge 跑在 127.0.0.1:7897，环境变量 `HTTP_PROXY` 全局生效。Python `requests` 默认走代理 → 502 因为代理够不到 tailnet `100.64.0.0/10` CIDR。Rust `ureq` 默认 NOT honor env，没问题。本地脚本 `scripts/mode2_local_cut.py` 显式 `_session.trust_env = False` 绕过。

如果以后引入新的 HTTP 客户端，记得检查它是否会走 env proxy。

### 6.6 Worker autostart 双保险
1. **前端**: `App.tsx` 启动时读 `cloud-session.json` 里的 `cloud.workerAutostart.v1`，自动登录 + 启用 worker
2. **Rust 端**: setup() hook 检查 `CLOUD_WORKER_AUTOSTART` 环境变量，从纯 Rust 路径登录 + 启用

两条路径独立，互不依赖。launchd 模式下走 Rust 路径（webview 可能没启动也无所谓）；用户手动开 dev 模式走前端路径。

### 6.7 部署只 rsync 应用代码，保留 `.env` + `data/`
```
rsync -avz --exclude 'data/' --exclude '.env' \
  services/dispatcher/ root@47.93.1.242:/srv/dispatcher/
```
注意：**不要加 `--delete`**！加了会把服务器上的 `.env` 删掉，container 起不来。我犯过这个错。

### 6.8 firewalld 默认挡 8080
阿里云 ECS 装的 Alibaba Cloud Linux 3 默认 firewalld 只放 22/80/443。8080 / 9900 必须显式：
```bash
firewall-cmd --zone=public --add-port=8080/tcp --permanent
firewall-cmd --reload
```
重启服务器会丢配置如果忘了 `--permanent`。

### 6.10 账号审核（admin approval gate）
自助注册 (`/api/auth/register`) 后**默认不能登录**——必须 admin 审核。设计：

- 第一个注册者（bootstrap admin）自动批准、直接发 token
- 之后所有 `/api/auth/register` 返回 `RegisterPendingOut`（`status: "pending_approval"`），**不发 token**
- pending 用户登录 → 403 + 「正在等待管理员审核」中文消息
- admin 用 `POST /api/users/{id}/approve` 批准（幂等：重复批准返回 200）
- admin 用 `POST /api/users` 创建的账号**直接预批准**（admin 创建 = 隐含批准）
- admin 用 `DELETE /api/users/{id}` 删除 pending 账号 = 拒绝申请
- `is_approved` / `approved_at` / `approved_by_id` 三列加在 users 表，inline migration 把现存用户全部 grandfather 为 approved=True（避免锁死自己）
- `GET /api/admin/stats` 多了 `pending_user_count` 字段
- 前端 LoginPage 用绿色 banner 显示 pending；AdminUsersPage 顶部用黄色 banner + 行级红色 chip 突出 pending 用户

测试在 `tests/test_auth.py` 9 个 case 覆盖：bootstrap 自动批准、pending 拒登、admin 批准链路、批准幂等、非 admin 不能批准、admin 创建预批准、list_users 排序 pending 在前、stats 计数。

### 6.11 公开 stats 端点 (`/api/public/stats`)
LoginPage 重设计后底部有「活跃用户 / 累计任务 / 处理时长 / 24h 完成」KPI strip。登录前展示 → 不能用 admin/stats。新增 `routes/public.py`：

- 无 auth required
- 只返回聚合数字（用户数、任务总数、处理秒数、24h 计数）
- **不返回**任何 PII（邮箱、任务 id、错误信息）
- 30s 一拉，前端失败静默退回占位

未来加新「safe-pre-login」字段就放这里。**严禁**加任何 per-task / per-user 细节——那是 admin/stats 的活。

### 6.12 Mac Worker tray spinner（菜单栏动画）
原 tray 只是静态图标 + 「打开窗口/退出」菜单。现在 worker 工作时：
- macOS 菜单栏 icon 旁出现文字 spinner（`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`，120ms / 帧）+ 当前阶段中文（「切割中」「识别中」…）
- icon 本身切到 12 帧旋转 spinner（src-tauri/icons/spinner/，每 30° 一帧，scripts/gen_tray_spinner_icons.py 程序生成）
- 菜单顶部两个 disabled item 显示当前任务名 + 阶段
- 任务完成自动停 spinner + 切回 idle icon + 清菜单栏 title

实现位置：`src-tauri/src/lib.rs::TrayController`（在 `build_tray` 区块）。spinner thread 通过 `Arc<AtomicBool>` 取消标志，每次新任务来时先 cancel 旧的再 spawn。菜单更新通过 `ctx.set_stage` 闭包，modes 不直接碰 CloudState（保留 mode 分离边界）。

PNG 资源由 `scripts/gen_tray_spinner_icons.py` 程序生成（Pillow），可随时重跑覆盖。需要换风格直接改 Python 脚本（`INNER_RADIUS_FRAC` / `DOT_RADIUS_FRAC` / opacity 曲线）然后 `python3 scripts/gen_tray_spinner_icons.py`。

### 6.14 Identifier 重构 + 手机号注册（2026-05-14）
`User.email` 列被重命名成 `User.identifier`（更准确地反映它的角色——可以是手机号 / 用户名 / 历史 email）。`POST /api/auth/register` 现在**只接受 11 位手机号**（`^1[3-9]\d{9}$`），其它形式都返回 422。`POST /api/auth/login` 接受任何 identifier 字符串（exact match），所以管理员用 `admin` 登录、service bot 用 `worker-bot@example.com` 登录都行。`POST /api/users`（admin-only）保留任意 identifier 输入——用于创建命名服务账号。

inline migration 在 `app/db.py::_apply_sqlite_inline_migrations`：
1. `ALTER TABLE users RENAME COLUMN email TO identifier`
2. `UPDATE users SET identifier='admin' WHERE identifier='admin@example.com'` — bootstrap admin 一键改名
3. 其它 email-shaped identifier 保留原值（worker-bot / uploader 等）

**API wire field 兼容**：`TaskOut.claimer_email` 字段名保留（避免破坏旧 SPA），但值现在是 `claimer.identifier`（可能是手机号 / 用户名 / 邮箱）。前端 SPA 已经全部从 `user.email` 改成 `user.identifier`。

**前端 UX**：
- 登录 tab：label「账号」，placeholder「手机号 / 用户名」，icon 用 UserIcon
- 注册 tab：label「手机号」，placeholder「11 位手机号」，icon 用 PhoneIcon，11 位 maxLength，提交前 regex 预校验
- AdminUsers 页的「新建账号」表单：placeholder 改成「账号（手机号 / 用户名）」（admin 可以随意创建）

测试覆盖：85 passed = 75 旧 + 10 新（其中 `test_register_rejects_non_mobile_format` 验证 4 种非法 phone format）。`tests/conftest.py::register()` helper 兼容老测试调用（如 `register(client, "founder@example.com")`）：先用一个生成的 fake phone 注册，然后 DB 直接 rename identifier 为传入字符串，避免重写每个测试。

### 6.13 登录页深色 reskin（2026-05-13）
LoginPage 从「居中卡片浅色」改成「双栏深色工业风」，灵感来自一份外部 React 原型（`AI 工作台登录页面设计.zip`）。组件拆到 `services/dispatcher/web/src/pages/login/`：
- `Logo.tsx` — 方形外框 SVG + 内嵌动画波形 bars
- `Pipeline.tsx` — 5 阶段流水线（upload → split → AI → pack → annotate），每 3.2s 推进高亮 + 粒子流动 + AI 阶段用冷色 secondary accent
- `Waveform.tsx` — 底部装饰波形条
- `StatStrip.tsx` — KPI 条 + 实时时钟，从 `/api/public/stats` 拉数据

所有 CSS 都 scope 在 `.login-shell` 容器里，**不污染**其它 admin/tasks 页面（这俩还是原浅色主题）。`oklch()` 色彩空间需要 Safari 15.4+ / Chrome 111+（生产部署内部使用，浏览器 baseline 没问题）。字体用 system fallback（PingFang SC / SF Pro Display / ui-monospace）—— **不引 Google Fonts CDN**，阿里云 ECS 访问境外 CDN 不稳。

保留功能：注册 → pending banner（绿色），login 403 → error banner（红色），bootstrap admin 直接 token。所有审核流程逻辑都没动，只是视觉变了。

### 6.15 物理路径隔离（2026-05-14）
2026-05-14 把 Mode 1 / Mode 2 的切割算法**物理上**搬到各自的模块文件，让下个 session 改一个 mode 时只需要看一个文件。

**搬移后**：
- 改 **Mode 1 切割算法** → 只看 `src-tauri/src/modes/silence.rs`（cut_audio_file_dialect_impl / detect_silence / build_segment_ranges / push_with_smart_split / trim/gate_head_tail / effective_silence_db / SilenceEvent 等都在这里）
- 改 **Mode 2 切割算法** → 只看 `src-tauri/src/modes/semantic.rs`（run_semantic_pipeline_impl / process_single_semantic_file / llm_pick_one_cut / transcribe_remote_with_segments 都在这里）
- 改 **两个 mode 共用的** → lib.rs（probe_audio / write_pcm_wav_segment / SegmentRecord / CutConfig / recognize_segments_impl / Tauri commands）

**留在 lib.rs 的 Mode 2 v1 死代码**：HANDOFF §10 列的 21 个 `#[allow(dead_code)]` 标记的 v1 函数（fine_pre_cut_for_semantic / build_semantic_cut_prompt / validate_semantic_cuts / 等等）**没搬**。理由：它们已是死代码，搬到 modes/semantic.rs 反而污染「干净的当前 Mode 2 代码」语义。HANDOFF §10 仍说「可删但不是优先项」。

**红线**：未来增删 mode 函数时，别又往 lib.rs 加。新 mode 的所有专属代码都应该在 `modes/<new_mode>.rs`。

### 6.9 Mode 绝对分离 — `src-tauri/src/modes/` 架构（P0 #2 落地）
当前两个 mode + 未来更多 mode 共用一套 worker pipeline，但**互不知道对方存在**。规则：

1. **新建 mode = 加一个文件**。在 `src/modes/<new>.rs` impl `Mode trait`（id / default_whisper_initial_prompt / run），在 `modes/mod.rs::lookup()` 加一行 `match` 分支。cloud.rs 不用动。
2. **mode 之间 `use` 互导是禁止的**。看到 `modes/silence.rs` 里 `use crate::modes::semantic::...` 就是回到旧耦合的信号——把共享部分挪到 `modes/common.rs` 或 lib.rs 公共层。
3. **mode 不直接碰 CloudState**。要更新 frontend stage badge 通过 `ctx.set_stage("recognizing")` 闭包（cloud.rs 提供）。
4. **跨 mode fallback 写在消费方**。Mode 2 借 Mode 1 的 Ollama 端点池，这个逻辑住在 `modes/semantic.rs::run()` 里——Mode 2 自己声明它愿意借，Mode 1 不知道有这事。`cloud.rs` 不再有 mode-aware 的 fallback 代码。
5. **prompt 字符串 mode 私有**。每个 mode 的 `DEFAULT_WHISPER_INITIAL_PROMPT` 和 LLM prompts 都是模块级 const。修一个 mode 的 prompt 绝对不会改另一个 mode。
6. **SegmentRecord 加字段 OK，但要 `#[serde(default, skip_serializing_if = "...")]`**。`qa_flags` 是范本：Mode 1 留空 Vec 不序列化，Mode 2 填值，老 project.json 反序列化不破。
7. **`SegmentQaFlag` enum 在 `modes/common.rs`**。enum 数据共享，但**检测逻辑 mode 私有**（`detect_qa_flags` 在 semantic.rs，Mode 1 永远不调）。新加 mode-shared 字段就放 common.rs；不要把检测函数也放进去——那是滑坡。
8. **`#[allow(dead_code)]` 标记的 trait method**：`Mode::id` 和 `default_whisper_initial_prompt` 当前只有 lookup() 路径用得到，预留给 Stage-2-后的诊断 / structured logging。删之前确认确实没人调。

## 7. 关键文件 + 命令速查

```bash
# 改 Rust 算法 → 重启 worker
$ vim src-tauri/src/lib.rs
$ cd src-tauri && cargo test --lib && cd ..
$ launchctl kickstart -k gui/$(id -u)/com.lanchs.dialectlabeler.worker

# 改 Tauri 前端 → vite HMR 自动刷新
$ vim src/components/ConfigBand.tsx

# 改服务器 → rsync + redeploy
$ rsync -avz --exclude 'data/' --exclude '.env' \
    --exclude '__pycache__' --exclude '.venv/' \
    --exclude 'web/node_modules' --exclude 'web/dist' \
    services/dispatcher/ root@47.93.1.242:/srv/dispatcher/
$ ssh root@47.93.1.242 "cd /srv/dispatcher && docker compose up -d --build"

# 看 worker 日志
$ tail -f ~/Library/Logs/dialect-labeler-worker.err.log

# 看 dispatcher 日志
$ ssh root@47.93.1.242 "docker logs --tail 50 -f dialect-labeler-dispatcher"

# 运行所有测试
$ cd src-tauri && cargo test --lib
$ cd services/dispatcher && .venv/bin/python -m pytest -q

# Build 全套
$ npm run build               # Tauri frontend
$ cd services/dispatcher/web && npm run build  # SPA
$ cd src-tauri && cargo build --release        # Rust release
```

## 8. 已知问题 / 注意事项

1. **Whisper 偶尔切到英文模式**：方言录音里偶尔 Whisper 把方言当英文听。**P0 #2 已部分缓解**：（a）Mode 2 默认 Whisper `initial_prompt` 升级为长沙话友好 + 显式禁英文/拼音（`modes/semantic.rs::DEFAULT_WHISPER_INITIAL_PROMPT`）；（b）每个 Mode 2 segment 现在带 `qaFlags`，annotator UI 用红/黄 badge 高亮可疑段（`non-chinese` / `empty` / `low-chinese-ratio` / `very-short-text`）。Mode 1 没改（HANDOFF §6 红线），还是要在 Tauri 客户端手动校正。
2. **Mac Worker 单机瓶颈**：现在只有这一台 Mac 在 launchd 上跑 worker。多用户并发会排队（dispatcher 的 `single-task-per-user` 限制部分缓解）。下一步可以加更多 worker 节点（任何 tailnet 上的 mac 都行，复制 plist 即可）。
3. **服务器没 HTTPS**：用户要求只开 8080，没启用 HTTPS。浏览器密码栏旁边「不安全」是正常的。
4. **下载产物大且慢**：每个 Mode 2 任务的产物 ~200 MB，下载（ECS → 用户）经常 3-5 分钟。可以加 Caddy 的 zstd 压缩，已经开了，但 wav 文件压缩比不高。
5. **`docs/mode-2-semantic-cutter.md` 部分过时**：描述了原 spec 的 7-phase 流程（loudnorm + normalize + auto-QA + xlsx），但现在算法只用了 Phase 3 (pre-cut) 的概念。Phase 8 orchestrator 整个换成滑窗了。
6. **Tauri release 构建未经发布测试**：dev 模式跑得通；release `.app` 在 launchd 下没法直接启动（NSApplication 资源 bundle 问题）。所以 launchd plist 跑的是 `npm run tauri dev`。未来如果要发布给用户，需要做 notarized .app + 改 plist。

## 9. 下一步 — 可继续做的事（按优先级）

### P0 — 直接影响可用性
1. **加更多 Mac Worker 节点**
   - 在另一台 tailnet Mac 上 (`mac-mini` / `mac-pro` / `huayu`) 复制 launchd plist
   - 每台用不同的 worker bot 账号（避免 lease 冲突）
   - dispatcher 的 `/api/worker/claim` 是 SELECT FOR UPDATE 安全的，多 worker 不会抢同一个任务
   - 验收：上传 2 个 Mode 2 任务，看是否被 2 个 worker 并行处理
2. **Whisper 转写质量改进**
   - 当前 large-v3-turbo 偶尔识别成英文。可能调 `initial_prompt` 更准（用更长的方言示例）
   - 或者：在 LLM 切点决策时检测 "明显的英文/乱码 → 把这段标记为 QA-flagged"，让 Windows 标注员重点关注
   - 验收：手动检查 16 段，确认无明显英文 segment

### P1 — 体验改进
3. **Tauri release 打包 + 分发**
   - `cargo tauri build` → `.dmg` (Mac) + `.msi` (Windows)
   - Mac 上需要 Apple Developer ID 签名 + notarization
   - 上传到 dispatcher 的 `/api/releases`
   - 用户从 `/downloads` 页下载
   - 当前 reviewer build 已经有 GH Actions workflow（`.github/workflows/build-windows.yml`），还需要 macOS workflow
4. **Worker 进度细化**
   - 现在 Worker 在 ASR + LLM 阶段不上报中间进度（卡在 25%）
   - 可以在每个窗口完成后 push 一次 `client.progress()` 带 "段 X/总估算 Y"
   - 文件：`src-tauri/src/cloud.rs` 的 `run_pipeline` Mode 2 分支
5. **Mode 1 / Mode 2 端点池统一**
   - 用户已经要求两个 mode 共用配置。Rust 端有 fallback 但 UI 上还是分两个面板编辑
   - 可以在 Mode 2 面板把端点池替换成 "复用 Mode 1（推荐）" 单选 + "Mode 2 独立配置" 单选
   - 简化设置复杂度

### P2 — 技术债 / cleanup
6. **删除 v1 Mode 2 死代码**
   - `#[allow(dead_code)]` 标记了 20 个 v1 helper（Phase 4-7）+ 它们的测试
   - 当前文件 `src-tauri/src/lib.rs` ~10K 行，删了 v1 能省 ~900 行
   - 风险低：v2 已经在生产环境跑通，git history 可恢复
   - 文件区域：1680-2348 + 3041-3287（外加对应 tests）
7. **更新过时文档**
   - `docs/mode-2-semantic-cutter.md` 还在讲 7-phase 旧流程，更新成滑窗
8. **dispatcher 用 Postgres 替代 SQLite**
   - 现在 SQLite 跑 single-instance 没问题
   - 但加 Worker 数 + 用户数后写入并发可能成瓶颈
   - 数据量不大，迁移成本低
9. **`/api/tasks/{id}/output` 加 Range 支持**
   - 现在 200MB zip 是一次性 GET，断网就要重下
   - FastAPI 内置 `FileResponse` 支持 Range，但要确认有正确返回 `Content-Range` header
10. **添加 admin 后台批量操作**
    - 现在 admin 后台 (`/admin/dashboard`) 只能看数据，不能批量清理 expired 任务
    - 加 "全选删除" / "重试所有 failed"

### P3 — 长远规划
11. **完整 dialect 标注流程文档化**
    - 给标注员的 SOP（每段听 → 校文字 → 加情绪 → 加 tags → 段级 tag → 注释问题 → 下一段）
12. **JSONL 导出验证**
    - 现在 export 出来的 JSONL 没有自动 schema 校验
    - 可以加 Python 校验脚本（`tools/validate_jsonl.py`）
13. **多语言支持**
    - Tauri UI 是中文写死的，没 i18n 框架
    - 如果要给其他方言团队用（粤语、闽南语、客家话），现在只能改 prompt 不能改 UI

## 10. v1 死代码清单（保留参考，可删）

打了 `#[allow(dead_code)] // legacy Mode 2 v1` 标记的 21 个项：

| 文件:行 | 项 | 用途 |
|---|---|---|
| lib.rs:1681 | `fine_pre_cut_for_semantic` | v1 pre-cut（细切到 ~5s pieces） |
| lib.rs:1741 | `struct SemanticCutDecision` | v1 LLM 返回的切点 |
| lib.rs:1749 | `struct AsredPiece` | v1 pre-cut piece + ASR |
| lib.rs:1764 | `build_semantic_cut_prompt` | v1 一次性合并 prompt |
| lib.rs:1822 | `parse_semantic_cut_response` | v1 LLM 响应解析 |
| lib.rs:1898 | `validate_semantic_cuts` | v1 候选池验证 |
| lib.rs:1992 | `llm_decide_semantic_cuts` | v1 主调用 |
| lib.rs:2048 | `_one_endpoint` | v1 子调用 |
| lib.rs:2114 | `SEMANTIC_NORMALIZE_PROMPT` | v1 per-segment 规范化 prompt |
| lib.rs:2148 | `llm_normalize_segment_text` | v1 规范化 LLM 调用 |
| lib.rs:2174 | `_one_endpoint` | 子调用 |
| lib.rs:2230 | `parse_semantic_normalize_response` | 规范化响应解析 |
| lib.rs:2267 | `auto_qa_segment` | v1 自动 QA 检查 |
| lib.rs:3057 | `struct SemanticExportSegment` | v1 xlsx 导出 row |
| lib.rs:3071 | `semantic_segment_filename` | v1 spec 命名规则 |
| lib.rs:3081 | `derive_spec_stem` | v1 spec 名解析 |
| lib.rs:3096 | `export_semantic_mode` | v1 xlsx + wav 导出 |
| lib.rs:3132 | `write_semantic_xlsx` | rust_xlsxwriter 用 |
| lib.rs:3211 | `preprocess_audio_for_semantic` | v1 loudnorm + arnndn |
| lib.rs:3279 | `find_rnnoise_model` | v1 rnnoise model finder |

对应的测试也保留着，cargo test 全过。删除时一并删测试 + 可以拿掉 `rust_xlsxwriter` 依赖（在 `src-tauri/Cargo.toml`）。

## 11. 上次会话留的待办 (从前文)

**本次会话（2026-05-13）做了**：
- ✅ P0 #2 D：Mode 2 Whisper prompt 升级（长沙话示例 + 禁英文）
- ✅ P0 #2 E：SegmentQaFlag + detect_qa_flags + Tauri 客户端 badge 展示
- ✅ mode 架构分离：`src-tauri/src/modes/`，cloud.rs 248 行 if/else → 30 行 trait dispatch
- ✅ Mac Worker tray 菜单栏：Braille 文字 spinner + 12 帧旋转 icon + 任务/阶段菜单项（§6.12）
- ✅ 账号审核流程：register pending、admin approve、3 列 migration、9 新测试（§6.10）
- ✅ `/api/public/stats` 公开端点（§6.11）
- ✅ LoginPage 双栏深色 reskin + Pipeline / Waveform / StatStrip 子组件（§6.13）
- ✅ Tests: cargo 77 passed / pytest 84 passed（75 旧 + 9 审核）
- ✅ Dispatcher 已部署 (rebuild + restart)；Mac Worker 已重启用新 tray 代码
- ⚠️ **没在真实任务上跑过端到端**——单测 + 编译 + healthz + public/stats 都过，但 Mode 2 prompt 改进 + qaFlags + tray spinner 在生产里的实际效果还没看到

**2026-05-14 凌晨补做**：
- ✅ 输入框 focus 双层框 bug 修（`.login-shell input` 重置全局 input 样式）
- ✅ identifier 重构 + admin 改成 username "admin"（migration 一键改名）
- ✅ 手机号注册（11 位强校验 + Pydantic validator + 前端 regex 预检）
- ✅ AdminUsersPage / Topbar 全部从 `user.email` 改成 `user.identifier`
- ✅ pytest 85 passed（10 新审核测试 + 1 新 phone-format reject 测试）
- ✅ 已部署 + 服务器 .env `ALLOW_OPEN_REGISTRATION=true`
- ✅ 验证：`admin / Admin2026!` 能登录；新手机号 `13900000001` 能注册并落 pending；非法 phone 422；pending 用户登录 403

仍待做：
- `Tauri release` 打包 + macOS notarization（P1 #3）
- 多 worker 节点扩容（P0 #1，上次会话用户选了暂缓）
- 文档过时（mode-2-semantic-cutter.md，P2 #7）—— Phase 7 旧流程还在文里，应该按 §6.4 滑窗版重写

## 12. 联系方式 / 凭据放哪

- 服务器密码 / SSH key: `~/.claude/ssh-hosts.json` (alias `dispatcher`)
- 调度器 admin 凭据: 见 §2，密码硬编码在 `services/dispatcher/.env` (服务器上 600 权限)
- Worker bot 凭据: 在 `~/Library/LaunchAgents/com.lanchs.dialectlabeler.worker.plist` (本机)
- GH Actions secrets: 需要在 https://github.com/jefflower/dialect-labeler/settings/secrets/actions 配
  - `DISPATCHER_URL` = `http://47.93.1.242:8080`
  - `DISPATCHER_TOKEN` = admin JWT（用 POST /api/auth/login 拿一次，30 天有效）

## 13. 健康检查 (每次会话开始时跑一遍)

```bash
# 1. 服务器活着？
curl -sI http://47.93.1.242:8080/healthz | head -1
# 期望 HTTP/1.1 200 OK

# 2. Worker 活着？
launchctl list | grep dialect
tail -3 ~/Library/Logs/dialect-labeler-worker.err.log
# 期望看到 [autostart] worker enabled

# 3. tailnet 端点活着？
for h in 100.64.0.2 100.64.0.4 100.64.0.6 100.64.0.11; do
  curl -s -o /dev/null -w "$h: HTTP=%{http_code}\n" --max-time 3 http://$h:9090/ || echo "$h: dead"
done
curl -s --max-time 3 http://100.64.0.4:11434/api/tags | head -c 100

# 4. 测试都过？
cd src-tauri && cargo test --lib 2>&1 | tail -3
cd ../services/dispatcher && .venv/bin/python -m pytest -q 2>&1 | tail -3
```

如果任何一项炸了，先看日志、再读 §6 的设计决策、最后再动代码。

---

**祝下一位顺利。** 项目当前是「可以给用户用」的状态——别为了重构再次破坏它。需要重构时按 §9 的优先级一步步来。
