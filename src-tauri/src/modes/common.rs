//! Types every mode may emit. The detection / production logic lives in
//! the mode that owns it — only the data shape is shared here.
//!
//! Coupling note: this is the ONE place where modes touch shared data
//! schema. Adding a flag here doesn't force every mode to emit it —
//! Mode 1 leaves `SegmentRecord::qa_flags` as the empty default (it
//! has its own dialect QA flow in the Tauri annotator), Mode 2 fills
//! it in via `crate::modes::semantic::detect_qa_flags`.

use serde::{Deserialize, Serialize};

/// Per-segment quality-assurance hint emitted to the annotator UI /
/// downstream pipelines. `serde(rename_all = "kebab-case")` keeps the
/// project.json wire format stable (`"qaFlags": ["non-chinese"]`)
/// regardless of internal Rust naming.
///
/// **Add new flags freely** — `#[serde(other)]` on the consuming side
/// (the Tauri annotator) treats unknown variants as ignorable hints,
/// so a new Rust variant won't break older annotator builds.
///
/// Detection is mode-private: each mode picks which flags it can
/// confidently emit. Don't put detection logic in this file — it
/// quickly becomes a junk drawer of regexes that don't apply to every
/// mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SegmentQaFlag {
    /// Whisper output is dominated by non-Chinese characters — usually
    /// means it misclassified the audio as English / pinyin / another
    /// language and the segment needs human attention.
    NonChinese,
    /// Whisper returned an empty string (or whitespace-only) for a
    /// segment that has measurable duration — suggests the cut landed
    /// on a silent / noise-only span.
    Empty,
    /// Chinese character ratio is suspiciously low (e.g. lots of
    /// punctuation, numbers, or mojibake). Borderline cases the
    /// annotator should glance at.
    LowChineseRatio,
    /// Segment audio is long enough to contain speech (>1s) but the
    /// transcribed text is very short (<3 chars). Common cause: stale
    /// segment from a cut decision the LLM later overrode but the
    /// scratch file lingered. Flag it for review.
    VeryShortText,
}
