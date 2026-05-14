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
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::{
    build_segment_file_name, cut_audio_file_impl, ensure_ffmpeg, export_dataset_bundle_impl,
    output_pcm_codec, path_file_name, path_file_stem, path_to_string, probe_audio,
    recognize_segments_impl, safe_name, seconds_to_ms, silent_command, write_pcm_wav_segment,
    CutConfig, CutValidationResult, ExportOptions, RecognitionResult, RepairCutSilenceResult,
    SegmentEdgeSilence, SegmentRecord,
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

// ============================================================
// Mode 1 cutting algorithm — moved out of lib.rs (2026-05-14)
// ============================================================

// ---- SilenceEvent enum (was lib.rs:681) ----
#[derive(Clone, Copy, Debug)]
pub(crate) enum SilenceEvent {
    Start(f64),
    End(f64),
}

// ---- validate_cut_segments_impl (was lib.rs:1367) ----
pub(crate) fn validate_cut_segments_impl(
    segments: Vec<SegmentRecord>,
    config: CutConfig,
) -> Vec<CutValidationResult> {
    segments
        .iter()
        .map(|segment| validate_cut_segment(segment, &config))
        .collect()
}

// ---- repair_cut_segment_silence_impl (was lib.rs:1391) ----
pub(crate) fn repair_cut_segment_silence_impl(
    segments: Vec<SegmentRecord>,
    config: CutConfig,
) -> Result<RepairCutSilenceResult, String> {
    let mut updated_segments = Vec::with_capacity(segments.len());
    for mut segment in segments {
        let path = Path::new(&segment.segment_path);
        let (head_trim_ms, new_duration_ms) = trim_segment_head_tail(
            path,
            config.silence_db,
            config.pre_roll_ms,
            config.post_roll_ms,
        )?;
        gate_segment_head_tail(path, config.silence_db)?;
        if new_duration_ms > 0 {
            segment.start_ms = segment.start_ms.saturating_add(head_trim_ms);
            segment.duration_ms = new_duration_ms;
            segment.end_ms = segment.start_ms.saturating_add(new_duration_ms);
        }
        updated_segments.push(segment);
    }
    let validation = validate_cut_segments_impl(updated_segments.clone(), config);
    Ok(RepairCutSilenceResult {
        processed_count: validation.len(),
        updated_segments,
        validation,
    })
}

// ---- validate_cut_segment (was lib.rs:1420) ----
pub(crate) fn validate_cut_segment(segment: &SegmentRecord, config: &CutConfig) -> CutValidationResult {
    const EDGE_TOLERANCE_MS: u64 = 1;
    let mut result = CutValidationResult {
        segment_id: segment.id.clone(),
        segment_path: segment.segment_path.clone(),
        segment_file_name: segment.segment_file_name.clone(),
        ok: false,
        leading_silence_ms: 0,
        trailing_silence_ms: 0,
        max_leading_silence_ms: config.pre_roll_ms,
        max_trailing_silence_ms: config.post_roll_ms,
        threshold_db: config.silence_db,
        all_silence: false,
        message: None,
    };

    match analyze_segment_edge_silence(Path::new(&segment.segment_path), config.silence_db) {
        Ok(edges) => {
            result.leading_silence_ms = edges.leading_ms;
            result.trailing_silence_ms = edges.trailing_ms;
            result.all_silence = edges.all_silence;
            let leading_ok = edges.leading_ms <= config.pre_roll_ms + EDGE_TOLERANCE_MS;
            let trailing_ok = edges.trailing_ms <= config.post_roll_ms + EDGE_TOLERANCE_MS;
            let duration_ok = edges.duration_ms >= config.min_segment_ms;
            result.ok = !edges.all_silence && leading_ok && trailing_ok && duration_ok;
            if !result.ok {
                let mut parts = Vec::new();
                if edges.all_silence {
                    parts.push("整段低于静音阈值".to_string());
                }
                if !leading_ok {
                    parts.push(format!(
                        "前留空 {}ms > {}ms",
                        edges.leading_ms, config.pre_roll_ms
                    ));
                }
                if !trailing_ok {
                    parts.push(format!(
                        "后留空 {}ms > {}ms",
                        edges.trailing_ms, config.post_roll_ms
                    ));
                }
                if !duration_ok {
                    parts.push(format!(
                        "时长 {}ms < 最短语音 {}ms",
                        edges.duration_ms, config.min_segment_ms
                    ));
                }
                result.message = Some(parts.join("；"));
            }
        }
        Err(err) => {
            result.message = Some(err);
        }
    }
    result
}

// ---- cut_audio_file_dialect_impl (was lib.rs:3331) — Mode 1 entry point ----
pub(crate) fn cut_audio_file_dialect_impl(
    input_path: String,
    segments_dir: String,
    config: CutConfig,
    role: Option<String>,
    topic_id: Option<u32>,
    original_text: Option<String>,
    emotion: Option<Vec<String>>,
    target_file_names: Vec<String>,
) -> Result<Vec<SegmentRecord>, String> {
    ensure_ffmpeg()?;

    let input = PathBuf::from(&input_path);
    if !input.is_file() {
        return Err(format!("Input audio not found: {}", input_path));
    }

    let segments_root = PathBuf::from(&segments_dir);
    fs::create_dir_all(&segments_root).map_err(|err| err.to_string())?;

    let probe = probe_audio(&input)?;
    let duration_ms = probe
        .duration_ms
        .ok_or_else(|| "Unable to read audio duration with ffprobe".to_string())?;
    let duration_sec = duration_ms as f64 / 1000.0;

    // Calibrate the silence threshold against the recording's own noise
    // floor. A fixed dB cutoff (e.g. -30) fails in two opposite ways: it
    // misses ambient hiss louder than the cutoff (long silence baked into
    // the start of a segment) and it overshoots, catching mid-utterance
    // dips quieter than the cutoff (sentence chopped in half). The
    // effective threshold below is `max(user_db, noise_floor + 8dB)`,
    // capped at -15 dB, so it tracks the recording instead of fighting it.
    let noise_floor_db = analyze_noise_floor_db(&input);
    let mut effective_config = config.clone();
    effective_config.silence_db = effective_silence_db(config.silence_db, noise_floor_db);
    if let Some(floor) = noise_floor_db {
        eprintln!(
            "[cut_audio_file] noise_floor={:.1}dB user_silence_db={:.1}dB effective_silence_db={:.1}dB",
            floor, config.silence_db, effective_config.silence_db
        );
    }

    let events = detect_silence(&input, &effective_config)?;
    let ranges = build_segment_ranges(duration_sec, &events, &effective_config);
    let codec = output_pcm_codec(&probe);
    let mut segments = Vec::new();
    let text = original_text.clone().unwrap_or_default();
    let text_chunks = split_text_by_ranges(&text, &ranges);

    for (index, (start_sec, end_sec)) in ranges.iter().enumerate() {
        let raw_start_ms = seconds_to_ms(*start_sec);
        let raw_end_ms = seconds_to_ms(*end_sec);
        let segment_file_name = build_segment_file_name(
            &input,
            role.as_deref(),
            topic_id,
            &target_file_names,
            index,
            raw_start_ms,
            raw_end_ms,
        );
        let output = segments_root.join(&segment_file_name);
        write_pcm_wav_segment(
            &input,
            &output,
            *start_sec,
            end_sec - start_sec,
            &probe,
            codec,
        )?;
        // Physically trim residual noise floor at the head/tail of the
        // segment. This is a backstop for cases where silencedetect
        // missed a long ambient region (most often the leading 5-10 s
        // of a recording) — the cut would otherwise carry that silence
        // forward into the segment file. Threshold matches the
        // recording-calibrated value used for silencedetect so the same
        // notion of "silence" applies on both passes.
        let (head_trim_ms, trimmed_duration_ms) = trim_segment_head_tail(
            &output,
            effective_config.silence_db as f32,
            effective_config.pre_roll_ms,
            effective_config.post_roll_ms,
        )
        .unwrap_or((0, raw_end_ms.saturating_sub(raw_start_ms)));
        // After trimming down to the allowed breathing room, make the
        // remaining non-speech head/tail bit-true silence. This keeps the
        // 100/200ms padding budget without leaving room tone in it.
        let _ = gate_segment_head_tail(&output, effective_config.silence_db as f32);

        // Shift segment metadata to match the trimmed file. start_ms /
        // end_ms are positions in the SOURCE audio; trimming X ms off
        // the front of the segment means the segment now starts X ms
        // later in source time, and ends start_ms + trimmed_duration.
        let start_ms = raw_start_ms.saturating_add(head_trim_ms);
        let duration_ms = if trimmed_duration_ms > 0 {
            trimmed_duration_ms
        } else {
            raw_end_ms.saturating_sub(raw_start_ms)
        };
        let end_ms = start_ms.saturating_add(duration_ms);

        let segment_text = text_chunks.get(index).cloned().unwrap_or_default();
        segments.push(SegmentRecord {
            id: path_file_stem(Path::new(&segment_file_name)),
            source_path: path_to_string(&input),
            source_file_name: path_file_name(&input),
            segment_path: path_to_string(&output),
            segment_file_name,
            role: role.clone(),
            start_ms,
            end_ms,
            duration_ms,
            original_text: segment_text.clone(),
            phonetic_text: segment_text,
            emotion: emotion.clone().unwrap_or_default(),
            tags: Vec::new(),
            notes: String::new(),
            qa_flags: Vec::new(),
        });
    }

    Ok(segments)
}

// ---- analyze_noise_floor_db (was lib.rs:6762) ----
pub(crate) fn analyze_noise_floor_db(input: &Path) -> Option<f32> {
    // Decode to mono i16 at 8 kHz — same cheap pipeline as the waveform
    // peaks reader. Noise floor estimation does not need fidelity.
    let mut child = silent_command("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(input)
        .arg("-vn")
        .arg("-ac")
        .arg("1")
        .arg("-ar")
        .arg("8000")
        .arg("-f")
        .arg("s16le")
        .arg("-acodec")
        .arg("pcm_s16le")
        .arg("-")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;

    let stdout = child.stdout.take()?;
    let mut reader = BufReader::new(stdout);
    let mut buf = [0u8; 8192];
    const FRAME_SAMPLES: usize = 400; // 50 ms at 8 kHz
    let mut sum_sq: f64 = 0.0;
    let mut count: usize = 0;
    let mut frame_dbs: Vec<f32> = Vec::new();

    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        let chunks = n / 2;
        for index in 0..chunks {
            let lo = buf[index * 2];
            let hi = buf[index * 2 + 1];
            let s = i16::from_le_bytes([lo, hi]) as f64;
            sum_sq += s * s;
            count += 1;
            if count >= FRAME_SAMPLES {
                let rms = (sum_sq / count as f64).sqrt();
                let normalized = (rms / i16::MAX as f64).max(1e-9);
                let db = 20.0 * normalized.log10();
                frame_dbs.push(db as f32);
                sum_sq = 0.0;
                count = 0;
            }
        }
    }
    let _ = child.wait();

    if frame_dbs.is_empty() {
        return None;
    }
    frame_dbs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // 10th percentile — robust to the occasional very-quiet outlier frame
    // that a 0th-percentile (min) would latch onto.
    let idx = ((frame_dbs.len() as f64) * 0.10).round() as usize;
    let idx = idx.min(frame_dbs.len() - 1);
    Some(frame_dbs[idx])
}

// ---- effective_silence_db (was lib.rs:6844) ----
pub(crate) fn effective_silence_db(user_db: f32, noise_floor_db: Option<f32>) -> f32 {
    const HEADROOM_DB: f32 = 8.0;
    const MAX_THRESHOLD_DB: f32 = -15.0;
    let auto = noise_floor_db
        .map(|floor| floor + HEADROOM_DB)
        .unwrap_or(user_db);
    auto.max(user_db).min(MAX_THRESHOLD_DB)
}

// ---- detect_silence (was lib.rs:6853) ----
pub(crate) fn detect_silence(path: &Path, config: &CutConfig) -> Result<Vec<SilenceEvent>, String> {
    let noise = format!("{}dB", config.silence_db);
    let duration = format!("{:.3}", config.min_silence_ms as f64 / 1000.0);
    let filter = format!("silencedetect=noise={}:d={}", noise, duration);

    let output = silent_command("ffmpeg")
        .arg("-hide_banner")
        .arg("-nostats")
        .arg("-i")
        .arg(path)
        .arg("-af")
        .arg(filter)
        .arg("-f")
        .arg("null")
        .arg("-")
        .output()
        .map_err(|err| format!("Unable to run ffmpeg: {}", err))?;

    let logs = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(logs.to_string());
    }

    let start_re = Regex::new(r"silence_start:\s*([0-9.]+)").expect("valid silence regex");
    let end_re = Regex::new(r"silence_end:\s*([0-9.]+)").expect("valid silence regex");
    let mut events = Vec::new();

    for line in logs.lines() {
        if let Some(caps) = start_re.captures(line) {
            if let Some(value) = caps
                .get(1)
                .and_then(|item| item.as_str().parse::<f64>().ok())
            {
                events.push(SilenceEvent::Start(value));
            }
        }
        if let Some(caps) = end_re.captures(line) {
            if let Some(value) = caps
                .get(1)
                .and_then(|item| item.as_str().parse::<f64>().ok())
            {
                events.push(SilenceEvent::End(value));
            }
        }
    }

    Ok(events)
}

