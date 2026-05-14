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
    // `run_semantic_pipeline_impl` lives in this same file now (was in
    // lib.rs before the 2026-05-14 migration). Just call it directly.
    use super::run_semantic_pipeline_impl;
    use crate::{OllamaEndpointDef, SegmentRecord};

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

// ============================================================
// Mode 2 cutting algorithm — moved out of lib.rs (2026-05-14)
// All these functions used to live in lib.rs. Centralised here so
// changes to the sliding-window LLM-cut algorithm live in one file.
// ============================================================

// Imports for the moved-from-lib.rs functions below.
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::Emitter;

use crate::{
    ensure_ffmpeg, output_pcm_codec, path_file_name, path_file_stem, path_to_string,
    probe_audio, strip_thinking, truncate_for_log, uniq_token, write_pcm_wav_segment,
    CutConfig, CutMode, OllamaEndpointDef, RecognitionOptions, SegmentRecord,
    SemanticPipelineFileReport, SemanticPipelineResult, WhisperResult, WhisperSegment,
};

// ---- run_semantic_pipeline_impl (was lib.rs:2339) — public entry from Tauri wrapper ----
pub(crate) fn run_semantic_pipeline_impl(
    app: tauri::AppHandle,
    input_paths: Vec<String>,
    out_dir: String,
    config: CutConfig,
    recognition_options: RecognitionOptions,
) -> Result<SemanticPipelineResult, String> {
    use tauri::Emitter as _;
    ensure_ffmpeg()?;
    if input_paths.is_empty() {
        return Err("no input audio paths supplied".into());
    }
    if config.mode != CutMode::Semantic {
        return Err(format!(
            "run_semantic_pipeline only supports mode=semantic (got {:?})",
            config.mode
        ));
    }
    let out_root = PathBuf::from(&out_dir);
    fs::create_dir_all(&out_root).map_err(|err| err.to_string())?;

    // ASR options are forced into "ASR only" shape. Mode 2 has its own
    // post-ASR pipeline (Phase 5) so the dialect-flavour polish must
    // not run on these short pieces.
    let mut asr_options = recognition_options.clone();
    asr_options.use_llm = Some(false);
    // Mode 2's own Whisper pool takes precedence over whatever the
    // caller passed in `recognition_options.whisper_endpoints` (which
    // is semantically the Mode 1 / global pool). Empty list = fall back
    // to whatever was passed.
    let semantic_whisper_pool = config.effective_semantic_whisper_endpoints();
    if !semantic_whisper_pool.is_empty() {
        eprintln!(
            "[semantic] using Mode 2's own Whisper pool: {} endpoint(s)",
            semantic_whisper_pool.len()
        );
        asr_options.whisper_endpoints = Some(semantic_whisper_pool);
    }

    let mut reports: Vec<SemanticPipelineFileReport> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    let total = input_paths.len();
    for (idx, input_path) in input_paths.iter().enumerate() {
        let _ = app.emit(
            "semantic:progress",
            &serde_json::json!({
                "source": input_path,
                "current": idx + 1,
                "total": total,
                "phase": "starting",
            }),
        );
        match process_single_semantic_file(
            &app,
            input_path,
            &out_root,
            &config,
            &asr_options,
        ) {
            Ok(report) => reports.push(report),
            Err(err) => {
                eprintln!("[semantic] {input_path}: {err}");
                errors.push(format!("{input_path}: {err}"));
            }
        }
    }

    let _ = app.emit(
        "semantic:progress",
        &serde_json::json!({"phase": "done", "ok": errors.is_empty()}),
    );
    Ok(SemanticPipelineResult { reports, errors })
}

// ---- process_single_semantic_file (was lib.rs:2413) — the sliding-window core ----

