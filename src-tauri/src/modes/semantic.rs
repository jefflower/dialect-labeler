//! Mode 2 — semantic sliding-window cutter for Mandarin / 长沙话.
//!
//! Owns:
//!   - `DEFAULT_WHISPER_INITIAL_PROMPT` — what Whisper is told to expect
//!     when the worker config doesn't override. Mode-private, so changes
//!     here can't accidentally regress Mode 1's dialect transcription.
//!   - `DEFAULT_CUT_PROMPT` — the LLM "where do I cut?" instructions
//!     used by the sliding-window loop in `lib.rs::process_single_semantic_file`.
//!     Physically moved out of `lib.rs` so adding a Mode 3 with a
//!     different prompt doesn't sit alphabetically next to this one in
//!     a 10K-line file.
//!   - `run()` (cfg(not(windows))) — the worker pipeline for semantic
//!     tasks.

// ------------------------------------------------------------------
// Mode-private prompts. Both are `pub(crate)` so the rest of the lib
// (the sliding-window orchestrator) can read them without going
// through the `Mode` trait — the trait is the cloud-worker entry
// point, not the only consumer of these strings.
// ------------------------------------------------------------------

/// Whisper `initial_prompt` for Mode 2.
///
/// Why this exact shape (P0 #2 task D):
///   - **First sentence is the genre cue.** Whisper conditions on the
///     prompt's last ~224 tokens; leading with "普通话 / 长沙方言 …
///     录音" pushes it firmly into Chinese-language transcription mode.
///   - **Explicit "禁止英文 / 拼音 / 其它语言".** Without this,
///     `large-v3-turbo` periodically slips into English on heavily-
///     accented Changsha dialect spans (HANDOFF §8 #1). The bare prompt
///     ("普通话口语对话") was too permissive — Whisper interpreted
///     unfamiliar dialect phonetics as "must be English I haven't heard
///     before" rather than "must be Chinese I haven't heard before".
///   - **Phonetic-spelling instruction for dialect words.** Tells
///     Whisper to fall back to phonetic Chinese characters rather than
///     pinyin Roman letters for sounds it doesn't know — this is what
///     dialect annotators actually want anyway (they can clean up later).
///   - **Two concrete Changsha example sentences with the marker
///     characters (冒得 / 咯个 / 晓得 / 哒 / 嘞 / 撇).** Whisper's
///     prompt-conditioning is example-driven — these tokens in the
///     prompt make it much more likely to emit the same tokens for
///     similar phonemes. Same trick as zero-shot LLM prompting.
///
/// Length budget: ~150 tokens, well under the 224-token cap. We don't
/// add more examples because (a) returns diminish past ~3 and (b) each
/// extra token of prompt is one fewer token Whisper can spend on actual
/// transcription context.
pub(crate) const DEFAULT_WHISPER_INITIAL_PROMPT: &str =
    "以下是普通话与长沙方言的口语对话/独白录音。\
请按发音转写为简体中文汉字稿。\
禁止输出英文、拼音或其它语言；遇到方言词按发音落字。\
长沙话常见尾音字：哒、咯、嘞、啵、撇、嘎。\
示例：「我冒得呷过咯个东西」「咯个事我哒晓得撇」。";

