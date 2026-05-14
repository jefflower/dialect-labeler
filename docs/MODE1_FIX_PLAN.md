# Mode 1 修复跟踪表

> **下个 session 直接看这份**。SESSION_2026-05-13.md §7 的完整分析是这份的来源；这份是浓缩的执行清单。
>
> **代码位置**（2026-05-14 物理隔离后）：所有改动都在 `src-tauri/src/modes/silence.rs`，不要去 lib.rs 找。

---

## 用户硬约束（不能违反）

1. **`max_segment_ms` ≤ 30 秒** 是非协商上限。所有合并 / 切分都要受这个约束。
2. **Mode 1 音频中间会出现较长的静音段**（自由对话场景特征，Mode 2 没有）。所有处理逻辑都要容忍段内长静音。
3. **不能借鉴 Mode 2 的 LLM 切割逻辑**——Mode 2 算法假设窗口内是连续语音，Mode 1 不满足。详见 SESSION §7.3a 的 4 条论证。

---

## 三个问题 × 三组修复

| 问题 | 用户表现 | 根因函数 | 修复方案 | 工作量 |
|---|---|---|---|---|
| **P1 click 噪声**（频谱紫色垂直线 + 听觉爆音）| 截图 1 红框处 | `gate_segment_head_tail` 头尾置零无 ramp（阶跃跳变） | **M1**：边界加 5ms 线性 ramp | 30min + 单测 |
| **P2 段头无静音 padding**（segment 起头立刻是声音） | 截图 3 | `pre_roll_ms=100ms` 太小 + `trim_segment_head_tail` 二次激进 | **M2.A**：默认值改 250-300ms | 5min + 单测 |
| **P3 切得太碎**（切片数量过多） | 用户描述 | `min_silence_ms=400` / `min_segment_ms=300` 默认值过敏感 + 没有短段合并 | **M3.1**：默认值 800 / 2000 + **M3.2**：短段合并后处理 | 5min + 1.5h |

---

## 详细修复方案

### M1: click 噪声 — gate 边界加 ramp

**问题代码**（modes/silence.rs，搜函数名 `gate_segment_head_tail`）：

```rust
// 当前实现：直接置零 → 阶跃跳变 → click
let head_bytes = head * frame_size;
if head_bytes > 0 {
    for b in &mut bytes[data_start..data_start + head_bytes] {
        *b = 0;  // ← 一刀切到 0
    }
}
```

**修复**：在置零区域和首个非零样本之间加 ~5ms 线性 ramp（`ramp_frames = sample_rate * 5 / 1000`）。head 段：前段全 0，最后 5ms 从 0 渐升到原样本；tail 同理。

**测试**：合成「头部 -20dB 噪声 + 中段 -3dB 信号」WAV，gate 后 FFT 验证无不该有的全频段噪声。

**风险**：旧测试 `gate_segment_head_tail_zeroes_only_boundary_noise` 断言「严格 == 0」，要松绑成「ramp 区间允许非零」。

### M2.A: 段头 padding — 调大默认值

**问题代码**（modes/silence.rs，搜 `default_pre_roll_ms`）：

```rust
fn default_pre_roll_ms() -> u64 { 100 }   // ← 改成 250 或 300
fn default_post_roll_ms() -> u64 { 200 }  // 可选同步调大到 250
```

**注意**：改默认值只影响新建任务；老 `project.json` 反序列化时拿文件里的值，不受影响。

### M3.1: 减少敏感切片 — 调大默认值

```rust
fn default_min_silence_ms() -> u64 { 400 }   // → 800
fn default_min_segment_ms() -> u64 { 300 }   // → 2000
```

社区做 ASR 数据集的常用基准：呼吸停顿 > 0.8s 才算句间停顿，最短 2s 才单独成段。

### M3.2: 短段合并后处理（**Mode 1 特有，关键算法**）

**位置**：modes/silence.rs `build_segment_ranges` 末尾加 post-processing。

**算法**：