/// Process one source file with the sliding-window semantic cutter.
///
/// Per-iteration loop (per § user-proposed algorithm):
///   1. Take the next ≤90s slice starting at the cursor.
///   2. Send the slice to Whisper (one of the configured endpoints).
///   3. Hand the resulting `segments[]` (per-utterance timestamps) to
///      Qwen and ask: "where in this slice should we cut to end on a
///      complete utterance?"
///   4. Cut the source WAV at `cursor + cut_at` and emit one segment.
///   5. Advance cursor to the cut point and repeat until the source
///      is exhausted.
///
/// Why a sliding window vs the previous "pre-cut everything → big LLM
/// merge" pass:
///   - LLM context per iteration stays tiny (one window's Whisper
///     segments ≈ a few hundred tokens), so num_ctx pressure is gone.
///   - One iteration failing doesn't sink the whole task — we can
///     retry the same window or force a fixed 90s cut.
///   - The LLM only ever picks ONE number per call, so off-by-a-few-ms
///     errors that broke the candidate-pool validator can't happen
///     (we accept whatever ms the LLM returns within the window).
///   - Serial by nature: cursor depends on previous decision. Fine —
///     each window finishes in ~10s (Whisper 90s + Qwen one-shot),
///     so a 13min source runs in ~3 minutes regardless.
fn process_single_semantic_file(
    app: &tauri::AppHandle,
    input_path: &str,
    out_root: &Path,
    config: &CutConfig,
    asr_options: &RecognitionOptions,
) -> Result<SemanticPipelineFileReport, String> {
    use tauri::Emitter as _;

    let input = PathBuf::from(input_path);
    if !input.is_file() {
        return Err("input file not found".into());
    }
    let source_basename = path_file_name(&input);
    let source_stem = path_file_stem(&input);

    let source_dir = out_root.join("source");
    let segments_dir = out_root.join("segments");
    fs::create_dir_all(&source_dir).map_err(|err| err.to_string())?;
    fs::create_dir_all(&segments_dir).map_err(|err| err.to_string())?;
    let work_dir = out_root.join(".semantic-pipeline");
    fs::create_dir_all(&work_dir).map_err(|err| err.to_string())?;

    let probe = probe_audio(&input)?;
    let total_ms = probe.duration_ms.ok_or_else(|| {
        "ffprobe couldn't read source duration".to_string()
    })?;
    let codec = output_pcm_codec(&probe);

    // Copy source into the bundle so Windows annotators have it alongside
    // the segments.
    let source_dest = source_dir.join(&source_basename);
    fs::copy(&input, &source_dest).map_err(|err| err.to_string())?;

    // Resolve Whisper endpoints — Mode 2's own pool first, else the
    // recognition options' (Mode 1) pool.
    let whisper_endpoints: Vec<String> = config
        .effective_semantic_whisper_endpoints()
        .iter()
        .filter(|e| !e.url.trim().is_empty())
        .map(|e| e.url.clone())
        .chain(
            asr_options
                .whisper_endpoints
                .as_ref()
                .map(|v| {
                    v.iter()
                        .filter(|e| !e.url.trim().is_empty() && e.enabled)
                        .map(|e| e.url.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        )
        .collect();
    if whisper_endpoints.is_empty() {
        return Err("Mode 2 sliding cutter 需要至少一个 Whisper HTTP 端点".into());
    }

    // Same for Ollama — Mode 2 pool first, fall back to the Mode-1
    // pool from recognition options (Mode 1 ollama_extra_endpoints +
    // primary ollama_url). The sliding cutter's per-window prompts are
    // small enough that even num_ctx=2048 backends work, so any
    // reachable Ollama endpoint is acceptable.
    let mut ollama_pool: Vec<OllamaEndpointDef> = config
        .effective_semantic_ollama_endpoints()
        .into_iter()
        .filter(|e| !e.url.trim().is_empty() && e.enabled)
        .collect();
    if ollama_pool.is_empty() {
        if let Some(extras) = asr_options.ollama_extra_endpoints.as_ref() {
            for ep in extras {
                if !ep.url.trim().is_empty() && ep.enabled {
                    ollama_pool.push(OllamaEndpointDef {
                        url: ep.url.clone(),
                        model: ep.model.clone(),
                        enabled: true,
                    });
                }
            }
        }
        if ollama_pool.is_empty() {
            if let Some(url) = asr_options.ollama_url.as_ref() {
                if !url.trim().is_empty() {
                    ollama_pool.push(OllamaEndpointDef {
                        url: url.clone(),
                        model: asr_options.ollama_model.clone(),
                        enabled: true,
                    });
                }
            }
        }
        if !ollama_pool.is_empty() {
            eprintln!(
                "[semantic-sliding] Mode 2 LLM pool empty → promoted Mode 1 endpoints ({} url(s))",
                ollama_pool.len()
            );
        }
    }
    if ollama_pool.is_empty() {
        return Err(
            "Mode 2 sliding cutter 没有可用 Ollama 端点。请在 Mode 2 配置里加端点池，或者在 Mode 1 配置里加（会自动复用）。".into(),
        );
    }
    // Pick a default model: Mode 2 explicit > first endpoint's pinned
    // model > AppSettings/Mode-1 primary model > built-in fallback.
    let ollama_model = ollama_pool[0]
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .or_else(|| asr_options.ollama_model.clone().filter(|m| !m.trim().is_empty()))
        .unwrap_or_else(|| {
            if config.semantic_model.trim().is_empty() {
                "qwen2.5:32b".to_string()
            } else {
                config.semantic_model.clone()
            }
        });

    // Mode-2-private Whisper prompt — physically owned by
    // `modes::semantic`. See that module's `DEFAULT_WHISPER_INITIAL_PROMPT`
    // doc for why this exact wording (banning English/pinyin + Changsha
    // example sentences). worker_config / per-task overrides still win.
    let initial_prompt = asr_options
        .initial_prompt
        .clone()
        .unwrap_or_else(|| {
            crate::modes::semantic::DEFAULT_WHISPER_INITIAL_PROMPT.to_string()
        });

    // Sliding-window length. Defaults to `max_segment_s` (the same 90s
    // ceiling we'll cap segments at) so a fresh config "just works".
    // Tuning intent: bumping `semantic_window_s` higher than
    // `max_segment_s` lets the LLM see more lookahead context before
    // picking a cut, at the cost of larger per-window Whisper requests.
    let window_size_ms: u64 = config
        .semantic_window_s
        .filter(|w| *w >= 1.0)
        .map(|w| (w * 1000.0) as u64)
        .unwrap_or_else(|| (config.max_segment_s * 1000.0) as u64);
    let max_ms: u64 = (config.max_segment_s * 1000.0) as u64; // 90_000 default
    let min_ms: u64 = (config.min_segment_s * 1000.0) as u64; // 6_000 default

    let mut segments: Vec<SegmentRecord> = Vec::new();
    let mut cursor: u64 = 0;
    let mut window_idx: usize = 0;
    let mut whisper_rr: usize = 0; // round-robin index across Whisper pool

    while cursor < total_ms {
        window_idx += 1;
        let window_end_planned = (cursor + window_size_ms).min(total_ms);
        let window_ms = window_end_planned - cursor;

        let _ = app.emit(
            "semantic:progress",
            &serde_json::json!({
                "source": input_path,
                "phase": "window",
                "window_idx": window_idx,
                "cursor_ms": cursor,
                "window_ms": window_ms,
                "total_ms": total_ms,
            }),
        );

        // 1. Extract the window WAV to scratch.
        let window_path = work_dir.join(format!("win_{window_idx:04}.wav"));
        write_pcm_wav_segment(
            &input,
            &window_path,
            cursor as f64 / 1000.0,
            window_ms as f64 / 1000.0,
            &probe,
            codec,
        )?;

        // 2. Whisper ASR — round-robin across the pool. Retry on
        // transient failure (per-endpoint try, full pool on each
        // window's first call so a hot endpoint isn't punished
        // forever).
        let mut whisper_attempts = 0;
        let asr_result = loop {
            let url = &whisper_endpoints[whisper_rr % whisper_endpoints.len()];
            whisper_rr = whisper_rr.wrapping_add(1);
            match transcribe_remote_with_segments(url, &window_path, "zh", &initial_prompt) {
                Ok(r) => break r,
                Err(err) if whisper_attempts < whisper_endpoints.len() * 2 - 1 => {
                    eprintln!("[semantic-sliding] whisper {url} failed: {err} (retrying)");
                    whisper_attempts += 1;
                    std::thread::sleep(Duration::from_millis(
                        500 + (whisper_attempts as u64) * 500,
                    ));
                    continue;
                }
                Err(err) => return Err(format!("whisper 全部失败: {err}")),
            }
        };

        // 3. If this is the last window OR the audio is shorter than
        // max_ms total, emit the whole thing as one segment without
        // asking the LLM (no cut needed).
        // The LLM's cut must be ≤ max_ms (the spec cap), even when the
        // window itself is larger (e.g. user set semantic_window_s=120
        // for more lookahead but still wants ≤90s segments). Use
        // `cut_upper` as the actual decision ceiling.
        let cut_upper = window_ms.min(max_ms);
        let is_final_window = window_end_planned == total_ms && window_ms <= max_ms;
        let cut_at_in_window: u64 = if is_final_window {
            window_ms
        } else {
            // 4. Ask Qwen to pick a single cut point in [min_ms, cut_upper],
            // preferring late values for long segments.
            match llm_pick_one_cut(
                &asr_result,
                cut_upper,
                min_ms,
                &ollama_pool,
                &ollama_model,
                config.semantic_num_ctx,
                &config.semantic_cut_prompt,
            ) {
                Ok(cut) => cut.clamp(min_ms.min(cut_upper), cut_upper),
                Err(err) => {
                    eprintln!(
                        "[semantic-sliding] LLM cut decision failed (window {window_idx}): {err}; \
                         falling back to last whisper segment end"
                    );
                    // Fallback: cut at the end of the last Whisper
                    // segment within the cut window. Beats forcing a
                    // hard 90s cut mid-word.
                    asr_result
                        .segments
                        .iter()
                        .rev()
                        .find(|s| s.end_ms >= min_ms && s.end_ms <= cut_upper)
                        .map(|s| s.end_ms)
                        .unwrap_or(cut_upper)
                }
            }
        };

        // 5. Build segment text from the Whisper segments that fall
        // entirely before the cut point.
        let cut_text: String = asr_result
            .segments
            .iter()
            .filter(|s| s.end_ms <= cut_at_in_window + 50) // small tolerance
            .map(|s| s.text.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("");

        let seg_start_abs = cursor;
        let seg_end_abs = cursor + cut_at_in_window;

        // 6. Cut the source WAV at absolute timestamps.
        let seg_filename = format!(
            "{stem}_{seq:04}_{start}-{end}.wav",
            stem = source_stem,
            seq = segments.len() + 1,
            start = seg_start_abs,
            end = seg_end_abs,
        );
        let seg_dest = segments_dir.join(&seg_filename);
        write_pcm_wav_segment(
            &input,
            &seg_dest,
            seg_start_abs as f64 / 1000.0,
            (seg_end_abs - seg_start_abs) as f64 / 1000.0,
            &probe,
            codec,
        )?;

        let seg_id = seg_filename
            .strip_suffix(".wav")
            .unwrap_or(&seg_filename)
            .to_string();
        // QA hints — Mode-2-private detection. The annotator UI uses
        // these to highlight segments that need human review (non-
        // Chinese output, empty transcript, suspiciously short text).
        // Mode 1 doesn't run this; it has a separate dialect-aware QA
        // flow in the Tauri client.
        let seg_duration_ms = seg_end_abs - seg_start_abs;
        let qa_flags = crate::modes::semantic::detect_qa_flags(&cut_text, seg_duration_ms);
        segments.push(SegmentRecord {
            id: seg_id,
            source_path: format!("./source/{}", source_basename),
            source_file_name: source_basename.clone(),
            segment_path: format!("./segments/{}", seg_filename),
            segment_file_name: seg_filename,
            role: None,
            start_ms: seg_start_abs,
            end_ms: seg_end_abs,
            duration_ms: seg_duration_ms,
            original_text: cut_text.clone(),
            phonetic_text: cut_text,
            emotion: Vec::new(),
            tags: Vec::new(),
            notes: String::new(),
            qa_flags,
        });

        // 7. Drop the scratch window file (each is ~10MB at 48kHz mono).
        let _ = fs::remove_file(&window_path);

        // 8. Advance cursor. Safety: ensure we always make progress.
        let next_cursor = cursor + cut_at_in_window;
        if next_cursor <= cursor {
            // The LLM returned a cut at 0; force at least min_ms to
            // avoid an infinite loop.
            cursor += min_ms.max(1000);
            eprintln!(
                "[semantic-sliding] cut returned 0 progress; forcing cursor += {}ms",
                min_ms.max(1000)
            );
        } else {
            cursor = next_cursor;
        }
    }

    let _ = fs::remove_dir_all(&work_dir);

    let count = segments.len();
    Ok(SemanticPipelineFileReport {
        source_path: path_to_string(&input),
        segment_count: count,
        segments,
        source_copy_path: path_to_string(&source_dest),
    })
}

// ---- llm_pick_one_cut doc + DEFAULT_CUT_PROMPT use (was lib.rs:2767) ----

/// Ask Qwen for ONE cut point inside a single window. The prompt
/// includes the Whisper-segment timestamps so the model knows where
/// natural utterance boundaries are within this window. Returns the
/// cut time in ms (relative to the window's start).
///
/// Prompt is intentionally compact — Whisper segments rarely exceed
/// ~30 entries per 90s window, so the whole call is a few hundred
/// tokens and fits easily even in num_ctx=2048 backends. That removes
/// the original "must have 32K context" requirement.
/// Built-in default Mode-2 cut prompt. Tuned for Mandarin + Changsha
/// dialect (the project's primary corpus) with the rules from the
///录制数据剪辑转写规则（新）spec baked in. Users override via
/// `CutConfig.semantic_cut_prompt` for other dialects / languages.
///
/// Template placeholders — substituted at call time:
///   `{transcript}` `{window_ms}` `{min_ms}` `{max_s}` `{sweet_lo}` `{sweet_hi}`
///
/// Design notes for the actual prompt body:
///   - We DON'T tell the LLM the strict ms ceiling in the rules
///     section — it goes in the "硬性约束" preface so the model treats
///     it as non-negotiable rather than a soft preference.
///   - "Prefer 句末标点" emphasised over "prefer late time" — a clean
///     break at 45s beats a mid-clause break at 80s.
///   - "Don't cut in the middle of an utterance" is repeated because
///     Qwen sometimes drifts toward "split where there's punctuation
///     inside a Whisper segment" — bad, the punctuation is in the
///     text but the AUDIO segment boundary may be 200ms later.
///   - JSON format hint at the bottom — Qwen with format=json
///     respects it well.
///
/// Stage-1 refactor moved the prompt string into `modes/semantic.rs`
/// (Mode-2-private). This local re-export keeps the rest of lib.rs
/// untouched.
use crate::modes::semantic::DEFAULT_CUT_PROMPT as DEFAULT_SEMANTIC_CUT_PROMPT;


// ---- llm_pick_one_cut (was lib.rs:2803) — single-cut LLM call ----
fn llm_pick_one_cut(
    asr: &WhisperResult,
    window_ms: u64,
    min_ms: u64,
    pool: &[OllamaEndpointDef],
    default_model: &str,
    num_ctx: u32,
    cut_prompt_override: &str,
) -> Result<u64, String> {
    if asr.segments.is_empty() {
        // No utterances detected in this window — cut at window end so
        // we don't loop forever on silence.
        return Ok(window_ms);
    }

    // Build a compact timestamped transcript for the LLM. One line per
    // Whisper segment.
    let mut transcript = String::new();
    for (i, s) in asr.segments.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = writeln!(
            &mut transcript,
            "[{:>2}] {:>5}-{:>5}ms  {}",
            i + 1,
            s.start_ms,
            s.end_ms,
            s.text.trim(),
        );
    }

    let min_s = min_ms as f64 / 1000.0;
    let max_s = window_ms as f64 / 1000.0;
    let sweet_lo = (max_s * 0.65).max(min_s);
    let sweet_hi = (max_s * 0.95).max(sweet_lo);

    // Render the user-side prompt. User-provided template wins; fall
    // back to the canonical default. Placeholders are dumb string
    // replacements — missing ones stay literal, extras are tolerated.
    let template = if cut_prompt_override.trim().is_empty() {
        DEFAULT_SEMANTIC_CUT_PROMPT.to_string()
    } else {
        cut_prompt_override.to_string()
    };
    let user = template
        .replace("{transcript}", &transcript)
        .replace("{window_ms}", &window_ms.to_string())
        .replace("{min_ms}", &min_ms.to_string())
        .replace("{max_s}", &format!("{max_s:.1}"))
        .replace("{sweet_lo}", &format!("{sweet_lo:.0}"))
        .replace("{sweet_hi}", &format!("{sweet_hi:.0}"));
    // If the user's template didn't include `{transcript}`, append it
    // so the LLM always sees the data — avoids the "I rewrote my
    // prompt and forgot the placeholder" failure mode.
    let user = if user.contains(&transcript) {
        user
    } else {
        format!("{user}\n\n转写：\n{transcript}")
    };

    let system = "你是音频切割助手。你的任务是在一段录音的开头部分找到一个语义完整的切点，\
                  让切出的第一段话既完整、又尽量接近窗口上限。只输出 JSON，不要其他文字。"
        .to_string();

    let mut last_err: Option<String> = None;
    for endpoint in pool {
        if !endpoint.enabled {
            continue;
        }
        let model = endpoint
            .model
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| default_model.to_string());
        let req_body = serde_json::json!({
            "model": model,
            "stream": false,
            "messages": [
                {"role": "system", "content": &system},
                {"role": "user", "content": &user},
            ],
            "options": {
                "temperature": 0.1,
                "num_ctx": num_ctx.max(2048),
            },
            "format": "json",
        });
        let url = format!(
            "{}/api/chat",
            endpoint.url.trim_end_matches('/')
        );
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(180))
            .build();
        let resp = match agent.post(&url).send_json(req_body) {
            Ok(r) => r,
            Err(err) => {
                last_err = Some(format!("HTTP {url} failed: {err}"));
                continue;
            }
        };
        let raw_body = match resp.into_string() {
            Ok(s) => s,
            Err(err) => {
                last_err = Some(format!("read body failed: {err}"));
                continue;
            }
        };
        let v: Value = match serde_json::from_str(&raw_body) {
            Ok(v) => v,
            Err(err) => {
                last_err = Some(format!(
                    "Ollama 非 JSON 响应: {err} (body={})",
                    truncate_for_log(&raw_body, 200)
                ));
                continue;
            }
        };
        let content = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or_default();
        // Strip <think>...</think> wrappers + fenced JSON in case the
        // model decorates its output.
        let cleaned = strip_thinking(content);
        let trimmed = cleaned
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        match serde_json::from_str::<Value>(trimmed) {
            Ok(parsed) => {
                if let Some(cut) = parsed.get("cut_ms").and_then(|c| c.as_u64()) {
                    return Ok(cut);
                }
                last_err = Some(format!(
                    "JSON 缺 cut_ms 字段: {}",
                    truncate_for_log(trimmed, 200)
                ));
            }
            Err(err) => {
                last_err = Some(format!(
                    "解析切点 JSON 失败: {err} (content={})",
                    truncate_for_log(trimmed, 200)
                ));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "所有 Ollama 端点都未返回有效切点".into()))
}

// ---- transcribe_remote_with_segments (was lib.rs:5677) — Whisper /transcribe wrapper ----
fn transcribe_remote_with_segments(
    base_url: &str,
    segment_path: &Path,
    language: &str,
    initial_prompt: &str,
) -> Result<WhisperResult, String> {
    let bytes = fs::read(segment_path)
        .map_err(|err| format!("读取切片失败 {}: {}", segment_path.display(), err))?;
    let filename = segment_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("segment.wav");

    let boundary = format!("----dlbWhisper{}", uniq_token());
    let mut body: Vec<u8> = Vec::with_capacity(bytes.len() + 1024);
    let crlf = b"\r\n";

    let mut field_text = |name: &str, value: &str| {
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", name).as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(crlf);
    };
    field_text("language", language);
    field_text("initial_prompt", initial_prompt);

    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"audio\"; filename=\"{}\"\r\n",
            filename.replace('"', "_")
        )
        .as_bytes(),
    );
    body.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
    body.extend_from_slice(&bytes);
    body.extend_from_slice(crlf);
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

    let url = format!("{}/transcribe", base_url.trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(600))
        .build();
    let resp = agent
        .post(&url)
        .set(
            "Content-Type",
            &format!("multipart/form-data; boundary={}", boundary),
        )
        .send_bytes(&body)
        .map_err(|err| format!("whisper HTTP {}: {}", url, err))?;
    let raw = resp
        .into_string()
        .map_err(|err| format!("whisper 响应读取失败：{}", err))?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|err| format!("whisper 响应非法 JSON：{} (body={})", err, raw))?;
    let segments: Vec<WhisperSegment> = value
        .get("segments")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let start = s.get("start").and_then(|v| v.as_f64())?;
                    let end = s.get("end").and_then(|v| v.as_f64())?;
                    let text = s
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    Some(WhisperSegment {
                        start_ms: (start * 1000.0).max(0.0) as u64,
                        end_ms: (end * 1000.0).max(0.0) as u64,
                        text,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(WhisperResult { segments })
}