/// LLM cut-decision prompt used by `llm_pick_one_cut`. The
/// `{placeholder}`s are filled in by the orchestrator before sending —
/// see lib.rs near the call site for the substitution map.
pub(crate) const DEFAULT_CUT_PROMPT: &str = "你是一个普通话 / 长沙话音频切割助手。\
我会给你一段录音开头 {max_s} 秒的 Whisper 自动转写。每一行是一个 Whisper 自动检测的语音段，\
格式：[序号] start_ms-end_ms ms  转写文本。\n\
\n\
你的任务：在这些行的 end_ms 中挑出**一个**合适的「切第一刀」位置。\n\
\n\
硬性约束（必须满足，否则结果作废）：\n\
- 切点 cut_ms 必须 ≤ {window_ms}（窗口上限）。\n\
- 切点 cut_ms 必须恰好等于某一行的 end_ms（不要凭空算时间）。\n\
- 切点前后的音频要是一句完整的话，不要把一句话切两半。\n\
\n\
优选规则（按优先级）：\n\
1. 切点应 ≥ {min_ms}（短段尽量避免，除非剩余内容确实只有这么短就一个完整意群）。\n\
2. 落在自然句末：句号、问号、感叹号；其次是较长停顿（前后两行 end_ms / start_ms 间隙 ≥ 500ms）。\n\
3. 在 {sweet_lo} - {sweet_hi} ms 之间寻找最靠后的合适点；这个区间内有句末就用它。\n\
4. 避免切在以下位置：\n\
   - 一个完整意群中间（如「我觉得这个东西」后面直接接「特别有意思」，别切在「东西」后）；\n\
   - 语气词或拖音之前（「然后呢…」后面紧跟正题，别留个孤零零的「然后呢」）；\n\
   - 两个并列分句的连接处（用「，」连接的两个并列子句通常算一个语义单元）。\n\
5. 长沙话特征字（哒、咯、嘞、啵、嘎、撇 等）做句末标志时，可以视为句号等价物。\n\
\n\
输出格式（**只输出这一行 JSON**，不要解释、不要 markdown 包围）：\n\
{{\"cut_ms\": <整数>, \"reason\": \"<≤30字说明，包含选了哪行的 end_ms 以及理由>\"}}\n\
\n\
Whisper 转写：\n{transcript}";

// ------------------------------------------------------------------
// Mode handle.
// ------------------------------------------------------------------

// ------------------------------------------------------------------
// QA detection — Mode-2-private. Stage 2 (P0 #2 task E).
//
// We run this on each segment AFTER Whisper transcribes it but BEFORE
// emitting the `SegmentRecord`. The flags are advisory hints for the
// Windows annotator — they don't gate the pipeline.
// ------------------------------------------------------------------

use crate::modes::common::SegmentQaFlag;

/// Minimum segment duration (ms) before "very short text" becomes
/// suspicious. Anything under this is too short for the heuristic to
/// be meaningful (could just be a one-syllable backchannel).
const VERY_SHORT_MIN_DURATION_MS: u64 = 1_000;

/// Text length (in `chars()` units, not bytes) below which a segment
/// of audio is considered to have suspiciously little text.
const VERY_SHORT_TEXT_THRESHOLD_CHARS: usize = 3;

/// Ratio of Chinese characters (`is_chinese_han`) to total non-whitespace
/// characters. Below this we flag `LowChineseRatio`; below 0.2 we
/// upgrade to `NonChinese`.
const LOW_CHINESE_RATIO_THRESHOLD: f64 = 0.6;
const NON_CHINESE_RATIO_THRESHOLD: f64 = 0.2;