// ---- build_segment_ranges (was lib.rs:6902) ----
pub(crate) fn build_segment_ranges(
    duration_sec: f64,
    events: &[SilenceEvent],
    config: &CutConfig,
) -> Vec<(f64, f64)> {
    let mut ranges = Vec::new();
    let mut cursor = 0.0;
    let min_segment = config.min_segment_ms as f64 / 1000.0;
    let max_segment = if config.max_segment_ms > 0 {
        config.max_segment_ms as f64 / 1000.0
    } else {
        f64::INFINITY
    };
    let pre_roll = config.pre_roll_ms as f64 / 1000.0;
    let post_roll = config.post_roll_ms as f64 / 1000.0;

    // Three booleans / one buffer drive the merge semantics:
    //
    //   `skipping_silence` — last Start was rejected (would produce a
    //     sub-min piece), so the matching End must NOT advance the cursor;
    //     the silence "vanishes" and audio continues into the next phrase.
    //
    //   `in_trailing_silence` — last Start was accepted (range pushed),
    //     so cursor logically sits at the silence boundary. If the audio
    //     ends before an End arrives (file ends in silence), the final-
    //     tail block below must NOT re-push the same range.
    //
    //   `skipped_silences` — positions of silence Starts that were too
    //     short to honour. Used by the force-split path: when the
    //     accumulated voice exceeds `max_segment`, we prefer cutting at
    //     one of these "real (but short) pauses" over an arbitrary equal
    //     split, so the cut feels more natural to the listener.
    let mut in_trailing_silence = false;
    // `skipped_silences` stores (silence_start, silence_end) pairs for
    // every silence that was too short to act as a stand-alone split
    // point. The force-split logic prefers cutting at one of these natural
    // pauses (using silence_end as the next sub-segment's start, so the
    // silence itself stays out of both segments) over equal-split.
    let mut skipped_silences: Vec<(f64, f64)> = Vec::new();
    let mut pending_skip_start: Option<f64> = None;

    for event in events {
        match event {
            SilenceEvent::Start(start) => {
                let seg_duration = *start - cursor;
                // Head silence (or back-to-back silence events at the same
                // boundary): no voice between cursor and this silence_start.
                // The "skip and merge forward" path is wrong here — there's
                // nothing to merge. Instead let the matching End advance the
                // cursor through this silence so leading silence gets trimmed.
                if seg_duration <= 0.0 {
                    pending_skip_start = None;
                    in_trailing_silence = true;
                    skipped_silences.clear();
                    continue;
                }
                if seg_duration < min_segment {
                    pending_skip_start = Some(*start);
                    continue;
                }
                push_with_smart_split(
                    &mut ranges,
                    cursor,
                    *start,
                    duration_sec,
                    max_segment,
                    &skipped_silences,
                    pre_roll,
                    post_roll,
                );
                in_trailing_silence = true;
                skipped_silences.clear();
            }
            SilenceEvent::End(end) => {
                if let Some(s) = pending_skip_start.take() {
                    skipped_silences.push((s, *end));
                    continue;
                }
                cursor = (*end).min(duration_sec).max(0.0);
                in_trailing_silence = false;
            }
        }
    }

    if !in_trailing_silence {
        let tail = duration_sec - cursor;
        if tail > 0.0 {
            // Always push the tail as its own segment — even when shorter
            // than min_segment. Gluing onto the previous segment would
            // re-introduce the long mid-segment silence (the user reported
            // a 1s interior silence from exactly that bug). A short
            // trailing segment is cleaner; user can drop it manually.
            push_with_smart_split(
                &mut ranges,
                cursor,
                duration_sec,
                duration_sec,
                max_segment,
                &skipped_silences,
                pre_roll,
                post_roll,
            );
        }
    }

    // "整段都小于最短时长" escape hatch: if nothing was pushed (audio so
    // short that even the always-push branch produced zero ranges, e.g.
    // empty duration), emit the whole file as one range.
    if ranges.is_empty() && duration_sec > 0.0 {
        ranges.push((0.0, duration_sec));
    }

    ranges
}

