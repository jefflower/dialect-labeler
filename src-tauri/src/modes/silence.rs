//! Mode 1 — silence-detection cut for dialect recording sessions.
//!
//! Owns:
//!   - `DEFAULT_WHISPER_INITIAL_PROMPT` — what Whisper is told to expect
//!     when the worker config doesn't override. Mode-1's pipeline runs
//!     Whisper over already-cut segments, then has a polish pass (Ollama
//!     dialect rewriter). The initial_prompt here just nudges Whisper
//!     toward Mandarin transcription — the dialect characters come from
//!     the polish step, not from Whisper.
//!   - `run()` — the worker pipeline for dialect tasks: scan → cut
//!     per source → recognize → bundle JSONL → zip.
//!   - `apply_recognition` — Mode-1-private helper that folds
//!     `recognize_segments_impl` results back into the segments slice.

/// Whisper `initial_prompt` for Mode 1. Conservative — Mode 1 is in
/// production and HANDOFF §6 calls out not changing its defaults
/// without a clear win. Empty/short is fine here because the polish
/// pass is the one that introduces dialect characters.
pub(crate) const DEFAULT_WHISPER_INITIAL_PROMPT: &str = "";

/// Zero-size marker.
pub(crate) struct SilenceMode;

impl SilenceMode {
    #[allow(dead_code)]
    pub(crate) const fn default_whisper_initial_prompt() -> &'static str {
        DEFAULT_WHISPER_INITIAL_PROMPT
    }
}

// ------------------------------------------------------------------
// Mode trait impl (worker-only).
// ------------------------------------------------------------------

use std::collections::HashMap;
use std::fs;

use serde_json::{json, Value};

use crate::cloud::{zip_dir, CloudError};
use crate::modes::{Mode, ModeRunCtx};
use crate::{
    cut_audio_file_impl, export_dataset_bundle_impl, recognize_segments_impl, ExportOptions,
    RecognitionResult, SegmentRecord,
};

impl Mode for SilenceMode {
    fn id(&self) -> &'static str {
        "dialect"
    }

    fn default_whisper_initial_prompt(&self) -> &'static str {
        DEFAULT_WHISPER_INITIAL_PROMPT
    }

    fn run(&self, ctx: ModeRunCtx<'_>) -> Result<(Value, u64), CloudError> {
        let ModeRunCtx {
            app,
            client,
            claim,
            worker_config,
            scan,
            extract_dir,
            bundle_dir,
            output_zip,
            cut_config,
            started,
            set_stage,
            ..
        } = ctx;

        set_stage("cutting");
        let _ = client.progress(
            &claim.id,
            25,
            "切割",
            Some(&format!("{} 个源文件", scan.audio_files.len())),
        );
        let total_sources = scan.audio_files.len().max(1);
        let mut all_segments: Vec<SegmentRecord> = Vec::new();
        for (idx, audio) in scan.audio_files.iter().enumerate() {
            let segs = cut_audio_file_impl(
                audio.path.clone(),
                scan.segments_dir.clone(),
                cut_config.clone(),
                audio.role.clone(),
                audio.topic_id,
                audio.matched_text.clone(),
                Some(audio.matched_emotion.clone()),
                audio.target_file_names.clone(),
            )
            .map_err(CloudError::Pipeline)?;
            all_segments.extend(segs);
            // 25 -> 45 across the cutting phase, scaled by source count.
            let pct = 25 + (20 * (idx + 1) / total_sources);
            let _ = client.progress(
                &claim.id,
                pct as u8,
                "切割",
                Some(&format!("{} / {}", idx + 1, total_sources)),
            );
        }

        if all_segments.is_empty() {
            return Err(CloudError::Pipeline(
                "切分后无可识别的片段（可能整段静音）".into(),
            ));
        }

        // ---- recognize ----
        set_stage("recognizing");
        let _ = client.progress(
            &claim.id,
            50,
            "识别",
            Some(&format!("{} 段", all_segments.len())),
        );
        let recognition = recognize_segments_impl(
            app.clone(),
            scan.project_dir.clone(),
            all_segments.clone(),
            worker_config.to_recognition_options(),
        )
        .map_err(CloudError::Pipeline)?;
        apply_recognition(&mut all_segments, &recognition);

        // ---- bundle + zip ----
        set_stage("bundling");
        let _ = client.progress(&claim.id, 85, "打包产物", None);
        fs::create_dir_all(bundle_dir)
            .map_err(|e| CloudError::Pipeline(format!("mkdir bundle: {e}")))?;
        // audio_file_prefix="./" rewrites every segment path from the
        // absolute worker temp dir to a path relative to the bundle
        // root. Without this the downloader can't replay the bundle on
        // their own machine — the absolute path only exists on the Mac
        // that did the processing.
        export_dataset_bundle_impl(
            bundle_dir.to_string_lossy().to_string(),
            all_segments.clone(),
            ExportOptions {
                system_prompt: None,
                pair_user_assistant: Some(true),
                use_source_audio_for_user: Some(false),
                audio_file_prefix: Some(".".to_string()),
                input_root: Some(extract_dir.to_string_lossy().to_string()),
            },
            true,
        )
        .map_err(CloudError::Pipeline)?;

        zip_dir(bundle_dir, output_zip).map_err(CloudError::Pipeline)?;
        let output_bytes = fs::metadata(output_zip).map(|m| m.len()).unwrap_or(0);

        // Build the summary the dispatcher keeps after files are
        // cleaned. Per-role duration aggregation is Mode-1 specific
        // (the dialect dataset cares about发音人 vs 陪聊人 balance);
        // semantic mode doesn't emit it.
        let mut duration_by_role: serde_json::Map<String, Value> = Default::default();
        for seg in &all_segments {
            let key = seg.role.clone().unwrap_or_else(|| "unknown".to_string());
            let entry = duration_by_role.entry(key).or_insert_with(|| json!(0u64));
            if let Some(current) = entry.as_u64() {
                *entry = json!(current + seg.duration_ms);
            }
        }
        let summary = json!({
            "mode": "dialect",
            "segment_count": all_segments.len(),
            "duration_by_role": duration_by_role,
            "source_files": scan.audio_files.len(),
            "asr_engine": worker_config
                .whisper_model
                .clone()
                .unwrap_or_else(|| "large-v3-turbo".to_string()),
            "llm_endpoints": worker_config.ollama_extra_endpoints.clone(),
            "elapsed_ms": started.elapsed().as_millis() as u64,
        });

        Ok((summary, output_bytes))
    }
}

/// Mode-1-private. Maps `recognize_segments_impl` results back onto the
/// segments slice. Lives here (not in cloud.rs) so Mode 2 can't
/// accidentally call it — its recognition path is different.
fn apply_recognition(segments: &mut [SegmentRecord], results: &[RecognitionResult]) {
    let by_id: HashMap<&str, &RecognitionResult> =
        results.iter().map(|r| (r.segment_id.as_str(), r)).collect();
    for seg in segments.iter_mut() {
        if let Some(r) = by_id.get(seg.id.as_str()) {
            seg.original_text = r.raw_text.clone();
            seg.phonetic_text = r.text.clone();
            if let Some(emotion) = r.emotion.clone() {
                if !emotion.trim().is_empty() {
                    seg.emotion = vec![emotion];
                }
            }
            if !r.tags.is_empty() {
                seg.tags = r.tags.clone();
            }
        }
    }
}