/// Inspect a Whisper transcript span and return the QA flags that
/// apply. Stays pure / synchronous / cheap — runs on every segment in
/// the hot path.
///
/// Mode 1 deliberately doesn't call this — its annotator UI has its
/// own per-segment QA flow and a different definition of "suspicious"
/// (dialect transcription is expected to look noisy by design).
pub(crate) fn detect_qa_flags(text: &str, duration_ms: u64) -> Vec<SegmentQaFlag> {
    let mut flags = Vec::new();
    let trimmed = text.trim();

    if trimmed.is_empty() {
        flags.push(SegmentQaFlag::Empty);
        return flags;
    }

    // Count meaningful chars only — punctuation + whitespace are
    // neither Chinese nor "other"; they shouldn't tip the ratio.
    let mut total_meaningful: usize = 0;
    let mut chinese: usize = 0;
    let mut chars_iter = trimmed.chars();
    for ch in &mut chars_iter {
        if ch.is_whitespace() {
            continue;
        }
        // Treat CJK punctuation as Chinese-side (so a sentence with
        // commas + periods doesn't get falsely flagged).
        if is_chinese_punctuation(ch) {
            total_meaningful += 1;
            chinese += 1;
            continue;
        }
        // ASCII punctuation: skip — neutral.
        if ch.is_ascii_punctuation() {
            continue;
        }
        total_meaningful += 1;
        if is_chinese_han(ch) {
            chinese += 1;
        }
    }

    if total_meaningful == 0 {
        // All-punctuation segment — same signal as empty.
        flags.push(SegmentQaFlag::Empty);
        return flags;
    }

    let ratio = chinese as f64 / total_meaningful as f64;
    if ratio < NON_CHINESE_RATIO_THRESHOLD {
        flags.push(SegmentQaFlag::NonChinese);
    } else if ratio < LOW_CHINESE_RATIO_THRESHOLD {
        flags.push(SegmentQaFlag::LowChineseRatio);
    }

    // "Long audio, almost no text" — segment-level signal that Whisper
    // gave up. Use `chars().count()` not byte length (Chinese chars
    // are 3 bytes each in UTF-8).
    if duration_ms >= VERY_SHORT_MIN_DURATION_MS
        && trimmed.chars().count() < VERY_SHORT_TEXT_THRESHOLD_CHARS
    {
        flags.push(SegmentQaFlag::VeryShortText);
    }

    flags
}

#[inline]
fn is_chinese_han(ch: char) -> bool {
    // CJK Unified Ideographs (basic). Covers >99% of contemporary
    // Mandarin / 长沙话 use. We don't include CJK Extensions A-F here
    // — they cover historical / rare chars that wouldn't realistically
    // appear in dialect transcription; including them just lets exotic
    // mojibake slip through the filter.
    matches!(ch, '\u{4E00}'..='\u{9FFF}')
}

#[inline]
fn is_chinese_punctuation(ch: char) -> bool {
    // Full-width punctuation Whisper / annotators commonly use.
    matches!(
        ch,
        '，' | '。' | '、' | '；' | '：' | '？' | '！'
        | '「' | '」' | '『' | '』' | '（' | '）'
        | '【' | '】' | '《' | '》' | '…' | '—'
    )
}

#[cfg(test)]
mod qa_flag_tests {
    use super::*;

    #[test]
    fn detect_empty_text() {
        assert_eq!(detect_qa_flags("", 5_000), vec![SegmentQaFlag::Empty]);
        assert_eq!(detect_qa_flags("   ", 5_000), vec![SegmentQaFlag::Empty]);
    }

    #[test]
    fn detect_all_punctuation_treated_as_empty() {
        // ASCII punctuation alone yields no meaningful chars.
        assert_eq!(detect_qa_flags("...", 5_000), vec![SegmentQaFlag::Empty]);
    }

    #[test]
    fn detect_pure_english_flagged_non_chinese() {
        let flags = detect_qa_flags("hello world this is english speech", 5_000);
        assert!(
            flags.contains(&SegmentQaFlag::NonChinese),
            "expected NonChinese in {flags:?}"
        );
    }

    #[test]
    fn detect_pinyin_flagged_non_chinese() {
        // Whisper sometimes falls back to pinyin when confused.
        let flags = detect_qa_flags("wo bu zhi dao zhe ge shi qing", 3_000);
        assert!(
            flags.contains(&SegmentQaFlag::NonChinese),
            "expected NonChinese in {flags:?}"
        );
    }

    #[test]
    fn detect_normal_mandarin_no_flags() {
        let flags = detect_qa_flags("我今天去了菜市场，买了一些苹果和橘子。", 5_000);
        assert!(
            flags.is_empty(),
            "expected zero flags for clean Mandarin, got {flags:?}"
        );
    }