/// Push a voice range, enforcing `max_segment` as a soft cap.
///
/// Within an over-long range we prefer cutting at a *real* (but too-short
/// for stand-alone splitting) pause carried in `skipped_silences` — that
/// gives the listener a natural break rather than an arbitrary 30s mark.
/// Selection rule: latest silence whose start sits within
/// `[range_start, range_start + max_segment]`. That maximises the first
/// half while still respecting the cap. Only when no candidate exists in
/// that window does the equal-N fallback run.
///
/// Each silence stays out of BOTH halves: first half ends at silence_start
/// (last spoken word), second half starts at silence_end (next word). All

// ---- push_with_smart_split (was lib.rs:7030) ----
pub(crate) fn push_with_smart_split(
    ranges: &mut Vec<(f64, f64)>,
    start: f64,
    end: f64,
    duration_sec: f64,
    max_segment: f64,
    skipped_silences: &[(f64, f64)],
    pre_roll: f64,
    post_roll: f64,
) {
    let dur = end - start;
    if dur <= 0.0 {
        return;
    }
    if !max_segment.is_finite() || dur <= max_segment {
        push_padded_range(ranges, start, end, duration_sec, 0.0, pre_roll, post_roll);
        return;
    }

    // Walk candidates latest → earliest. Pick the first one whose
    // silence_start fits in [start, start + max_segment]. Each silence is
    // also constrained to fall inside the current voice range (start, end).
    let smart = skipped_silences
        .iter()
        .rev()
        .copied()
        .find(|&(s_silence, _)| {
            s_silence > start && s_silence < end && (s_silence - start) <= max_segment
        });

    if let Some((s_silence, e_silence)) = smart {
        // First half: voice up to the pause start. Already ≤ max by the
        // window check, so this recursive call won't split again.
        push_with_smart_split(
            ranges,
            start,
            s_silence,
            duration_sec,
            max_segment,
            skipped_silences,
            pre_roll,
            post_roll,
        );
        // Second half: voice from where the pause ends, to the original
        // end. May still exceed max if the original voice was very long
        // with one early pause; recursion handles that.
        push_with_smart_split(
            ranges,
            e_silence,
            end,
            duration_sec,
            max_segment,
            skipped_silences,
            pre_roll,
            post_roll,
        );
        return;
    }

    // No usable skipped silence → equal-N split (existing behaviour).
    let parts = (dur / max_segment).ceil() as usize;
    let part_duration = dur / parts as f64;
    for i in 0..parts {
        let s = start + i as f64 * part_duration;
        let e = if i + 1 == parts {
            end
        } else {
            start + (i + 1) as f64 * part_duration
        };
        push_padded_range(ranges, s, e, duration_sec, 0.0, pre_roll, post_roll);
    }
}

