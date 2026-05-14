//! Per-mode dispatch — each cutting algorithm lives in its own file under
//! this directory. Modes never call into each other; they share only the
//! types in `crate::modes::common`.
//!
//! Design rationale (HANDOFF §6.3, §6.4): Mode 1 (silence-cut for dialect)
//! and Mode 2 (semantic sliding window) have different prompts, different
//! pipeline shapes, and different per-segment metadata. Keeping each
//! mode's private constants and dispatch logic physically separated makes
//! it cheap to add a Mode 3 — drop a new file in `src/modes/`, impl
//! `Mode`, register in `lookup()`.
//!
//! What lives where:
//!   - `common.rs`        — types that ALL modes can emit (e.g.
//!                          `SegmentQaFlag`). Detection logic stays in the
//!                          mode that uses it; only the data shape is shared.
//!   - `silence.rs`       — Mode 1 ("dialect"): silence-detection cut +
//!                          Whisper/Ollama recognize + JSONL bundle.
//!   - `semantic.rs`      — Mode 2 ("semantic"): sliding-window LLM cut.
//!                          Owns the LLM cut prompt + Mandarin/Changsha
//!                          Whisper initial_prompt + QA flag detection.
//!
//! Coupling rules:
//!   1. A mode MAY use shared lib.rs helpers (`scan_project_folder_impl`,
//!      `cut_audio_file_impl`, ...). Those are the foundation, not other
//!      modes.
//!   2. A mode MUST NOT import another mode's module. If logic looks
//!      shared, lift it into `common.rs` (data) or `crate::` (algorithm).
//!   3. Cross-mode fallback (e.g. Mode 2 reusing Mode 1's Ollama pool
//!      when its own is empty) lives in the consuming mode's `run()` —
//!      never reach across modules.

pub(crate) mod common;
pub(crate) mod semantic;

#[cfg(not(target_os = "windows"))]
pub(crate) mod silence;

// `SemanticMode` / `SilenceMode` are reached via `lookup()` from cloud.rs;
// re-exports kept commented for future direct-use callers (Stage 2 D will
// have lib.rs reference `SemanticMode::default_whisper_initial_prompt()`
// directly — uncomment when that lands to avoid the dead-import warning).
// pub(crate) use semantic::SemanticMode;
// #[cfg(not(target_os = "windows"))]
// pub(crate) use silence::SilenceMode;

// ---------------------------------------------------------------------
// Trait + dispatch — cloud-worker-only. Windows reviewer build doesn't
// link `cloud` (see lib.rs top), so the worker-facing trait is gated.
// ---------------------------------------------------------------------

#[cfg(not(target_os = "windows"))]
pub(crate) use dispatch::*;

#[cfg(not(target_os = "windows"))]
mod dispatch {
    use std::path::Path;
    use std::time::Instant;

    use serde_json::Value;
    use tauri::AppHandle;

    use crate::cloud::{CloudClient, CloudError, TaskClaim, WorkerConfig};
    use crate::{CutConfig, ProjectScan};

    /// Everything a mode needs to run one task end-to-end. Owned-ish:
    /// `cut_config` is mutable so a mode can apply its own fallbacks
    /// without leaking that logic back to `cloud.rs`.
    pub(crate) struct ModeRunCtx<'a> {
        pub(crate) app: &'a AppHandle,
        pub(crate) client: &'a CloudClient,
        pub(crate) claim: &'a TaskClaim,
        pub(crate) worker_config: &'a WorkerConfig,
        pub(crate) scan: &'a ProjectScan,
        pub(crate) extract_dir: &'a Path,
        pub(crate) bundle_dir: &'a Path,
        pub(crate) output_zip: &'a Path,
        pub(crate) cut_config: CutConfig,
        pub(crate) started: Instant,
        /// Stage callback — cloud.rs hands us a closure that updates
        /// `CloudState::current_task.stage` + emits the
        /// `cloud://status-changed` event. Modes call this when they
        /// transition between major phases ("cutting" → "recognizing" →
        /// "bundling"). Keeps modes ignorant of `CloudState` (the
        /// frontend status pane is a worker-pipeline concern, not a
        /// per-mode one).
        pub(crate) set_stage: &'a dyn Fn(&str),
    }

    pub(crate) trait Mode: Sync {
        /// Stable id matching the dispatcher's `task.mode` column. Used
        /// for diagnostics + structured logging; `lookup()` is the source
        /// of truth for routing.
        #[allow(dead_code)] // wired up by Stage 2 lookup-driven diagnostics
        fn id(&self) -> &'static str;

        /// Default `initial_prompt` to pass to Whisper when the worker
        /// config doesn't override. Mode-private — never shared across
        /// modes (different dialects, different transcription quirks).
        /// Stage 2 (P0 #2 task D) wires the orchestrators up to call
        /// this; until then it's read via the inherent
        /// `SemanticMode::default_whisper_initial_prompt()` constant.
        #[allow(dead_code)]
        fn default_whisper_initial_prompt(&self) -> &'static str;

        /// Drive the full worker pipeline for this mode. Returns the
        /// summary JSON the dispatcher persists, plus the output zip
        /// size in bytes.
        fn run(&self, ctx: ModeRunCtx<'_>) -> Result<(Value, u64), CloudError>;
    }

    /// Map a dispatcher's `claim.mode` string to a mode handle. Unknown
    /// modes return `None` so the worker can fail the task cleanly
    /// instead of silently picking the wrong algorithm.
    pub(crate) fn lookup(claim_mode: &str) -> Option<&'static dyn Mode> {
        match claim_mode {
            "dialect" => Some(&super::silence::SilenceMode),
            "semantic" => Some(&super::semantic::SemanticMode),
            _ => None,
        }
    }
}