    #[test]
    fn detect_normal_changsha_no_flags() {
        // Changsha-flavoured sentence with dialect particles — should
        // still pass as clean Chinese.
        let flags = detect_qa_flags("咯个事我哒晓得撇，冒得啥子大不了的。", 5_000);
        assert!(
            flags.is_empty(),
            "expected zero flags for Changsha Mandarin, got {flags:?}"
        );
    }

    #[test]
    fn detect_mixed_mostly_chinese_no_flags() {
        // 60% threshold means "我去 KFC" should NOT be flagged
        // (3 Chinese chars + 3 latin letters = exactly 50% — tip into
        // LowChineseRatio territory but not NonChinese).
        let flags = detect_qa_flags("我去吃 KFC 了", 2_000);
        assert!(
            !flags.contains(&SegmentQaFlag::NonChinese),
            "expected NOT NonChinese in {flags:?}"
        );
    }

    #[test]
    fn detect_very_short_text_for_long_audio() {
        // 5-second audio with a 2-char transcript — Whisper gave up.
        let flags = detect_qa_flags("嗯。", 5_000);
        assert!(
            flags.contains(&SegmentQaFlag::VeryShortText),
            "expected VeryShortText in {flags:?}"
        );
    }

    #[test]
    fn detect_short_text_below_duration_threshold_skips_short_flag() {
        // 500ms backchannel "嗯" — too short to flag as suspicious.
        let flags = detect_qa_flags("嗯", 500);
        assert!(
            !flags.contains(&SegmentQaFlag::VeryShortText),
            "expected NOT VeryShortText in {flags:?}"
        );
    }
}

// ------------------------------------------------------------------
// Mode handle.
// ------------------------------------------------------------------

/// Zero-size marker. `lookup()` returns a `&'static SemanticMode`.
pub(crate) struct SemanticMode;

impl SemanticMode {
    /// Inherent accessor — callable from non-cloud code paths that
    /// don't want to depend on the cloud-gated `Mode` trait.
    #[allow(dead_code)] // used by lib.rs after Stage 2 (D) lands
    pub(crate) const fn default_whisper_initial_prompt() -> &'static str {
        DEFAULT_WHISPER_INITIAL_PROMPT
    }
}

// ------------------------------------------------------------------
// Mode trait impl. Worker-only (Windows reviewer doesn't link cloud).
// ------------------------------------------------------------------

#[cfg(not(target_os = "windows"))]
mod worker {
    use std::fs;

    use serde_json::{json, Value};

    use super::SemanticMode;
    use crate::cloud::{unix_ms, zip_dir, CloudError};
    use crate::modes::{Mode, ModeRunCtx};
    use crate::{run_semantic_pipeline_impl, OllamaEndpointDef, SegmentRecord};

    impl Mode for SemanticMode {
        fn id(&self) -> &'static str {
            "semantic"
        }