// ---- split_text_by_ranges (was lib.rs:7103) ----
pub(crate) fn split_text_by_ranges(text: &str, ranges: &[(f64, f64)]) -> Vec<String> {
    if ranges.is_empty() {
        return Vec::new();
    }

    let chars: Vec<char> = text.trim().chars().collect();
    if chars.is_empty() {
        return vec![String::new(); ranges.len()];
    }

    let total_duration: f64 = ranges
        .iter()
        .map(|(start, end)| (end - start).max(0.0))
        .sum();
    if total_duration <= 0.0 {
        return vec![text.trim().to_string(); ranges.len()];
    }

    let mut chunks = Vec::with_capacity(ranges.len());
    let mut cursor = 0usize;
    let mut cumulative = 0.0;

    for (index, (start, end)) in ranges.iter().enumerate() {
        if index == ranges.len() - 1 {
            chunks.push(
                chars[cursor..]
                    .iter()
                    .collect::<String>()
                    .trim()
                    .to_string(),
            );
            break;
        }

        cumulative += (end - start).max(0.0) / total_duration * chars.len() as f64;
        let mut next = cumulative.round() as usize;
        next = next.clamp(cursor, chars.len());
        chunks.push(
            chars[cursor..next]
                .iter()
                .collect::<String>()
                .trim()
                .to_string(),
        );
        cursor = next;
    }

    chunks
}