```text
扫 ranges 数组：
对每个 ranges[i]：
    if duration(ranges[i]) < min_segment_ms * 1.5:
        # 短段。尝试合并到相邻段
        merge_target = 选 ranges[i-1] 和 ranges[i+1] 中较短的一个
        gap_silence = ranges[i].start - merge_target.end  # 跨长静音的时长
        if merge_target.duration + ranges[i].duration + gap_silence <= max_segment_ms:
            合并 + 删除 ranges[i]
        else:
            # 受 30s 上限约束无法合并 → 保留独立短段
            (用户能在 Tauri 客户端再手工处理)
```

**关键设计点**：
- 合并时**算上中间长静音的时长**到总时长上限里
- 总时长 ≤ 30s 才合并，否则不合并（碎，但不能突破 30s）
- **不压缩 / 不删除中间的 silence**——保留为 segment 内部时长。这是 Mode 1 vs Mode 2 的关键不同：Mode 1 segment 内允许有长静音

**测试用例**：
- 短段在中间 → 合并到较短邻段
- 短段在头部 → 合并到 ranges[1]
- 短段在尾部 → 合并到 ranges[-2]
- 合并会超 30s → 保留独立
- 没有相邻段（只有一个短段）→ 保留独立
- 跨长静音合并 → 中间静音计入时长

### M3.3: 段内长静音处理（可选 / 看 M3.2 效果再决定）

如果 M3.2 合并后用户反馈「段内 silence 太长听感不连贯」，再加：用 ffmpeg `silenceremove` filter 压缩段内 > 1s 的静音。**默认不开启**——真实自由对话本来就有停顿，强制压缩听起来不自然。

### M3.4: UX 暴露参数（P2 polish）

`min_silence_ms` / `min_segment_ms` / `max_segment_ms` 在 ConfigBand UI 暴露，让用户根据录音类型自调。

---

## 执行顺序（推荐）

| 步骤 | 任务 | 工作量 |
|---|---|---|
| 1 | **M1** click 修复（gate 加 ramp） | 30min |
| 2 | **M3.1** 默认参数调大 | 5min |
| 3 | **M3.2** 短段合并算法 | 1.5h + 5-6 单测 |
| 4 | **M2.A** pre_roll 默认调大 | 5min |
| 5 | **真实录音 E2E 验证**（必做） | 30min |
| 6 | M3.3 / M3.4 进阶（看 1-5 效果决定） | 可选 |

---

## 测试 + 验证

每个修复都加单元测试。完成 1-5 之后用真实录音做端到端：

1. 用一段已知有 click 问题的真实长沙话录音
2. 跑 Mode 1 切割
3. 用 Audacity 或类似工具肉眼检查：
   - 频谱图无紫色垂直线（M1 验证）
   - 每段开头有可见 silence padding（M2.A 验证）
   - 切片数量合理，无 < 2s 的孤立短段（M3.1 + M3.2 验证）
4. 修复前/后截图存到 `docs/screenshots/mode1-fix-2026-05-XX/` 留档

---

## 风险提示

- **HANDOFF §6 红线**：Mode 1 在生产环境跑通，改之前要让用户拿真实录音确认「新参数切出来的效果 OK」再 commit
- **向后兼容**：改默认值不影响老 `project.json` 反序列化（serde 用文件里的值）
- **测试**：cargo test --lib 77 passed 是基线。改 `gate_segment_head_tail` 加 ramp 后，相关测试断言可能要松绑
- **代码位置全在 `src-tauri/src/modes/silence.rs`**——2026-05-14 物理隔离后，所有 Mode 1 切割代码集中在这一个文件里（1316 行），不用再去 lib.rs 找

---

## 完成后做的事

- 跑 cargo test --lib（应 77+ passed，新单测加进去）
- 真实录音端到端验证 + 截图对比
- commit（建议每个 M 一个 commit 或一波 commit）
- 更新本文件标记「已完成」
- 跑 `npx gitnexus analyze` 重建索引
- push origin main