        fn default_whisper_initial_prompt(&self) -> &'static str {
            super::DEFAULT_WHISPER_INITIAL_PROMPT
        }

        fn run(&self, ctx: ModeRunCtx<'_>) -> Result<(Value, u64), CloudError> {
            let ModeRunCtx {
                app,
                client,
                claim,
                worker_config,
                scan,
                bundle_dir,
                output_zip,
                mut cut_config,
                started,
                set_stage,
                ..
            } = ctx;

            set_stage("semantic-cutting");

            // Mode-2-private fallback: if the user hasn't configured
            // semantic-specific Ollama endpoints, borrow from the Mode-1
            // pool living in worker_config. This used to be in
            // cloud.rs — bringing it into the mode keeps the cross-mode
            // touchpoint visible from one place.
            if cut_config.semantic_ollama_endpoints.is_empty()
                && cut_config.semantic_endpoint.trim().is_empty()
            {
                if !worker_config.ollama_extra_endpoints.is_empty() {
                    cut_config.semantic_ollama_endpoints = worker_config
                        .ollama_extra_endpoints
                        .iter()
                        .map(|url| OllamaEndpointDef {
                            url: url.clone(),
                            model: worker_config.ollama_model.clone(),
                            enabled: true,
                        })
                        .collect();
                    eprintln!(
                        "[cloud] Mode 2: promoted {} Mode 1 Ollama endpoint(s) → semantic pool",
                        cut_config.semantic_ollama_endpoints.len()
                    );
                } else if let Some(url) = worker_config.ollama_url.as_ref() {
                    cut_config.semantic_endpoint = url.clone();
                    if let Some(model) = worker_config.ollama_model.clone() {
                        cut_config.semantic_model = model;
                    }
                    eprintln!("[cloud] Mode 2: promoted Mode 1 primary endpoint → semantic_endpoint");
                }
            }

            fs::create_dir_all(bundle_dir)
                .map_err(|e| CloudError::Pipeline(format!("mkdir bundle: {e}")))?;
            let input_paths: Vec<String> =
                scan.audio_files.iter().map(|a| a.path.clone()).collect();
            let _ = client.progress(
                &claim.id,
                25,
                "语义切割",
                Some(&format!("{} 个源文件", input_paths.len())),
            );

            let result = run_semantic_pipeline_impl(
                app.clone(),
                input_paths.clone(),
                bundle_dir.to_string_lossy().to_string(),
                cut_config.clone(),
                worker_config.to_recognition_options(),
            )
            .map_err(CloudError::Pipeline)?;

            if !result.errors.is_empty() {
                return Err(CloudError::Pipeline(format!(
                    "semantic pipeline failed for {} source(s): {}",
                    result.errors.len(),
                    result.errors.join("; ")
                )));
            }

            // The semantic pipeline produces Mode-1-compatible segment
            // dirs. We top that off with a project.json so Windows
            // annotators can open the unzipped bundle directly.
            set_stage("bundling");
            let _ = client.progress(&claim.id, 86, "写入 project.json", None);
            let mut all_segments: Vec<SegmentRecord> = Vec::new();
            let mut audio_files_json: Vec<Value> = Vec::new();
            for report in &result.reports {
                if let Some(first_seg) = report.segments.first() {
                    audio_files_json.push(json!({
                        "id": first_seg.source_file_name.clone(),
                        "path": first_seg.source_path.clone(),
                        "fileName": first_seg.source_file_name.clone(),
                        "matchedEmotion": Vec::<String>::new(),
                    }));
                }
                all_segments.extend(report.segments.iter().cloned());
            }
            let project_json = json!({
                "version": 2,
                "savedAt": format!("epoch_ms_{}", unix_ms()),
                "rootPath": ".",
                "projectDir": ".",
                "segmentsDir": "./segments",
                "config": &cut_config,
                "audioFiles": audio_files_json,
                "manifestRecords": Vec::<Value>::new(),
                "segments": &all_segments,
            });
            let project_path = bundle_dir.join("project.json");
            fs::write(
                &project_path,
                serde_json::to_string_pretty(&project_json)
                    .map_err(|err| CloudError::Pipeline(format!("serialize project.json: {err}")))?,
            )
            .map_err(|err| CloudError::Pipeline(format!("write project.json: {err}")))?;

            let _ = client.progress(&claim.id, 88, "打包产物", None);
            zip_dir(bundle_dir, output_zip).map_err(CloudError::Pipeline)?;
            let output_bytes = fs::metadata(output_zip).map(|m| m.len()).unwrap_or(0);
            let total_segments: usize = result.reports.iter().map(|r| r.segment_count).sum();
            let summary = json!({
                "mode": "semantic",
                "segment_count": total_segments,
                "source_files": result.reports.len(),
                "asr_engine": worker_config
                    .whisper_model
                    .clone()
                    .unwrap_or_else(|| "large-v3".to_string()),
                "semantic_endpoint": cut_config.semantic_endpoint.clone(),
                "semantic_model": cut_config.semantic_model.clone(),
                "elapsed_ms": started.elapsed().as_millis() as u64,
            });
            Ok((summary, output_bytes))
        }
    }
}