// ---- push_padded_range (was lib.rs:7153) ----
pub(crate) fn push_padded_range(
    ranges: &mut Vec<(f64, f64)>,
    start: f64,
    end: f64,
    duration_sec: f64,
    min_segment: f64,
    pre_roll: f64,
    post_roll: f64,
) {
    if end <= start || end - start < min_segment {
        return;
    }

    let padded_start = (start - pre_roll).max(0.0);
    let padded_end = (end + post_roll).min(duration_sec);
    if padded_end > padded_start {
        ranges.push((padded_start, padded_end));
    }
}

// ---- trim_segment_head_tail (was lib.rs:7198) ----
pub(crate) fn trim_segment_head_tail(
    path: &Path,
    threshold_db: f32,
    pre_roll_ms: u64,
    post_roll_ms: u64,
) -> Result<(u64, u64), String> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(_) => return Ok((0, 0)),
    };
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Ok((0, 0));
    }

    // Walk RIFF chunks to locate `fmt ` (sample format) and `data` (PCM).
    let mut bps = 0u16;
    let mut channels = 0u16;
    let mut sample_rate = 0u32;
    let mut data_start = 0usize;
    let mut data_len = 0usize;
    let mut i = 12usize;
    while i + 8 <= bytes.len() {
        let chunk_id = [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]];
        let chunk_size =
            u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        let chunk_data = i + 8;
        match &chunk_id {
            b"fmt " => {
                if chunk_data + 16 <= bytes.len() {
                    channels = u16::from_le_bytes([bytes[chunk_data + 2], bytes[chunk_data + 3]]);
                    sample_rate = u32::from_le_bytes([
                        bytes[chunk_data + 4],
                        bytes[chunk_data + 5],
                        bytes[chunk_data + 6],
                        bytes[chunk_data + 7],
                    ]);
                    bps = u16::from_le_bytes([bytes[chunk_data + 14], bytes[chunk_data + 15]]);
                }
            }
            b"data" => {
                data_start = chunk_data;
                data_len = chunk_size;
                break;
            }
            _ => {}
        }
        i = chunk_data + chunk_size + (chunk_size & 1);
    }

    if data_start == 0
        || data_len == 0
        || channels == 0
        || sample_rate == 0
        || !matches!(bps, 16 | 24)
    {
        return Ok((0, 0));
    }
    let bytes_per_channel_sample = bps as usize / 8;
    let frame_size = bytes_per_channel_sample * channels as usize;
    if frame_size == 0 {
        return Ok((0, 0));
    }
    let data_end = (data_start + data_len).min(bytes.len());
    let usable = (data_end - data_start) / frame_size * frame_size;
    if usable == 0 {
        return Ok((0, 0));
    }
    let frame_count = usable / frame_size;
    let total_duration_ms = ((frame_count as u64).saturating_mul(1000)) / sample_rate as u64;

    let max_amp: f64 = match bps {
        16 => i16::MAX as f64,
        24 => 0x7F_FFFF as f64,
        _ => return Ok((0, total_duration_ms)),
    };
    let amp_threshold = 10.0_f64.powf(threshold_db as f64 / 20.0) * max_amp;

    // Read one frame's max-channel sample magnitude as f64.
    let frame_peak = |frame_idx: usize| -> f64 {
        let mut peak = 0.0_f64;
        let off = data_start + frame_idx * frame_size;
        for ch in 0..channels as usize {
            let p = off + ch * bytes_per_channel_sample;
            let val: f64 = match bps {
                16 => i16::from_le_bytes([bytes[p], bytes[p + 1]]) as f64,
                24 => {
                    let raw = (bytes[p] as i32)
                        | ((bytes[p + 1] as i32) << 8)
                        | ((bytes[p + 2] as i32) << 16);
                    let signed = if raw & 0x80_0000 != 0 {
                        raw | !0xFF_FFFF
                    } else {
                        raw
                    };
                    signed as f64
                }
                _ => 0.0,
            };
            let abs = val.abs();
            if abs > peak {
                peak = abs;
            }
        }
        peak
    };

    // 20 ms RMS window — short enough not to swallow phoneme transitions,
    // long enough to ignore single-sample clicks at the boundary.
    let window_frames = ((sample_rate as u64 * 20) / 1000).max(1) as usize;
    let window_rms = |start_frame: usize| -> f64 {
        let end = (start_frame + window_frames).min(frame_count);
        if end <= start_frame {
            return 0.0;
        }
        let mut sum_sq = 0.0_f64;
        let mut n = 0u64;
        for f in start_frame..end {
            let p = frame_peak(f);
            sum_sq += p * p;
            n += 1;
        }
        if n == 0 {
            0.0
        } else {
            (sum_sq / n as f64).sqrt()
        }
    };

    // Head scan — first window above threshold.
    let mut head_frame = 0usize;
    while head_frame < frame_count {
        if window_rms(head_frame) > amp_threshold {
            break;
        }
        head_frame += window_frames;
    }
    // Tail scan — last window above threshold (frame index just past it).
    let mut tail_frame = frame_count;
    while tail_frame > head_frame {
        let start = tail_frame.saturating_sub(window_frames);
        if window_rms(start) > amp_threshold {
            break;
        }
        tail_frame = start;
        if tail_frame == 0 {
            break;
        }
    }

    // If nothing crossed the threshold the whole segment looks like
    // silence. Bail rather than produce a zero-length file.
    if tail_frame <= head_frame {
        return Ok((0, total_duration_ms));
    }

    // Apply pre/post-roll padding in frames, clamped to [0, frame_count].
    let pre_frames = ((pre_roll_ms * sample_rate as u64) / 1000) as usize;
    let post_frames = ((post_roll_ms * sample_rate as u64) / 1000) as usize;
    let head_frame = head_frame.saturating_sub(pre_frames);
    let tail_frame = (tail_frame + post_frames).min(frame_count);

    if head_frame == 0 && tail_frame == frame_count {
        return Ok((0, total_duration_ms));
    }

    let head_bytes = head_frame * frame_size;
    let tail_bytes_end = tail_frame * frame_size;
    let new_data_len = tail_bytes_end - head_bytes;

    // Rewrite the file: header bytes up to `data_start`, with the data
    // chunk size patched, then the trimmed PCM slice. The RIFF outer
    // size at offset 4 is also patched to `total - 8`. This preserves
    // any pre-data chunks (e.g. `fmt `, `LIST`/`INFO`) that ffmpeg may
    // have emitted.
    let mut out = Vec::with_capacity(data_start + new_data_len);
    out.extend_from_slice(&bytes[..data_start]);
    if data_start >= 4 {
        let size_pos = data_start - 4;
        out[size_pos..size_pos + 4].copy_from_slice(&(new_data_len as u32).to_le_bytes());
    }
    out.extend_from_slice(&bytes[data_start + head_bytes..data_start + head_bytes + new_data_len]);
    let riff_size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&riff_size.to_le_bytes());

    fs::write(path, &out).map_err(|err| err.to_string())?;

    let head_trim_ms = (head_frame as u64).saturating_mul(1000) / sample_rate as u64;
    let new_duration_ms =
        ((tail_frame - head_frame) as u64).saturating_mul(1000) / sample_rate as u64;
    Ok((head_trim_ms, new_duration_ms))
}

// ---- gate_segment_head_tail (was lib.rs:7395) ----
pub(crate) fn gate_segment_head_tail(path: &Path, threshold_db: f32) -> Result<(), String> {
    let mut bytes = match fs::read(path) {
        Ok(b) => b,
        Err(_) => return Ok(()),
    };
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Ok(());
    }

    let mut bps = 0u16;
    let mut channels = 0u16;
    let mut data_start = 0usize;
    let mut data_len = 0usize;
    let mut i = 12usize;
    while i + 8 <= bytes.len() {
        let chunk_id = [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]];
        let chunk_size =
            u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        let chunk_data = i + 8;
        match &chunk_id {
            b"fmt " => {
                if chunk_data + 16 <= bytes.len() {
                    channels = u16::from_le_bytes([bytes[chunk_data + 2], bytes[chunk_data + 3]]);
                    bps = u16::from_le_bytes([bytes[chunk_data + 14], bytes[chunk_data + 15]]);
                }
            }
            b"data" => {
                data_start = chunk_data;
                data_len = chunk_size;
                break;
            }
            _ => {}
        }
        i = chunk_data + chunk_size + (chunk_size & 1);
    }

    if data_start == 0 || data_len == 0 || channels == 0 || !matches!(bps, 16 | 24) {
        return Ok(());
    }
    let bytes_per_channel_sample = bps as usize / 8;
    let frame_size = bytes_per_channel_sample * channels as usize;
    if frame_size == 0 {
        return Ok(());
    }
    let data_end = (data_start + data_len).min(bytes.len());
    let usable = (data_end - data_start) / frame_size * frame_size;
    if usable == 0 {
        return Ok(());
    }
    let frame_count = usable / frame_size;

    let max_amp: i32 = match bps {
        16 => i16::MAX as i32,
        24 => 0x7F_FFFF,
        _ => return Ok(()),
    };
    let amp_threshold = (10.0_f32.powf(threshold_db / 20.0) * max_amp as f32) as i32;

    let read_peak = |bytes: &[u8], frame_off: usize| -> i32 {
        let mut peak = 0i32;
        for ch in 0..channels as usize {
            let p = frame_off + ch * bytes_per_channel_sample;
            let val = match bps {
                16 => i16::from_le_bytes([bytes[p], bytes[p + 1]]) as i32,
                24 => {
                    let raw = (bytes[p] as i32)
                        | ((bytes[p + 1] as i32) << 8)
                        | ((bytes[p + 2] as i32) << 16);
                    if raw & 0x80_0000 != 0 {
                        raw | !0xFF_FFFF
                    } else {
                        raw
                    }
                }
                _ => 0,
            };
            peak = peak.max(val.abs());
        }
        peak
    };

    let mut head = 0usize;
    while head < frame_count {
        let off = data_start + head * frame_size;
        if read_peak(&bytes, off) > amp_threshold {
            break;
        }
        head += 1;
    }

    let mut tail = frame_count;
    while tail > head {
        let off = data_start + (tail - 1) * frame_size;
        if read_peak(&bytes, off) > amp_threshold {
            break;
        }
        tail -= 1;
    }

    if head == 0 && tail == frame_count {
        return Ok(());
    }

    let head_bytes = head * frame_size;
    if head_bytes > 0 {
        for b in &mut bytes[data_start..data_start + head_bytes] {
            *b = 0;
        }
    }

    let tail_start = data_start + tail * frame_size;
    let tail_end = data_start + frame_count * frame_size;
    if tail_end > tail_start {
        for b in &mut bytes[tail_start..tail_end] {
            *b = 0;
        }
    }

    fs::write(path, bytes).map_err(|err| err.to_string())
}

// ---- analyze_segment_edge_silence (was lib.rs:7516) ----
pub(crate) fn analyze_segment_edge_silence(
    path: &Path,
    threshold_db: f32,
) -> Result<SegmentEdgeSilence, String> {
    let bytes = fs::read(path).map_err(|err| format!("读取音频失败：{err}"))?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("不是可检测的 WAV 文件".to_string());
    }

    let mut bps = 0u16;
    let mut channels = 0u16;
    let mut sample_rate = 0u32;
    let mut data_start = 0usize;
    let mut data_len = 0usize;
    let mut i = 12usize;
    while i + 8 <= bytes.len() {
        let chunk_id = [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]];
        let chunk_size =
            u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        let chunk_data = i + 8;
        match &chunk_id {
            b"fmt " => {
                if chunk_data + 16 <= bytes.len() {
                    channels = u16::from_le_bytes([bytes[chunk_data + 2], bytes[chunk_data + 3]]);
                    sample_rate = u32::from_le_bytes([
                        bytes[chunk_data + 4],
                        bytes[chunk_data + 5],
                        bytes[chunk_data + 6],
                        bytes[chunk_data + 7],
                    ]);
                    bps = u16::from_le_bytes([bytes[chunk_data + 14], bytes[chunk_data + 15]]);
                }
            }
            b"data" => {
                data_start = chunk_data;
                data_len = chunk_size;
                break;
            }
            _ => {}
        }
        i = chunk_data + chunk_size + (chunk_size & 1);
    }

    if data_start == 0 || data_len == 0 || channels == 0 || sample_rate == 0 {
        return Err("WAV 缺少可检测的 PCM 数据".to_string());
    }
    if !matches!(bps, 16 | 24) {
        return Err(format!("暂不支持 {}-bit WAV 检测", bps));
    }

    let bytes_per_channel_sample = bps as usize / 8;
    let frame_size = bytes_per_channel_sample * channels as usize;
    if frame_size == 0 {
        return Err("WAV 帧大小异常".to_string());
    }
    let data_end = (data_start + data_len).min(bytes.len());
    let usable = (data_end - data_start) / frame_size * frame_size;
    if usable == 0 {
        return Err("WAV 数据为空".to_string());
    }

    let frame_count = usable / frame_size;
    let total_ms = ((frame_count as f64 * 1000.0) / sample_rate as f64).round() as u64;
    let max_amp = match bps {
        16 => i16::MAX as f64,
        24 => 0x7F_FFFF as f64,
        _ => unreachable!(),
    };
    let amp_threshold = 10.0_f64.powf(threshold_db as f64 / 20.0) * max_amp;

    let frame_peak = |frame_idx: usize| -> f64 {
        let mut peak = 0.0_f64;
        let off = data_start + frame_idx * frame_size;
        for ch in 0..channels as usize {
            let p = off + ch * bytes_per_channel_sample;
            let val: f64 = match bps {
                16 => i16::from_le_bytes([bytes[p], bytes[p + 1]]) as f64,
                24 => {
                    let raw = (bytes[p] as i32)
                        | ((bytes[p + 1] as i32) << 8)
                        | ((bytes[p + 2] as i32) << 16);
                    let signed = if raw & 0x80_0000 != 0 {
                        raw | !0xFF_FFFF
                    } else {
                        raw
                    };
                    signed as f64
                }
                _ => 0.0,
            };
            peak = peak.max(val.abs());
        }
        peak
    };

    let window_frames = ((sample_rate as u64 * 20) / 1000).max(1) as usize;
    let window_rms = |start_frame: usize| -> f64 {
        let end = (start_frame + window_frames).min(frame_count);
        if end <= start_frame {
            return 0.0;
        }
        let mut sum_sq = 0.0_f64;
        let mut n = 0u64;
        for f in start_frame..end {
            let p = frame_peak(f);
            sum_sq += p * p;
            n += 1;
        }
        if n == 0 {
            0.0
        } else {
            (sum_sq / n as f64).sqrt()
        }
    };

    let mut head_frame = 0usize;
    while head_frame < frame_count {
        if window_rms(head_frame) > amp_threshold {
            break;
        }
        head_frame += window_frames;
    }

    let mut tail_frame = frame_count;
    while tail_frame > head_frame {
        let start = tail_frame.saturating_sub(window_frames);
        if window_rms(start) > amp_threshold {
            break;
        }
        tail_frame = start;
        if tail_frame == 0 {
            break;
        }
    }

    if tail_frame <= head_frame {
        return Ok(SegmentEdgeSilence {
            leading_ms: total_ms,
            trailing_ms: total_ms,
            duration_ms: total_ms,
            all_silence: true,
        });
    }

    let leading_ms = ((head_frame as f64 * 1000.0) / sample_rate as f64).round() as u64;
    let trailing_frames = frame_count.saturating_sub(tail_frame);
    let trailing_ms = ((trailing_frames as f64 * 1000.0) / sample_rate as f64).round() as u64;
    Ok(SegmentEdgeSilence {
        leading_ms,
        trailing_ms,
        duration_ms: total_ms,
        all_silence: false,
    })
}

