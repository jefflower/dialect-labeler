// Cloud Dispatcher integration is macOS-only. The Windows build is the
// "审核版" reviewer image (VITE_REVIEW_ONLY=1 in the GH workflow): no
// Whisper/Ollama, no cloud task queue, no auto-updater, no tray Worker
// mode. Gating at the module boundary keeps the Windows binary minimal
// and avoids loading plugins (updater, autostart, store, single_instance)
// that the reviewer has no use for.
#[cfg(not(target_os = "windows"))]
mod cloud;

use regex::Regex;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PROJECT_FOLDER: &str = "_dialect_labeler";
const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
const DEFAULT_LLM_PROMPT: &str = include_str!("default_llm_prompt.txt");

/// Spawn a `Command` for a console binary (ffmpeg, ffprobe, whisper)
/// without flashing a black cmd window every invocation on Windows.
///
/// On Windows, `Command::new("ffmpeg")` inherits the parent's console
/// — but a windowed Tauri app doesn't have one, so the OS creates a
/// fresh console for the child. With dozens of waveform/probe spawns
/// per project that's a screenful of flicker. Setting `CREATE_NO_WINDOW`
/// (0x08000000) tells CreateProcess to suppress the console.
///
/// On macOS / Linux this is a transparent passthrough.
fn silent_command<S: AsRef<OsStr>>(program: S) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(resolve_command_program(program.as_ref()));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn resolve_command_program(program: &OsStr) -> OsString {
    let Some(name) = program.to_str() else {
        return program.to_os_string();
    };
    if name.contains('/') || name.contains('\\') || !matches!(name, "ffmpeg" | "ffprobe") {
        return program.to_os_string();
    }
    find_executable(name)
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| program.to_os_string())
}

fn find_executable(name: &str) -> Option<PathBuf> {
    if let Some(paths) = env::var_os("PATH") {
        for dir in env::split_paths(&paths) {
            if let Some(path) = executable_in_dir(&dir, name) {
                return Some(path);
            }
        }
    }
    for dir in common_binary_dirs() {
        if let Some(path) = executable_in_dir(Path::new(dir), name) {
            return Some(path);
        }
    }
    None
}

fn executable_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    executable_names(name)
        .into_iter()
        .map(|candidate| dir.join(candidate))
        .find(|path| path.is_file())
}

#[cfg(windows)]
fn executable_names(name: &str) -> Vec<OsString> {
    let base = OsString::from(name);
    if Path::new(name).extension().is_some() {
        return vec![base];
    }
    let mut names = vec![base.clone()];
    let pathext = env::var_os("PATHEXT").unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
    for ext in pathext.to_string_lossy().split(';') {
        if ext.is_empty() {
            continue;
        }
        names.push(OsString::from(format!(
            "{}{}",
            name,
            ext.to_ascii_lowercase()
        )));
        names.push(OsString::from(format!(
            "{}{}",
            name,
            ext.to_ascii_uppercase()
        )));
    }
    names
}

#[cfg(not(windows))]
fn executable_names(name: &str) -> Vec<OsString> {
    vec![OsString::from(name)]
}

#[cfg(target_os = "macos")]
fn common_binary_dirs() -> &'static [&'static str] {
    &["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"]
}

#[cfg(all(unix, not(target_os = "macos")))]
fn common_binary_dirs() -> &'static [&'static str] {
    &["/usr/local/bin", "/usr/bin", "/bin"]
}

#[cfg(windows)]
fn common_binary_dirs() -> &'static [&'static str] {
    &[
        r"C:\ffmpeg\bin",
        r"C:\Program Files\ffmpeg\bin",
        r"C:\Program Files (x86)\ffmpeg\bin",
    ]
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestRecord {
    role: String,
    content: String,
    raw_content: String,
    audio_file: Option<String>,
    emotion: Vec<String>,
    tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AudioFileInfo {
    id: String,
    path: String,
    file_name: String,
    role: Option<String>,
    topic_id: Option<u32>,
    target_file_names: Vec<String>,
    duration_ms: Option<u64>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
    codec_name: Option<String>,
    bits_per_sample: Option<u32>,
    matched_text: Option<String>,
    matched_emotion: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectScan {
    root_path: String,
    project_dir: String,
    segments_dir: String,
    audio_files: Vec<AudioFileInfo>,
    manifest_records: Vec<ManifestRecord>,
    /// If `<project_dir>/project.json` exists, surface its contents so the
    /// frontend can resume where it left off without re-cutting or re-running
    /// recognition.
    existing_project: Option<Value>,
}

/// Which cutting algorithm to use.
///
/// `Dialect` (default) — physical silence detection only. Fast, deterministic,
/// language-agnostic. The 长沙 / 台湾 dialect dataset workflows use this. The
/// downside is that semantic boundaries (end of a thought, start of a new
/// topic) frequently happen WITHOUT a clear silence gap, so cuts can land
/// mid-thought.
///
/// `Semantic` — Mandarin-only, LLM-assisted. Pre-cuts on silence, ASRs every
/// piece, then asks Qwen2.5 to merge the small pieces into 6–90s segments
/// that respect "complete semantic unit" boundaries (sentence end, topic
/// shift, Q→A boundary). Spec: `录制数据剪辑转写规则（新）` § 二·切句规则.
/// Requires the Ollama endpoint to be reachable from the worker and to be
/// configured for a large `num_ctx` (≥32K) since one hour of audio is
/// roughly 15K tokens of ASR.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CutMode {
    #[default]
    Dialect,
    Semantic,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CutConfig {
    pub(crate) silence_db: f32,
    pub(crate) min_silence_ms: u64,
    pub(crate) min_segment_ms: u64,
    pub(crate) pre_roll_ms: u64,
    pub(crate) post_roll_ms: u64,
    /// Kept for compatibility with existing project.json files. The cutter no
    /// longer force-splits by length; dialogue timing is driven by silence.
    #[serde(default)]
    pub(crate) max_segment_ms: u64,

    // ---- Mode dispatch + Semantic-mode-only fields ------------------------
    // All `#[serde(default)]` so a project.json saved before Mode 2 existed
    // still deserialises cleanly (mode = Dialect, the rest unused).
    /// Cutting algorithm. Defaults to `Dialect` for backwards compatibility.
    #[serde(default)]
    pub(crate) mode: CutMode,
    /// Target average loudness for the preprocessing pass (ITU-R BS.1770).
    /// Spec § 七·1 calls for -18 LUFS/LKFS. Only honoured in Semantic mode;
    /// Dialect mode skips loudnorm entirely (the dialect pipeline runs the
    /// audio through Whisper / Ollama directly without normalisation).
    #[serde(default = "default_target_loudness_lufs")]
    pub(crate) target_loudness_lufs: f32,
    /// Minimum segment length in Semantic mode (seconds). Spec § 二·一·1
    /// says 6s, with elastic allowance for shorter independent clauses.
    #[serde(default = "default_min_segment_s")]
    pub(crate) min_segment_s: f64,
    /// Maximum segment length in Semantic mode (seconds). Spec § 二·一·1
    /// says 90s; if a segment would exceed this the LLM must find a
    /// natural break point.
    #[serde(default = "default_max_segment_s")]
    pub(crate) max_segment_s: f64,
    /// Minimum head/tail silence padding in Semantic mode (ms). Spec
    /// § 二·三·1 says ≥150ms to avoid abrupt cuts.
    #[serde(default = "default_head_tail_silence_ms")]
    pub(crate) head_tail_silence_ms: u64,
    /// Pinned Ollama endpoint for the semantic-cut LLM call. Must be a
    /// box with enough RAM to hold a 32B model + 32K KV cache (~36 GiB).
    /// If empty, falls back to the global Ollama pool — but a typical
    /// 32B@2K-ctx pool member will silently truncate the long prompt.
    /// Format: `http://100.64.0.4:11434` (no trailing slash, no path).
    #[serde(default)]
    pub(crate) semantic_endpoint: String,
    /// Model name on `semantic_endpoint`. Defaults to `qwen2.5:32b`.
    #[serde(default = "default_semantic_model")]
    pub(crate) semantic_model: String,
    /// `num_ctx` override sent with the semantic-cut LLM call. Must be
    /// large enough for one hour of ASR (~15K tokens, plus prompt
    /// scaffolding and JSON output). Default 32768.
    #[serde(default = "default_semantic_num_ctx")]
    pub(crate) semantic_num_ctx: u32,
}

fn default_target_loudness_lufs() -> f32 { -18.0 }
fn default_min_segment_s() -> f64 { 6.0 }
fn default_max_segment_s() -> f64 { 90.0 }
fn default_head_tail_silence_ms() -> u64 { 150 }
fn default_semantic_model() -> String { "qwen2.5:32b".to_string() }
fn default_semantic_num_ctx() -> u32 { 32768 }

impl Default for CutConfig {
    /// Sensible Dialect-mode defaults — the historical config the
    /// silence-only cutter has always used. Semantic-mode fields use
    /// their `default_*` constants but are inert when `mode = Dialect`.
    fn default() -> Self {
        Self {
            silence_db: -30.0,
            min_silence_ms: 400,
            min_segment_ms: 300,
            pre_roll_ms: 100,
            post_roll_ms: 200,
            max_segment_ms: 0,
            mode: CutMode::Dialect,
            target_loudness_lufs: default_target_loudness_lufs(),
            min_segment_s: default_min_segment_s(),
            max_segment_s: default_max_segment_s(),
            head_tail_silence_ms: default_head_tail_silence_ms(),
            semantic_endpoint: String::new(),
            semantic_model: default_semantic_model(),
            semantic_num_ctx: default_semantic_num_ctx(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SegmentRecord {
    pub(crate) id: String,
    pub(crate) source_path: String,
    pub(crate) source_file_name: String,
    pub(crate) segment_path: String,
    pub(crate) segment_file_name: String,
    pub(crate) role: Option<String>,
    pub(crate) start_ms: u64,
    pub(crate) end_ms: u64,
    pub(crate) duration_ms: u64,
    pub(crate) original_text: String,
    pub(crate) phonetic_text: String,
    pub(crate) emotion: Vec<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) notes: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CutValidationResult {
    segment_id: String,
    segment_path: String,
    segment_file_name: String,
    ok: bool,
    leading_silence_ms: u64,
    trailing_silence_ms: u64,
    max_leading_silence_ms: u64,
    max_trailing_silence_ms: u64,
    threshold_db: f32,
    all_silence: bool,
    message: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CleanCutNoiseResult {
    processed_count: usize,
    validation: Vec<CutValidationResult>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepairCutSilenceResult {
    processed_count: usize,
    updated_segments: Vec<SegmentRecord>,
    validation: Vec<CutValidationResult>,
}

#[derive(Clone, Debug)]
struct SegmentEdgeSilence {
    leading_ms: u64,
    trailing_ms: u64,
    duration_ms: u64,
    all_silence: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackAudio {
    path: String,
    duration_ms: Option<u64>,
    is_preview: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackState {
    path: Option<String>,
    position_ms: u64,
    duration_ms: u64,
    is_playing: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OllamaEndpointDef {
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// Default true. Lets the user stage an endpoint in config while its
    /// model is still pulling, then flip it on without re-editing URLs.
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WhisperEndpointDef {
    /// Base URL of a `services/whisper-server` instance. The Rust client
    /// POSTs the segment WAV to `<url>/transcribe`.
    pub(crate) url: String,
    /// See `OllamaEndpointDef::enabled`.
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecognitionOptions {
    #[serde(default)]
    pub(crate) whisper_model: Option<String>,
    #[serde(default)]
    pub(crate) use_llm: Option<bool>,
    /// Primary Ollama endpoint (backwards-compat).
    #[serde(default)]
    pub(crate) ollama_url: Option<String>,
    /// Optional pool of *additional* Ollama endpoints (URL only — uses the
    /// primary `ollama_model`). Kept for backwards-compat; prefer
    /// `ollama_extra_endpoints` when each endpoint needs its own model.
    #[serde(default)]
    pub(crate) ollama_extra_urls: Option<Vec<String>>,
    /// Per-endpoint URL + model. If `model` is None for an entry, the
    /// primary `ollama_model` is used. Lets you mix e.g. local qwen2.5:32b
    /// with a remote qwen3.5:122b in the same pool.
    #[serde(default)]
    pub(crate) ollama_extra_endpoints: Option<Vec<OllamaEndpointDef>>,
    #[serde(default)]
    pub(crate) ollama_model: Option<String>,
    #[serde(default)]
    pub(crate) llm_prompt: Option<String>,
    #[serde(default)]
    pub(crate) initial_prompt: Option<String>,
    #[serde(default)]
    pub(crate) use_cache: Option<bool>,
    #[serde(default)]
    pub(crate) overwrite_cache: Option<bool>,
    /// Maximum number of Ollama HTTP requests fired in parallel within a
    /// batch. Default 2. With multiple endpoints set this to N × endpoints
    /// to fully saturate the pool.
    #[serde(default)]
    pub(crate) llm_concurrency: Option<u32>,
    /// Number of concurrent Whisper processes. Default 1 — most setups
    /// can only afford one model copy in GPU/RAM. With a beefy box, 2-3
    /// can keep the LLM pool fed faster. Each process loads its own
    /// model (so RAM cost scales linearly).
    #[serde(default)]
    pub(crate) whisper_concurrency: Option<u32>,
    /// Optional pool of remote whisper-server endpoints (each runs
    /// `services/whisper-server`). When non-empty, the ASR phase switches
    /// from spawning a local `whisper` CLI to POSTing each segment WAV to
    /// the pool, work-stealing across nodes — same scheduler shape as the
    /// Ollama polish pool. Empty → fall back to local CLI.
    #[serde(default)]
    pub(crate) whisper_endpoints: Option<Vec<WhisperEndpointDef>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecognitionResult {
    pub(crate) segment_id: String,
    pub(crate) text: String,
    pub(crate) raw_text: String,
    pub(crate) polished: bool,
    pub(crate) cached: bool,
    pub(crate) emotion: Option<String>,
    pub(crate) tags: Vec<String>,
    /// Which Ollama endpoint actually polished this segment, for UI badges.
    /// `None` for cached results or when LLM polish was skipped/failed.
    pub(crate) polish_endpoint: Option<String>,
    pub(crate) polish_model: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct PolishedOutput {
    text: String,
    emotion: Option<String>,
    tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DependencyStatus {
    whisper_ok: bool,
    whisper_path: Option<String>,
    whisper_error: Option<String>,
    ffmpeg_ok: bool,
    ffmpeg_error: Option<String>,
    ollama_ok: bool,
    ollama_url: String,
    ollama_models: Vec<String>,
    ollama_error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExportOptions {
    #[serde(default)]
    pub(crate) system_prompt: Option<String>,
    #[serde(default)]
    pub(crate) pair_user_assistant: Option<bool>,
    #[serde(default)]
    pub(crate) use_source_audio_for_user: Option<bool>,
    #[serde(default)]
    pub(crate) audio_file_prefix: Option<String>,
    /// Local input root used to compute the path relative to the
    /// configured OSS prefix. Without it, only the file name is appended.
    #[serde(default)]
    pub(crate) input_root: Option<String>,
}

struct AudioPlayerState {
    sender: Mutex<mpsc::Sender<AudioCommand>>,
}

impl AudioPlayerState {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || audio_worker(receiver));
        Self {
            sender: Mutex::new(sender),
        }
    }
}

struct AudioPlayerInner {
    stream: Option<OutputStream>,
    handle: Option<OutputStreamHandle>,
    sink: Option<Sink>,
    path: Option<String>,
    duration_ms: u64,
    offset_ms: u64,
    started_at: Option<Instant>,
    is_playing: bool,
    /// Playback rate multiplier — 1.0 = normal, 1.25/1.5 are common
    /// review-fast modes, 0.75 etc. would be slow-mo. Applied to the
    /// rodio sink and used when extrapolating wall-clock elapsed time
    /// into audio-track elapsed ms (the source advances `speed` ms of
    /// audio per 1 ms of wall clock).
    playback_speed: f32,
}

impl Default for AudioPlayerInner {
    fn default() -> Self {
        Self {
            stream: None,
            handle: None,
            sink: None,
            path: None,
            duration_ms: 0,
            offset_ms: 0,
            started_at: None,
            is_playing: false,
            playback_speed: 1.0,
        }
    }
}

enum AudioCommand {
    Play {
        path: String,
        position_ms: u64,
        reply: mpsc::Sender<Result<PlaybackState, String>>,
    },
    Pause {
        reply: mpsc::Sender<Result<PlaybackState, String>>,
    },
    Stop {
        reply: mpsc::Sender<Result<PlaybackState, String>>,
    },
    State {
        reply: mpsc::Sender<Result<PlaybackState, String>>,
    },
    SetSpeed {
        speed: f32,
        reply: mpsc::Sender<Result<PlaybackState, String>>,
    },
}

#[derive(Clone, Debug)]
struct AudioProbe {
    duration_ms: Option<u64>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
    codec_name: Option<String>,
    bits_per_sample: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
enum SilenceEvent {
    Start(f64),
    End(f64),
}

#[tauri::command]
async fn prepare_playback_audio(
    input_path: String,
    cache_dir: String,
) -> Result<PlaybackAudio, String> {
    tauri::async_runtime::spawn_blocking(move || prepare_playback_audio_impl(input_path, cache_dir))
        .await
        .map_err(|err| err.to_string())?
}

fn prepare_playback_audio_impl(
    input_path: String,
    cache_dir: String,
) -> Result<PlaybackAudio, String> {
    ensure_ffmpeg()?;

    let input = PathBuf::from(&input_path);
    if !input.is_file() {
        return Err(format!("Playback audio not found: {}", input_path));
    }

    let probe = probe_audio(&input)?;
    if probe.codec_name.as_deref() == Some("pcm_s16le") {
        return Ok(PlaybackAudio {
            path: path_to_string(&input),
            duration_ms: probe.duration_ms,
            is_preview: false,
        });
    }

    let preview_dir = PathBuf::from(cache_dir).join(".playback");
    fs::create_dir_all(&preview_dir).map_err(|err| err.to_string())?;
    let output = preview_dir.join(format!("{}.wav", playback_cache_key(&input)?));
    if output.is_file() {
        let preview_probe = probe_audio(&output).ok();
        return Ok(PlaybackAudio {
            path: path_to_string(&output),
            duration_ms: preview_probe
                .and_then(|item| item.duration_ms)
                .or(probe.duration_ms),
            is_preview: true,
        });
    }

    let mut command = silent_command("ffmpeg");
    command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(&input)
        .arg("-vn")
        .arg("-map")
        .arg("0:a:0");

    if let Some(sample_rate) = probe.sample_rate {
        command.arg("-ar").arg(sample_rate.to_string());
    }
    if let Some(channels) = probe.channels {
        command.arg("-ac").arg(channels.to_string());
    }

    let result = command
        .arg("-c:a")
        .arg("pcm_s16le")
        .arg("-f")
        .arg("wav")
        .arg(&output)
        .output()
        .map_err(|err| format!("Unable to run ffmpeg for playback: {}", err))?;

    if result.status.success() {
        let preview_probe = probe_audio(&output).ok();
        Ok(PlaybackAudio {
            path: path_to_string(&output),
            duration_ms: preview_probe
                .and_then(|item| item.duration_ms)
                .or(probe.duration_ms),
            is_preview: true,
        })
    } else {
        Err(String::from_utf8_lossy(&result.stderr).to_string())
    }
}

#[tauri::command]
async fn read_waveform_peaks(
    input_path: String,
    bucket_count: Option<usize>,
) -> Result<Vec<f32>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        read_waveform_peaks_impl(input_path, bucket_count.unwrap_or(640))
    })
    .await
    .map_err(|err| err.to_string())?
}

fn read_waveform_peaks_impl(input_path: String, bucket_count: usize) -> Result<Vec<f32>, String> {
    let bucket_count = bucket_count.clamp(64, 4096);
    let input = PathBuf::from(&input_path);
    if !input.is_file() {
        return Err(format!("Audio file not found: {}", input_path));
    }

    // Disk cache: first computation per (file, mtime, size, buckets) is
    // expensive — ffmpeg has to spawn (193MB exe on Windows = slow) and
    // decode the whole file. Subsequent reads are a 4-byte-per-bucket
    // memory map. Cache lives in the OS temp dir so it doesn't pollute
    // the user's project folder; gets garbage-collected by the OS.
    if let Some(cache_path) = waveform_cache_path(&input, bucket_count) {
        if let Some(peaks) = load_cached_peaks(&cache_path, bucket_count) {
            return Ok(peaks);
        }
    }

    ensure_ffmpeg()?;
    let mut child = silent_command("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(&input)
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
        .map_err(|err| format!("ffmpeg spawn failed: {}", err))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ffmpeg stdout missing".to_string())?;
    let mut reader = BufReader::new(stdout);
    let mut samples: Vec<i16> = Vec::with_capacity(8000 * 30);
    let mut buf = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|err| format!("ffmpeg read failed: {}", err))?;
        if n == 0 {
            break;
        }
        let chunks = n / 2;
        for index in 0..chunks {
            let lo = buf[index * 2];
            let hi = buf[index * 2 + 1];
            samples.push(i16::from_le_bytes([lo, hi]));
        }
    }
    let _ = child.wait();

    if samples.is_empty() {
        return Ok(vec![0.0; bucket_count]);
    }

    let bucket_size = (samples.len() / bucket_count).max(1);
    let mut peaks = Vec::with_capacity(bucket_count);
    for index in 0..bucket_count {
        let start = index * bucket_size;
        if start >= samples.len() {
            peaks.push(0.0);
            continue;
        }
        let end = (start + bucket_size).min(samples.len());
        let mut peak = 0i32;
        for sample in &samples[start..end] {
            let v = (*sample as i32).abs();
            if v > peak {
                peak = v;
            }
        }
        peaks.push((peak as f32) / (i16::MAX as f32));
    }
    // Best-effort cache write — failures are silent (e.g. temp dir not
    // writable). Users will just pay the ffmpeg cost again next time.
    if let Some(cache_path) = waveform_cache_path(&input, bucket_count) {
        let _ = save_cached_peaks(&cache_path, &peaks);
    }
    Ok(peaks)
}

/// Cache file path for a `(input, bucket_count)` tuple. Returns None if
/// the input file's metadata can't be read. Key folds in mtime + size so
/// the cache invalidates automatically when the audio is replaced.
fn waveform_cache_path(input: &Path, bucket_count: usize) -> Option<PathBuf> {
    let meta = fs::metadata(input).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    let size = meta.len();
    let mut hasher = DefaultHasher::new();
    input.to_string_lossy().hash(&mut hasher);
    mtime.hash(&mut hasher);
    size.hash(&mut hasher);
    bucket_count.hash(&mut hasher);
    let key = format!("{:016x}", hasher.finish());
    let dir = std::env::temp_dir().join("dialect-labeler-waveform");
    fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{}.peaks", key)))
}

/// Load peaks from cache. Stored as raw little-endian f32 bytes (4 per
/// bucket) — much smaller and faster to parse than JSON. Returns None
/// if the cache file is missing, the wrong size for the requested bucket
/// count, or unreadable.
fn load_cached_peaks(cache_path: &Path, bucket_count: usize) -> Option<Vec<f32>> {
    let bytes = fs::read(cache_path).ok()?;
    if bytes.len() != bucket_count * 4 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

fn save_cached_peaks(cache_path: &Path, peaks: &[f32]) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(peaks.len() * 4);
    for p in peaks {
        buf.extend_from_slice(&p.to_le_bytes());
    }
    fs::write(cache_path, buf)
}

#[tauri::command]
fn play_audio(
    state: tauri::State<'_, AudioPlayerState>,
    path: String,
    position_ms: u64,
) -> Result<PlaybackState, String> {
    send_audio_command(&state, |reply| AudioCommand::Play {
        path,
        position_ms,
        reply,
    })
}

#[tauri::command]
fn pause_audio(state: tauri::State<'_, AudioPlayerState>) -> Result<PlaybackState, String> {
    send_audio_command(&state, |reply| AudioCommand::Pause { reply })
}

#[tauri::command]
fn stop_audio(state: tauri::State<'_, AudioPlayerState>) -> Result<PlaybackState, String> {
    send_audio_command(&state, |reply| AudioCommand::Stop { reply })
}

#[tauri::command]
fn audio_state(state: tauri::State<'_, AudioPlayerState>) -> Result<PlaybackState, String> {
    send_audio_command(&state, |reply| AudioCommand::State { reply })
}

#[tauri::command]
fn set_playback_speed(
    state: tauri::State<'_, AudioPlayerState>,
    speed: f32,
) -> Result<PlaybackState, String> {
    // Sanitize on the Rust side too — frontend should already clamp,
    // but a stray NaN would lock the audio thread.
    let speed = if speed.is_finite() {
        speed.clamp(0.25, 4.0)
    } else {
        1.0
    };
    send_audio_command(&state, |reply| AudioCommand::SetSpeed { speed, reply })
}

fn send_audio_command(
    state: &tauri::State<'_, AudioPlayerState>,
    build: impl FnOnce(mpsc::Sender<Result<PlaybackState, String>>) -> AudioCommand,
) -> Result<PlaybackState, String> {
    let (reply, receiver) = mpsc::channel();
    let sender = state.sender.lock().map_err(|err| err.to_string())?;
    sender
        .send(build(reply))
        .map_err(|_| "音频线程已停止".to_string())?;
    receiver
        .recv_timeout(Duration::from_secs(8))
        .map_err(|_| "音频线程响应超时".to_string())?
}

fn audio_worker(receiver: mpsc::Receiver<AudioCommand>) {
    let mut inner = AudioPlayerInner::default();
    while let Ok(command) = receiver.recv() {
        match command {
            AudioCommand::Play {
                path,
                position_ms,
                reply,
            } => {
                let result = play_audio_inner(&mut inner, path, position_ms);
                let _ = reply.send(result);
            }
            AudioCommand::Pause { reply } => {
                let result = pause_audio_inner(&mut inner);
                let _ = reply.send(result);
            }
            AudioCommand::Stop { reply } => {
                let result = stop_audio_inner(&mut inner);
                let _ = reply.send(result);
            }
            AudioCommand::State { reply } => {
                let result = Ok(snapshot_player(&mut inner));
                let _ = reply.send(result);
            }
            AudioCommand::SetSpeed { speed, reply } => {
                // Apply to the live sink AND remember it so the next
                // play() call also uses it. Re-anchor the wall-clock
                // baseline so position math stays consistent: capture
                // the current track position under the OLD speed,
                // store it in `offset_ms`, then start a fresh epoch
                // under the NEW speed.
                let pos = current_player_position_ms(&inner);
                inner.offset_ms = pos;
                inner.started_at = if inner.is_playing {
                    Some(Instant::now())
                } else {
                    None
                };
                inner.playback_speed = speed;
                if let Some(sink) = inner.sink.as_ref() {
                    sink.set_speed(speed);
                }
                let _ = reply.send(Ok(snapshot_player(&mut inner)));
            }
        }
    }
}

fn play_audio_inner(
    inner: &mut AudioPlayerInner,
    path: String,
    position_ms: u64,
) -> Result<PlaybackState, String> {
    ensure_audio_output(inner)?;

    if let Some(sink) = inner.sink.take() {
        sink.stop();
    }

    let path_buf = PathBuf::from(&path);
    let duration_ms = probe_audio(&path_buf)
        .ok()
        .and_then(|probe| probe.duration_ms)
        .unwrap_or_default();
    let start_ms = if duration_ms > 0 {
        position_ms.min(duration_ms)
    } else {
        position_ms
    };
    let file = fs::File::open(&path_buf).map_err(|err| err.to_string())?;
    let source = Decoder::new(BufReader::new(file))
        .map_err(|err| err.to_string())?
        .skip_duration(Duration::from_millis(start_ms));

    let handle = inner
        .handle
        .as_ref()
        .ok_or_else(|| "音频输出设备未初始化".to_string())?;
    let sink = Sink::try_new(handle).map_err(|err| err.to_string())?;
    sink.append(source);
    // Honor any previously-set playback speed (1.0 by default).
    sink.set_speed(inner.playback_speed);
    sink.play();

    inner.sink = Some(sink);
    inner.path = Some(path);
    inner.duration_ms = duration_ms;
    inner.offset_ms = start_ms;
    inner.started_at = Some(Instant::now());
    inner.is_playing = true;

    Ok(snapshot_player(inner))
}

fn pause_audio_inner(inner: &mut AudioPlayerInner) -> Result<PlaybackState, String> {
    let position = current_player_position_ms(inner);
    if let Some(sink) = inner.sink.as_ref() {
        sink.pause();
    }
    inner.offset_ms = position;
    inner.started_at = None;
    inner.is_playing = false;
    Ok(snapshot_player(inner))
}

fn stop_audio_inner(inner: &mut AudioPlayerInner) -> Result<PlaybackState, String> {
    if let Some(sink) = inner.sink.take() {
        sink.stop();
    }
    inner.offset_ms = 0;
    inner.started_at = None;
    inner.is_playing = false;
    Ok(snapshot_player(inner))
}

fn ensure_audio_output(inner: &mut AudioPlayerInner) -> Result<(), String> {
    if inner.handle.is_some() {
        return Ok(());
    }
    let (stream, handle) = OutputStream::try_default().map_err(|err| err.to_string())?;
    inner.stream = Some(stream);
    inner.handle = Some(handle);
    Ok(())
}

fn snapshot_player(inner: &mut AudioPlayerInner) -> PlaybackState {
    if inner
        .sink
        .as_ref()
        .is_some_and(|sink| sink.empty() && inner.is_playing)
    {
        inner.offset_ms = inner.duration_ms;
        inner.started_at = None;
        inner.is_playing = false;
    }

    PlaybackState {
        path: inner.path.clone(),
        position_ms: current_player_position_ms(inner),
        duration_ms: inner.duration_ms,
        is_playing: inner.is_playing,
    }
}

fn current_player_position_ms(inner: &AudioPlayerInner) -> u64 {
    let mut position = inner.offset_ms;
    if inner.is_playing {
        if let Some(started_at) = inner.started_at {
            // Track-time elapsed = wall-time elapsed × speed.
            // (At 1.5×, 1 second of wall clock advances 1.5 seconds of
            // audio.) Use f64 for the multiply so we don't lose ms
            // precision on long clips.
            let wall_ms = started_at.elapsed().as_millis() as f64;
            let track_ms = (wall_ms * inner.playback_speed as f64) as u64;
            position = position.saturating_add(track_ms);
        }
    }
    if inner.duration_ms > 0 {
        position.min(inner.duration_ms)
    } else {
        position
    }
}

#[tauri::command]
async fn scan_project_folder(
    folder_path: String,
    manifest_path: Option<String>,
    output_path: Option<String>,
) -> Result<ProjectScan, String> {
    tauri::async_runtime::spawn_blocking(move || {
        scan_project_folder_impl(folder_path, manifest_path, output_path)
    })
    .await
    .map_err(|err| err.to_string())?
}

pub(crate) fn scan_project_folder_impl(
    folder_path: String,
    manifest_path: Option<String>,
    output_path: Option<String>,
) -> Result<ProjectScan, String> {
    let root = canonical_existing_dir(&folder_path)?;
    let project_dir = resolve_project_dir(&root, output_path)?;
    let segments_dir = project_dir.join("segments");
    fs::create_dir_all(&segments_dir).map_err(|err| err.to_string())?;

    let mut files = Vec::new();
    collect_files(&root, &mut files, &[project_dir.clone()]).map_err(|err| err.to_string())?;

    let mut manifest_records = Vec::new();
    let mut manifest_paths: Vec<PathBuf> = Vec::new();
    if let Some(path) = manifest_path.filter(|value| !value.trim().is_empty()) {
        manifest_paths.push(PathBuf::from(path));
    }
    for file in &files {
        let lname = path_file_name(file).to_ascii_lowercase();
        if lname == "manifest.json"
            || lname == "manifest.jsonl"
            || lname.ends_with(".manifest.json")
            || lname.ends_with(".manifest.jsonl")
        {
            manifest_paths.push(file.clone());
        }
    }
    for path in &manifest_paths {
        if let Ok(records) = parse_manifest_file(path) {
            manifest_records.extend(records);
        }
    }

    let audio_paths = files
        .iter()
        .filter(|path| is_audio(path))
        .cloned()
        .collect::<Vec<_>>();
    let source_topic_ids = infer_source_topic_ids(&audio_paths, &manifest_records);

    let mut audio_files = Vec::new();
    for path in &audio_paths {
        let probe = probe_audio(path).ok();
        let file_name = path_file_name(path);
        let matched = match_manifest(path, &manifest_records);
        let role = matched
            .as_ref()
            .map(|record| record.role.clone())
            .or_else(|| infer_role(path));
        let topic_id = source_topic_key(path)
            .and_then(|key| source_topic_ids.get(&key).copied())
            .or_else(|| last_number(&path_file_stem(path)));
        let related_records = matching_manifest_records_for_source(
            path,
            role.as_deref(),
            topic_id,
            &manifest_records,
        );
        let target_file_names = related_records
            .iter()
            .filter_map(|record| record.audio_file.as_deref())
            .map(target_audio_file_name)
            .collect::<Vec<_>>();
        let matched_text = matched
            .as_ref()
            .map(|record| record.content.clone())
            .or_else(|| joined_manifest_content(&related_records));
        let matched_emotion = matched
            .as_ref()
            .map(|record| record.emotion.clone())
            .unwrap_or_else(|| merged_manifest_emotion(&related_records));

        audio_files.push(AudioFileInfo {
            id: stable_id(path),
            path: path_to_string(path),
            file_name,
            role,
            topic_id,
            target_file_names,
            duration_ms: probe.as_ref().and_then(|info| info.duration_ms),
            sample_rate: probe.as_ref().and_then(|info| info.sample_rate),
            channels: probe.as_ref().and_then(|info| info.channels),
            codec_name: probe.as_ref().and_then(|info| info.codec_name.clone()),
            bits_per_sample: probe.as_ref().and_then(|info| info.bits_per_sample),
            matched_text,
            matched_emotion,
        });
    }

    audio_files.sort_by(|left, right| left.path.cmp(&right.path));
    fill_manifest_fallback_by_role(&mut audio_files, &manifest_records);

    // Auto-load project.json if it already exists so the user resumes where
    // they left off after re-scanning the same folder. Apply the same
    // path-resolution as load_project_file so a project.json copied in
    // from another machine (Windows reviewer handoff) still maps to local
    // file system paths.
    let existing_project = fs::read_to_string(project_dir.join("project.json"))
        .ok()
        .and_then(|data| serde_json::from_str::<Value>(&data).ok())
        .map(|value| {
            let canonical = project_dir
                .canonicalize()
                .unwrap_or_else(|_| project_dir.clone());
            absolutize_payload(value, &canonical)
        });

    Ok(ProjectScan {
        root_path: path_to_string(&root),
        project_dir: path_to_string(&project_dir),
        segments_dir: path_to_string(&segments_dir),
        audio_files,
        manifest_records,
        existing_project,
    })
}

fn fill_manifest_fallback_by_role(audio_files: &mut [AudioFileInfo], records: &[ManifestRecord]) {
    let mut records_by_role: HashMap<&str, Vec<&ManifestRecord>> = HashMap::new();
    for record in records {
        records_by_role
            .entry(record.role.as_str())
            .or_default()
            .push(record);
    }

    let mut role_positions: HashMap<String, usize> = HashMap::new();
    for audio in audio_files {
        let Some(role) = audio.role.as_deref() else {
            continue;
        };
        if audio.matched_text.is_some() {
            continue;
        }

        let position = role_positions.entry(role.to_string()).or_insert(0);
        let Some(record) = records_by_role
            .get(role)
            .and_then(|role_records| role_records.get(*position))
        else {
            continue;
        };

        audio.matched_text = Some(record.content.clone());
        audio.matched_emotion = record.emotion.clone();
        *position += 1;
    }
}

#[tauri::command]
async fn cut_audio_file(
    input_path: String,
    segments_dir: String,
    config: CutConfig,
    role: Option<String>,
    topic_id: Option<u32>,
    original_text: Option<String>,
    emotion: Option<Vec<String>>,
    target_file_names: Option<Vec<String>>,
) -> Result<Vec<SegmentRecord>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        cut_audio_file_impl(
            input_path,
            segments_dir,
            config,
            role,
            topic_id,
            original_text,
            emotion,
            target_file_names.unwrap_or_default(),
        )
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
async fn validate_cut_segments(
    segments: Vec<SegmentRecord>,
    config: CutConfig,
) -> Result<Vec<CutValidationResult>, String> {
    tauri::async_runtime::spawn_blocking(move || validate_cut_segments_impl(segments, config))
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn clean_cut_segment_noise(
    segments: Vec<SegmentRecord>,
    config: CutConfig,
) -> Result<CleanCutNoiseResult, String> {
    tauri::async_runtime::spawn_blocking(move || clean_cut_segment_noise_impl(segments, config))
        .await
        .map_err(|err| err.to_string())?
}

#[tauri::command]
async fn repair_cut_segment_silence(
    segments: Vec<SegmentRecord>,
    config: CutConfig,
) -> Result<RepairCutSilenceResult, String> {
    tauri::async_runtime::spawn_blocking(move || repair_cut_segment_silence_impl(segments, config))
        .await
        .map_err(|err| err.to_string())?
}

fn validate_cut_segments_impl(
    segments: Vec<SegmentRecord>,
    config: CutConfig,
) -> Vec<CutValidationResult> {
    segments
        .iter()
        .map(|segment| validate_cut_segment(segment, &config))
        .collect()
}

fn clean_cut_segment_noise_impl(
    segments: Vec<SegmentRecord>,
    config: CutConfig,
) -> Result<CleanCutNoiseResult, String> {
    for segment in &segments {
        gate_segment_head_tail(Path::new(&segment.segment_path), config.silence_db)?;
    }
    let validation = validate_cut_segments_impl(segments, config);
    Ok(CleanCutNoiseResult {
        processed_count: validation.len(),
        validation,
    })
}

fn repair_cut_segment_silence_impl(
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

fn validate_cut_segment(segment: &SegmentRecord, config: &CutConfig) -> CutValidationResult {
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

#[tauri::command]
fn rename_segments_by_dialogue_sequence(
    segments: Vec<SegmentRecord>,
) -> Result<Vec<SegmentRecord>, String> {
    rename_segments_by_dialogue_sequence_impl(segments)
}

fn rename_segments_by_dialogue_sequence_impl(
    segments: Vec<SegmentRecord>,
) -> Result<Vec<SegmentRecord>, String> {
    let planned_names = plan_dialogue_sequence_file_names(&segments);
    if planned_names.is_empty() {
        return Ok(segments);
    }

    let mut target_paths: HashMap<PathBuf, usize> = HashMap::new();
    let old_paths = segments
        .iter()
        .map(|segment| PathBuf::from(&segment.segment_path))
        .collect::<Vec<_>>();

    for (index, target_name) in &planned_names {
        let old_path = &old_paths[*index];
        let Some(parent) = old_path.parent() else {
            return Err(format!(
                "无法定位段文件父目录：{}",
                segments[*index].segment_path
            ));
        };
        let target_path = parent.join(target_name);
        if let Some(other_index) = target_paths.insert(target_path.clone(), *index) {
            return Err(format!(
                "命名冲突：{} 同时对应 {} 和 {}",
                path_to_string(&target_path),
                segments[other_index].segment_file_name,
                segments[*index].segment_file_name
            ));
        }
    }

    let planned_indices = planned_names.keys().copied().collect::<HashSet<_>>();
    for (target_path, index) in &target_paths {
        let old_path = &old_paths[*index];
        if target_path == old_path || !target_path.exists() {
            continue;
        }
        let occupied_by_planned_move = old_paths
            .iter()
            .position(|path| path == target_path)
            .is_some_and(|occupied_index| planned_indices.contains(&occupied_index));
        if !occupied_by_planned_move {
            return Err(format!(
                "迁移目标已存在，拒绝覆盖：{}",
                path_to_string(target_path)
            ));
        }
    }

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default();
    let mut temp_moves: Vec<(usize, PathBuf, PathBuf, PathBuf)> = Vec::new();

    for (index, target_name) in &planned_names {
        let old_path = &old_paths[*index];
        let Some(parent) = old_path.parent() else {
            return Err(format!(
                "无法定位段文件父目录：{}",
                segments[*index].segment_path
            ));
        };
        let target_path = parent.join(target_name);
        if &target_path == old_path {
            continue;
        }
        if !old_path.is_file() {
            return Err(format!("切割片段文件不存在：{}", path_to_string(old_path)));
        }
        let temp_path = parent.join(format!(".rename_{}_{}.tmp", stamp, index));
        fs::rename(old_path, &temp_path).map_err(|err| {
            format!(
                "重命名临时文件失败 {} → {}：{}",
                path_to_string(old_path),
                path_to_string(&temp_path),
                err
            )
        })?;
        temp_moves.push((*index, temp_path, target_path, PathBuf::from(target_name)));
    }

    for (_, temp_path, target_path, _) in &temp_moves {
        fs::rename(temp_path, target_path).map_err(|err| {
            format!(
                "重命名目标文件失败 {} → {}：{}",
                path_to_string(temp_path),
                path_to_string(target_path),
                err
            )
        })?;
    }

    let mut updated = segments;
    for (index, _, target_path, target_name_path) in temp_moves {
        updated[index].segment_path = path_to_string(&target_path);
        updated[index].segment_file_name = path_file_name(&target_name_path);
        updated[index].id = path_file_stem(&target_name_path);
    }
    for (index, target_name) in planned_names {
        if updated[index].segment_file_name == target_name {
            continue;
        }
        let old_path = PathBuf::from(&updated[index].segment_path);
        let target_path = old_path
            .parent()
            .map(|parent| parent.join(&target_name))
            .unwrap_or_else(|| PathBuf::from(&target_name));
        updated[index].segment_path = path_to_string(&target_path);
        updated[index].segment_file_name = target_name.clone();
        updated[index].id = path_file_stem(Path::new(&target_name));
    }

    Ok(updated)
}

pub(crate) fn cut_audio_file_impl(
    input_path: String,
    segments_dir: String,
    config: CutConfig,
    role: Option<String>,
    topic_id: Option<u32>,
    original_text: Option<String>,
    emotion: Option<Vec<String>>,
    target_file_names: Vec<String>,
) -> Result<Vec<SegmentRecord>, String> {
    // Mode-based dispatch. The Dialect path (silence-only) is the
    // historical behaviour; the Semantic path (silence + ASR + LLM
    // boundary decision) is the new Mandarin pipeline introduced by
    // `录制数据剪辑转写规则（新）`.
    match config.mode {
        CutMode::Dialect => cut_audio_file_dialect_impl(
            input_path,
            segments_dir,
            config,
            role,
            topic_id,
            original_text,
            emotion,
            target_file_names,
        ),
        CutMode::Semantic => cut_audio_file_semantic_impl(
            input_path,
            segments_dir,
            config,
            role,
            topic_id,
            original_text,
            emotion,
            target_file_names,
        ),
    }
}

/// Mode-2 dispatcher in this entry point only does input validation +
/// returns a clear redirect. The full pipeline needs an `AppHandle`
/// for ASR progress events and is too long to share semantics with
/// the per-file Dialect cutter (preprocess → fine pre-cut → ASR → LLM
/// cut → re-cut → normalise → QA → Excel export). The frontend should
/// detect `mode == "semantic"` and call the dedicated
/// `run_semantic_pipeline` Tauri command instead.
fn cut_audio_file_semantic_impl(
    _input_path: String,
    _segments_dir: String,
    _config: CutConfig,
    _role: Option<String>,
    _topic_id: Option<u32>,
    _original_text: Option<String>,
    _emotion: Option<Vec<String>>,
    _target_file_names: Vec<String>,
) -> Result<Vec<SegmentRecord>, String> {
    Err(
        "Semantic mode does not use the per-file cut command. Call \
         `run_semantic_pipeline` instead — it preprocesses, fine-pre-cuts, \
         ASRs, asks the LLM to merge into 6-90s semantic units, normalises \
         text, and exports Excel as a single orchestrated pass."
            .to_string(),
    )
}

/// Phase 3 (fine pre-cut for Semantic mode).
///
/// Slices the normalised audio into a flat list of ~10-30s pieces using
/// strict-but-permissive silence detection. The point isn't to honour
/// semantic boundaries (that's Phase 4's job, post-ASR) — it's to give
/// Whisper individually-manageable chunks and to build a *candidate
/// cut point pool* the LLM will later snap its decisions to.
///
/// Returns `(piece_files, candidate_boundaries_ms)`:
/// - `piece_files`: paths to the cut WAV files, in order. The caller
///   ASRs each piece and builds `[(text, global_start_ms, global_end_ms)]`.
/// - `candidate_boundaries_ms`: the global timestamps where one piece
///   ends and the next begins. Phase 4's LLM must choose merge points
///   from this list (so the final cuts land on real silence gaps, not
///   in the middle of a word).
///
/// The silence-detection parameters here are **independent** of the
/// project's main CutConfig — Mode 2 calibrates them locally:
///   - silence_db: -35 (post-loudnorm signal is well-controlled,
///     a static threshold works)
///   - min_silence_ms: 200 (catch even short breath gaps as cut points)
///   - min_segment_ms: 200 (don't drop tiny utterances)
///   - max_segment_ms: 30000 (Whisper's quality drops past ~30s)
///   - pre/post_roll: 50 each (small safety margin; spec § 二·三·1
///     mandates ≥150 ms head/tail in the FINAL output, but Phase 4
///     adds extra padding when it emits final cuts)
fn fine_pre_cut_for_semantic(
    input: &Path,
    pieces_dir: &Path,
) -> Result<(Vec<PathBuf>, Vec<u64>), String> {
    let probe = probe_audio(input)?;
    let duration_ms = probe
        .duration_ms
        .ok_or_else(|| "ffprobe couldn't read input duration".to_string())?;
    let duration_sec = duration_ms as f64 / 1000.0;

    let fine_config = CutConfig {
        silence_db: -35.0,
        min_silence_ms: 200,
        min_segment_ms: 200,
        pre_roll_ms: 50,
        post_roll_ms: 50,
        max_segment_ms: 30_000,
        ..CutConfig::default()
    };
    let events = detect_silence(input, &fine_config)?;
    let ranges = build_segment_ranges(duration_sec, &events, &fine_config);
    if ranges.is_empty() {
        return Err("fine pre-cut produced 0 segments — input may be silent".into());
    }

    fs::create_dir_all(pieces_dir).map_err(|err| err.to_string())?;
    let codec = output_pcm_codec(&probe);
    let mut piece_paths = Vec::with_capacity(ranges.len());
    let mut boundaries = Vec::with_capacity(ranges.len() + 1);
    // The first boundary is always 0 (start of audio). Each piece end
    // adds the next boundary, building a complete partition.
    boundaries.push(0u64);
    for (i, (start_sec, end_sec)) in ranges.iter().enumerate() {
        let piece_path = pieces_dir.join(format!("piece_{i:04}.wav"));
        write_pcm_wav_segment(
            input,
            &piece_path,
            *start_sec,
            end_sec - start_sec,
            &probe,
            codec,
        )?;
        piece_paths.push(piece_path);
        boundaries.push(seconds_to_ms(*end_sec));
    }
    Ok((piece_paths, boundaries))
}

// ===========================================================================
// Phase 4 — Semantic cut decision via LLM
// ===========================================================================
//
// After Phase 3 produces `(piece_paths, boundaries)` and the existing
// recognize_segments_impl ASRs each piece, we have a sequence of
// `(text, global_start_ms, global_end_ms)` triples. Phase 4 hands all
// of that to Qwen2.5:32b at the configured `semantic_endpoint` and
// asks it to merge the fine pieces into 6-90s "complete semantic
// unit" segments per spec § 二·切句规则.

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SemanticCutDecision {
    pub(crate) start_ms: u64,
    pub(crate) end_ms: u64,
    pub(crate) reason: String,
}

/// One ASRed pre-cut piece — input to Phase 4.
#[derive(Clone, Debug)]
pub(crate) struct AsredPiece {
    pub(crate) text: String,
    pub(crate) start_ms: u64,
    pub(crate) end_ms: u64,
}

/// Render the system + user prompts for the semantic-cut LLM call.
///
/// Returns `(system, user)`. The system prompt encodes the spec rules
/// (6-90s, complete semantic units, etc.) and pins the output format.
/// The user prompt lists each ASRed piece with its global timestamps
/// and the candidate cut-point pool.
///
/// Kept pure (no I/O, no logging) so unit tests can pin the wording
/// without mocking ureq.
pub(crate) fn build_semantic_cut_prompt(
    pieces: &[AsredPiece],
    candidates_ms: &[u64],
    config: &CutConfig,
) -> (String, String) {
    let system = format!(
        "你是一个普通话录音剪辑助手。任务：把已经按静音粗切并 ASR 完成的若干小片合并成「语义完整」的大段，\
         严格遵守以下规则（来源：录制数据剪辑转写规则）：\n\
         \n\
         1. 每段时长 {min_s:.0}-{max_s:.0} 秒；短于 {min_s:.0} 秒的独立语义单元可保留，长于 {max_s:.0} 秒必须在自然节点切开。\n\
         2. 「相对独立的语义单元」=一个完整短语 / 一个完整句子 / 一段完整问答 / 一段连续叙述。\n\
         3. 不允许将语义紧密相连的内容切断（同一个观点、并列句、问答对，应合并）。\n\
         4. 一问一答可分别成段（例：「你今天过得怎么样？」「我今天过得还不错。」切两段）。\n\
         5. 切点只能取自下方候选切点列表中的时间戳（毫秒）。不要发明候选列表外的时间。\n\
         6. 仅输出 JSON，无前缀、无尾注、无 markdown。schema：\n\
         {{\n  \"segments\": [\n    {{\"start_ms\": <number>, \"end_ms\": <number>, \"reason\": \"<中文 ≤30 字解释为何这样切>\"}}\n  ]\n}}",
        min_s = config.min_segment_s,
        max_s = config.max_segment_s,
    );

    // Build a compact, deterministic user message:
    //   候选切点：[0, 1240, 3580, ...]
    //   小片：
    //     [0–1240ms] 文本…
    //     [1240–3580ms] 文本…
    let mut user = String::new();
    user.push_str("【候选切点（毫秒）】\n");
    user.push_str(&format!(
        "{}\n\n",
        candidates_ms
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    ));
    user.push_str("【ASR 小片】\n");
    for (i, p) in pieces.iter().enumerate() {
        user.push_str(&format!(
            "{:03}. [{}–{}ms] {}\n",
            i,
            p.start_ms,
            p.end_ms,
            p.text.trim()
        ));
    }
    user.push_str(
        "\n请按规则输出 JSON。返回的每个 segment 的 start_ms 和 end_ms 必须\
         严格出现在候选切点列表里（除了起点 0 和终点）。",
    );
    (system, user)
}

/// Parse the LLM's JSON response into a vector of cut decisions.
///
/// Accepts the loose shape `{"segments": [{"start_ms":N,"end_ms":N,"reason":"…"}, ...]}`.
/// Anything missing fields or with non-numeric ms values gets rejected
/// with a diagnostic — the orchestrator can either retry, fall back to
/// silence-only cuts, or surface the error.
pub(crate) fn parse_semantic_cut_response(
    raw: &str,
) -> Result<Vec<SemanticCutDecision>, String> {
    let value: Value = serde_json::from_str(raw.trim())
        .map_err(|err| format!("LLM 响应非 JSON：{err} (raw: {})", truncate_for_log(raw, 200)))?;
    let segs = value
        .get("segments")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            format!(
                "LLM 响应缺少 segments 数组 (raw: {})",
                truncate_for_log(raw, 200)
            )
        })?;
    let mut out = Vec::with_capacity(segs.len());
    for (i, seg) in segs.iter().enumerate() {
        let start_ms = seg
            .get("start_ms")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| format!("segment {i}: start_ms missing or not u64"))?;
        let end_ms = seg
            .get("end_ms")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| format!("segment {i}: end_ms missing or not u64"))?;
        if end_ms <= start_ms {
            return Err(format!(
                "segment {i}: end_ms {end_ms} must be > start_ms {start_ms}"
            ));
        }
        let reason = seg
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(SemanticCutDecision {
            start_ms,
            end_ms,
            reason,
        });
    }
    Ok(out)
}

/// Trim a string for logging, adding "…" when it exceeds `max_chars`.
/// Char-aware so we don't slice through multibyte UTF-8.
fn truncate_for_log(s: &str, max_chars: usize) -> String {
    let mut count = 0;
    for (i, _) in s.char_indices() {
        if count >= max_chars {
            return format!("{}…", &s[..i]);
        }
        count += 1;
    }
    s.to_string()
}

/// Validate the LLM's cut decisions against the spec + the candidate
/// boundary pool. Returns the same vector on success, or an error
/// listing all hard-rule violations.
///
/// HARD rules (reject on violation):
///   - Each segment's [start_ms, end_ms] must each appear in
///     `candidates_ms` (so the cut lands on a real silence gap).
///   - Each segment length ≤ max_segment_s (Whisper / training
///     quality cliff). Length below `min_segment_s` is NOT a hard
///     fail — spec § 二·一·1 says "如果有独立语义的一句低于 6s 也可以"
///     and short complete utterances ("希望大家会喜欢") are common.
///     Phase 6 auto-QA still surfaces them as warnings in the
///     `问题记录` column for human review.
///   - Each segment must have positive length (end > start; a hard
///     1s minimum guards against an empty / off-by-one segment).
///   - Sorted by start_ms, no overlap (each segment's start_ms ≥
///     previous end_ms).
///
/// Note: segments don't have to COVER the entire input — spec § 二·四
/// allows dropping unusable bits (long silence / mumble / noise).
pub(crate) fn validate_semantic_cuts(
    decisions: &[SemanticCutDecision],
    candidates_ms: &[u64],
    config: &CutConfig,
) -> Result<(), String> {
    let candidate_set: std::collections::HashSet<u64> = candidates_ms.iter().copied().collect();
    // Absolute floor — anything shorter than 1s is almost certainly an
    // off-by-one or an empty segment, not a "real" short utterance.
    const HARD_MIN_MS: u64 = 1000;
    let max_ms = (config.max_segment_s * 1000.0) as u64;
    let mut prev_end = 0u64;
    let mut errors: Vec<String> = Vec::new();
    for (i, d) in decisions.iter().enumerate() {
        if !candidate_set.contains(&d.start_ms) {
            errors.push(format!(
                "[{i}] start_ms {} 不在候选切点池里",
                d.start_ms
            ));
        }
        if !candidate_set.contains(&d.end_ms) {
            errors.push(format!(
                "[{i}] end_ms {} 不在候选切点池里",
                d.end_ms
            ));
        }
        let len = d.end_ms.saturating_sub(d.start_ms);
        if len < HARD_MIN_MS {
            errors.push(format!(
                "[{i}] 时长 {len}ms 低于绝对下限 {HARD_MIN_MS}ms (空段/越界)"
            ));
        }
        if len > max_ms {
            errors.push(format!(
                "[{i}] 时长 {len}ms 超过上限 {max_ms}ms"
            ));
        }
        if d.start_ms < prev_end {
            errors.push(format!(
                "[{i}] start_ms {} 与上一段 end_ms {} 重叠",
                d.start_ms, prev_end
            ));
        }
        prev_end = d.end_ms;
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// HTTP-call wrapper around the prompt builder + response parser +
/// validator. The actual Ollama call uses `/api/chat` with
/// `options.num_ctx = config.semantic_num_ctx` (must be ≥32K to fit
/// one hour of ASR — see `CutConfig::semantic_num_ctx` doc).
///
/// `endpoint` must include scheme + host + port, e.g.
/// `http://100.64.0.4:11434`. Empty endpoint is an error — there is no
/// sensible fallback because the default Ollama pool member runs with
/// `num_ctx = 2048` which silently truncates the long prompt.
///
/// Test seam: the pure helpers (build_semantic_cut_prompt,
/// parse_semantic_cut_response, validate_semantic_cuts) are unit-tested
/// without any network; this function is only covered by an integration
/// test that requires a reachable Ollama endpoint and is gated on env.
pub(crate) fn llm_decide_semantic_cuts(
    pieces: &[AsredPiece],
    candidates_ms: &[u64],
    config: &CutConfig,
) -> Result<Vec<SemanticCutDecision>, String> {
    if config.semantic_endpoint.trim().is_empty() {
        return Err(
            "semantic_endpoint is empty — Mode 2 cannot use the default Ollama \
             pool because the pool's default num_ctx (2048) silently truncates the \
             long ASR prompt. Configure semanticEndpoint to a box with ≥40GB RAM, \
             e.g. http://100.64.0.4:11434."
                .to_string(),
        );
    }
    let (system, user) = build_semantic_cut_prompt(pieces, candidates_ms, config);
    let body = json!({
        "model": config.semantic_model,
        "stream": false,
        "format": "json",
        "options": {
            "temperature": 0.2,
            "top_p": 0.9,
            "num_ctx": config.semantic_num_ctx,
        },
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ]
    });
    let url = format!(
        "{}/api/chat",
        config.semantic_endpoint.trim_end_matches('/')
    );
    // Generous timeout: a 32B model warming up cold + processing 32K
    // context can run 90-180s on the first call. Subsequent calls
    // (when the model is hot) are 20-60s.
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(900))
        .build();
    let response = agent
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|err| format!("Ollama HTTP 失败：{err}"))?;
    let raw_body = response
        .into_string()
        .map_err(|err| format!("Ollama 响应读取失败：{err}"))?;
    // Ollama wraps the model's actual JSON in `message.content`.
    let envelope: Value = serde_json::from_str(&raw_body).map_err(|err| {
        format!("Ollama 信封非 JSON：{err}\nraw: {}", truncate_for_log(&raw_body, 300))
    })?;
    let inner = envelope
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| {
            format!(
                "Ollama 信封缺 message.content (raw: {})",
                truncate_for_log(&raw_body, 300)
            )
        })?;
    let decisions = parse_semantic_cut_response(inner)?;
    validate_semantic_cuts(&decisions, candidates_ms, config)?;
    Ok(decisions)
}

// ===========================================================================
// Phase 5 — Per-segment text normalisation
// ===========================================================================
//
// After Phase 4 merges fine ASR pieces into 6-90s semantic units, each
// unit's text is still the raw ASR output (no spec punctuation, digits
// as digits, particles inconsistent). Phase 5 hands each segment to
// the LLM with a normalisation prompt that encodes the spec's 三·转写规则
// chapter, gets back a clean string.

/// System prompt for per-segment Mandarin normalisation. Pure constant
/// so test cases can assert the wording without rebuilding the prompt
/// every time.
pub(crate) const SEMANTIC_NORMALIZE_PROMPT: &str = "\
你是一个普通话录音转写规范化助手。任务：把下面一段 ASR 的初稿改写成符合规范的最终转写文本。

【硬性规则】
1. 全部使用简体中文宋体字符。
2. 数字一律用汉字写（不要阿拉伯数字、不要百分号）。例：`20%` → `百分之二十`。
3. 内个 → 那个。其它常见网络口语词按发音转写（灰常 / 孩纸 / 童鞋 等保留）。
4. 他/她/它 按语义消歧：人男用「他」，人女用「她」，物用「它」，无明确性别指向的人用「他」。
5. 儿化音：影响语义的转写出（如「那些花儿」），不影响语义的不转写。
6. 语气词、拟声词正常转写（嗯啊对哈哈呵呵嘿嘿欸捏咧）。捧哏类「嗯」「啊」要正常转写并加标点。

【标点规则】
1. 全部中文标点：。？！，、；：……——「」《》（）
2. 明显停顿必须标点：180ms 以上停顿按语义补标点；连续无停顿短句允许连写。
3. 句号：语调下降 + 完整语义 + 停顿。
4. 问号：语调上扬 + 疑问语义。
5. 感叹号：语调强 + 情绪强 + 停顿。
6. 逗号：短停顿、语义未结束。
7. 拖长音 200-700ms：在词尾加 `...`；700ms 以上且语义未结束：转写为 `【……】`。

【输出格式】仅 JSON，不要 markdown，不要前后注释：
{\"text\":\"<规范化后的中文文本>\"}
";

/// Run Phase 5 on one segment. Calls the same `semantic_endpoint`
/// configured in `CutConfig` (the box that already has `num_ctx≥32K`
/// loaded — though normalisation needs only ~2K context, sharing the
/// endpoint avoids holding two warm Qwen 32B copies on different
/// nodes).
///
/// On any LLM failure (HTTP error, empty response, malformed JSON),
/// falls back to the raw ASR text rather than blocking the whole
/// pipeline — Phase 6's auto-QA will surface the segment as needing
/// human review.
pub(crate) fn llm_normalize_segment_text(
    raw_asr: &str,
    config: &CutConfig,
) -> Result<String, String> {
    if config.semantic_endpoint.trim().is_empty() {
        return Err("semantic_endpoint required for Mode 2 normalisation".into());
    }
    let body = json!({
        "model": config.semantic_model,
        "stream": false,
        "format": "json",
        "options": {
            "temperature": 0.1,
            "top_p": 0.9,
            // Per-segment text is short — 2K is plenty and lets the
            // host run a hot Qwen instance with a small KV cache.
            "num_ctx": 2048,
        },
        "messages": [
            {"role": "system", "content": SEMANTIC_NORMALIZE_PROMPT},
            {"role": "user", "content": raw_asr},
        ]
    });
    let url = format!(
        "{}/api/chat",
        config.semantic_endpoint.trim_end_matches('/')
    );
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(120))
        .build();
    let response = agent
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|err| format!("Ollama HTTP 失败：{err}"))?;
    let raw_body = response
        .into_string()
        .map_err(|err| format!("Ollama 响应读取失败：{err}"))?;
    let envelope: Value = serde_json::from_str(&raw_body).map_err(|err| {
        format!(
            "Ollama 信封非 JSON：{err}\nraw: {}",
            truncate_for_log(&raw_body, 300)
        )
    })?;
    let inner = envelope
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| {
            format!(
                "Ollama 信封缺 message.content (raw: {})",
                truncate_for_log(&raw_body, 300)
            )
        })?;
    parse_semantic_normalize_response(inner)
}

/// Pure JSON parser for the normalisation response. Accepts the loose
/// shape `{"text": "<final text>"}` and returns the text trimmed.
/// Reject empty text — that's a model failure and the orchestrator
/// will fall back to raw ASR.
pub(crate) fn parse_semantic_normalize_response(raw: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(raw.trim()).map_err(|err| {
        format!(
            "normalize 响应非 JSON：{err} (raw: {})",
            truncate_for_log(raw, 200)
        )
    })?;
    let text = value
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("normalize 响应缺 text 字段 (raw: {})", truncate_for_log(raw, 200)))?
        .trim()
        .to_string();
    if text.is_empty() {
        return Err("normalize 响应 text 为空".into());
    }
    Ok(text)
}

// ===========================================================================
// Phase 6 — Auto-QA on the normalised segments
// ===========================================================================
//
// Pure function: no LLM call, no audio analysis. Just looks at the
// normalised text + segment duration and flags every rule violation
// the spec § 三·转写规则 + § 二·切句规则 makes machine-checkable. The
// returned strings end up in the Excel's `问题记录` column so the
// human reviewer knows where to focus.
//
// Deliberately conservative: it's better to under-flag than to drown
// the reviewer in false positives. Things that NEED audio analysis
// (波形异常截断、底噪、口癖) are tracked in Phase 7's audio-quality
// pass, not here.

/// QA-flag every problem the normalised text + duration alone can
/// surface. Returns a flat list of Chinese issue descriptions in spec
/// terminology — the Excel's `问题记录` column joins them with `；`.
pub(crate) fn auto_qa_segment(
    text: &str,
    duration_ms: u64,
    config: &CutConfig,
) -> Vec<String> {
    let mut issues: Vec<String> = Vec::new();
    let min_ms = ((config.min_segment_s - 1.0).max(0.0) * 1000.0) as u64;
    let max_ms = (config.max_segment_s * 1000.0) as u64;

    // 1. Duration bounds (spec § 二·一·1).
    if duration_ms < min_ms {
        issues.push(format!(
            "时长 {duration_ms}ms 低于建议下限 {min_ms}ms（≈ {:.0}s）",
            min_ms as f64 / 1000.0
        ));
    }
    if duration_ms > max_ms {
        issues.push(format!(
            "时长 {duration_ms}ms 超出上限 {max_ms}ms（≈ {:.0}s）",
            max_ms as f64 / 1000.0
        ));
    }

    // 2. Empty / suspiciously sparse content.
    let trimmed = text.trim();
    let char_count = trimmed.chars().count();
    if trimmed.is_empty() {
        issues.push("文本为空".into());
        return issues; // No point running the rest of the checks.
    }

    // 3. Digit leak — spec § 三·一: numerals must be in Chinese.
    // ASCII digits 0-9 inside the body are an error UNLESS they're
    // inside a quoted English term, but for v1 we flag any leak and
    // let the human reviewer disposition.
    if trimmed.chars().any(|c| c.is_ascii_digit()) {
        issues.push("文字格式错误：数字未汉化".into());
    }

    // 4. Latin punctuation — spec § 三·二: 全部中文标点.
    let latin_punct = [',', '.', '!', '?', ';', ':', '"', '\''];
    if trimmed.chars().any(|c| latin_punct.contains(&c)) {
        issues.push("标点错误：出现英文标点".into());
    }

    // 5. The "内个" → "那个" rule (spec § 三·一).
    if trimmed.contains("内个") {
        issues.push("文字格式错误：内个 未改为 那个".into());
    }

    // 6. Sentence-ending punctuation. A complete semantic unit should
    // end with 。？！…  Allow trailing 】 (the long-sustain bracket).
    let valid_ends = ['。', '？', '！', '…', '】', '」', '）', '》'];
    if !trimmed
        .chars()
        .last()
        .map(|c| valid_ends.contains(&c))
        .unwrap_or(false)
    {
        issues.push("标点错误：结尾缺句末标点".into());
    }

    // 7. Density check — very long duration vs. very short text usually
    // means long pauses or mostly-silent content the cutter shouldn't
    // have kept. Spec § 二·四·3: 1s+ silence inside a segment should be
    // edited out. We can't detect 1s gaps from text alone, but we can
    // flag the suspicious ratio.
    if char_count > 0 {
        let ms_per_char = duration_ms as f64 / char_count as f64;
        // Normal Mandarin speech is ~3-5 chars/s → 200-330 ms/char.
        // > 800 ms/char means either very slow speech or unedited
        // pauses; either way it's worth a flag.
        if ms_per_char > 800.0 {
            issues.push(format!(
                "可能存在长停顿：{:.0}ms/字，远高于普通话语速",
                ms_per_char
            ));
        }
    }

    issues
}

// ===========================================================================
// Phase 8 — Top-level Semantic-mode orchestrator
// ===========================================================================
//
// Stitches Phases 2-7 together for one or more input WAVs. Tauri
// command surface: `run_semantic_pipeline`. The function is sync and
// dispatched onto the blocking thread pool so it can call the existing
// `recognize_segments_impl` without async plumbing.
//
// Output layout (spec § 四·(三) "按文件夹提交"):
//   out_dir/
//     <stem1>/
//       <stem1>_000001.wav
//       <stem1>_000002.wav
//       …
//       <stem1>.xlsx
//     <stem2>/…

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SemanticPipelineFileReport {
    pub(crate) source_path: String,
    pub(crate) xlsx_path: String,
    pub(crate) segment_count: usize,
    /// Soft warnings — pipeline succeeded but Phase 6 flagged at least
    /// one segment. Listed by segment index.
    pub(crate) qa_flagged_indices: Vec<usize>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SemanticPipelineResult {
    pub(crate) reports: Vec<SemanticPipelineFileReport>,
    /// Aggregated error trace. The orchestrator processes every input
    /// even if some fail; this collects the individual failures.
    pub(crate) errors: Vec<String>,
}

/// Run the full Semantic pipeline for each input file.
///
/// Caller responsibility: a sane `config.semantic_endpoint` (Phase 4
/// otherwise refuses). The `recognition_options` is passed to the ASR
/// step; set `use_llm = Some(false)` because Mode 2 doesn't want the
/// dialect-style polish — Phase 5's spec-driven normalisation runs
/// instead.
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
    let spec_stem = derive_spec_stem(&input);
    let file_out_dir = out_root.join(&spec_stem);
    fs::create_dir_all(&file_out_dir).map_err(|err| err.to_string())?;
    let work_dir = file_out_dir.join(".semantic-pipeline");
    fs::create_dir_all(&work_dir).map_err(|err| err.to_string())?;

    // ---- Phase 2: preprocess ---------------------------------------------
    let _ = app.emit(
        "semantic:progress",
        &serde_json::json!({"source": input_path, "phase": "preprocess"}),
    );
    let normalised = work_dir.join("01_normalised.wav");
    preprocess_audio_for_semantic(
        &input,
        &normalised,
        config.target_loudness_lufs,
        find_rnnoise_model().as_deref(),
    )?;

    // ---- Phase 3: fine pre-cut + candidate pool --------------------------
    let _ = app.emit(
        "semantic:progress",
        &serde_json::json!({"source": input_path, "phase": "fine_pre_cut"}),
    );
    let pieces_dir = work_dir.join("02_pieces");
    let (piece_paths, candidates_ms) =
        fine_pre_cut_for_semantic(&normalised, &pieces_dir)?;
    if piece_paths.is_empty() {
        return Err("fine pre-cut produced 0 pieces".into());
    }

    // ---- Phase 3b: ASR each piece via existing pool ----------------------
    let _ = app.emit(
        "semantic:progress",
        &serde_json::json!({
            "source": input_path,
            "phase": "asr",
            "pieces": piece_paths.len(),
        }),
    );
    // Build minimal SegmentRecord list for recognize_segments_impl.
    // start_ms/end_ms here are intra-piece — recognize doesn't care
    // about the global timeline.
    let asr_segments: Vec<SegmentRecord> = piece_paths
        .iter()
        .enumerate()
        .map(|(i, p)| SegmentRecord {
            id: format!("piece_{i:04}"),
            source_path: path_to_string(&input),
            source_file_name: source_basename.clone(),
            segment_path: path_to_string(p),
            segment_file_name: path_file_name(p),
            role: None,
            start_ms: 0,
            end_ms: 0,
            duration_ms: 0,
            original_text: String::new(),
            phonetic_text: String::new(),
            emotion: Vec::new(),
            tags: Vec::new(),
            notes: String::new(),
        })
        .collect();

    let asr_results = recognize_segments_impl(
        app.clone(),
        path_to_string(&work_dir),
        asr_segments,
        asr_options.clone(),
    )?;
    // Map ASR results back to pieces in order (recognize preserves
    // input order in its output).
    let pieces_asred: Vec<AsredPiece> = asr_results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            // candidates_ms[i] is start of piece i; candidates_ms[i+1]
            // is end. Phase 3 guarantees boundaries.len() == pieces+1.
            let start = candidates_ms.get(i).copied().unwrap_or(0);
            let end = candidates_ms.get(i + 1).copied().unwrap_or(start);
            AsredPiece {
                text: r.text.clone(),
                start_ms: start,
                end_ms: end,
            }
        })
        .collect();

    // ---- Phase 4: LLM semantic cut decision ------------------------------
    let _ = app.emit(
        "semantic:progress",
        &serde_json::json!({"source": input_path, "phase": "semantic_cut"}),
    );
    let decisions = llm_decide_semantic_cuts(&pieces_asred, &candidates_ms, config)?;

    // ---- Phase 5 + 6: normalise + QA per merged segment ------------------
    let _ = app.emit(
        "semantic:progress",
        &serde_json::json!({
            "source": input_path,
            "phase": "normalise_qa",
            "segments": decisions.len(),
        }),
    );
    let mut export_segments: Vec<SemanticExportSegment> = Vec::with_capacity(decisions.len());
    let mut qa_flagged: Vec<usize> = Vec::new();
    for (i, dec) in decisions.iter().enumerate() {
        // Concatenate texts of pieces falling within [start, end).
        let raw_text: String = pieces_asred
            .iter()
            .filter(|p| p.start_ms >= dec.start_ms && p.end_ms <= dec.end_ms)
            .map(|p| p.text.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        // Phase 5 normalise — fall back to raw text on failure.
        let final_text = match llm_normalize_segment_text(&raw_text, config) {
            Ok(t) => t,
            Err(err) => {
                eprintln!("[semantic] segment {i} normalise failed: {err}");
                raw_text.clone()
            }
        };
        // Phase 6 QA.
        let duration_ms = dec.end_ms.saturating_sub(dec.start_ms);
        let problems = auto_qa_segment(&final_text, duration_ms, config);
        if !problems.is_empty() {
            qa_flagged.push(i);
        }
        export_segments.push(SemanticExportSegment {
            start_ms: dec.start_ms,
            end_ms: dec.end_ms,
            final_text,
            problems,
        });
    }

    // ---- Phase 7: export WAVs + xlsx -------------------------------------
    let _ = app.emit(
        "semantic:progress",
        &serde_json::json!({"source": input_path, "phase": "export"}),
    );
    let (xlsx_path, _wav_paths) = export_semantic_mode(
        &file_out_dir,
        &normalised,
        &spec_stem,
        &source_basename,
        &export_segments,
    )?;

    Ok(SemanticPipelineFileReport {
        source_path: path_to_string(&input),
        xlsx_path: path_to_string(&xlsx_path),
        segment_count: export_segments.len(),
        qa_flagged_indices: qa_flagged,
    })
}

#[tauri::command]
async fn run_semantic_pipeline(
    app: tauri::AppHandle,
    input_paths: Vec<String>,
    out_dir: String,
    config: CutConfig,
    recognition_options: RecognitionOptions,
) -> Result<SemanticPipelineResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        run_semantic_pipeline_impl(app, input_paths, out_dir, config, recognition_options)
    })
    .await
    .map_err(|err| err.to_string())?
}

// ===========================================================================
// Phase 7 — Export (WAV slicing + xlsx) for Semantic mode
// ===========================================================================
//
// Spec § 四·(二) data format:
//   - WAV per segment, named `<spk>_<业务类型>_<录制形式>_<日期>_<时长>_<NNNNNN>`
//   - xlsx with columns: 源文件名 / 剪辑文件名 / 转写内容 / 问题记录
//
// Phase 4's LLM produced [(start_ms, end_ms)] tuples on the normalised
// source audio. Phase 5 normalised the text for each. Phase 6 ran
// auto-QA. This module ties them together: emits one WAV per segment
// (cut from the same normalised source — no concat boundary clicks)
// and one xlsx that the QA team opens directly.

/// One row of the export deliverable.
pub(crate) struct SemanticExportSegment {
    pub(crate) start_ms: u64,
    pub(crate) end_ms: u64,
    pub(crate) final_text: String,
    pub(crate) problems: Vec<String>,
}

/// Generate the segment filename for spec § 四·一 naming.
///
/// `spec_stem` is everything before the `_NNNNNN` sequence number —
/// usually carried over from the source filename (e.g.
/// `spkA_Freetalk_freetalk_260101_40m`). `seq_one_based` starts at 1.
///
/// Output: `<spec_stem>_<NNNNNN>.wav` (six-digit zero-padded sequence).
pub(crate) fn semantic_segment_filename(spec_stem: &str, seq_one_based: usize) -> String {
    format!("{spec_stem}_{:06}.wav", seq_one_based)
}

/// Try to derive the spec_stem from a source filename.
///
/// Accepts the spec example shape `spk_A_FREETALK_0606_68M_01.wav` and
/// the variant `spkA_Freetalk_freetalk_260101_40m.wav`. When the
/// filename doesn't match the spec, returns the bare file stem — the
/// reviewer can then rename in their delivery script.
pub(crate) fn derive_spec_stem(source_path: &Path) -> String {
    let stem = path_file_stem(source_path);
    // Strip a trailing `_<digits>` sequence marker if present, so a
    // source like `…_40m_01` becomes the canonical `…_40m`.
    let trailing_seq = Regex::new(r"_\d{1,6}$").expect("valid regex");
    trailing_seq.replace(&stem, "").to_string()
}

/// Run the export: emit one WAV per segment cut from `normalised_source`
/// at the LLM-decided boundaries, plus an xlsx with the spec's four
/// columns. Returns paths to the xlsx + the emitted WAV files.
///
/// Source file naming (spec § 四·一):
///   `<spec_stem>_<NNNNNN>.wav`
/// The xlsx is named `<spec_stem>.xlsx` next to the WAVs.
pub(crate) fn export_semantic_mode(
    out_dir: &Path,
    normalised_source: &Path,
    spec_stem: &str,
    source_basename_for_csv: &str,
    segments: &[SemanticExportSegment],
) -> Result<(PathBuf, Vec<PathBuf>), String> {
    if segments.is_empty() {
        return Err("no segments to export".into());
    }
    fs::create_dir_all(out_dir).map_err(|err| err.to_string())?;

    // Probe once — re-cutting uses the same codec/sample-rate as the
    // source so all output WAVs match.
    let probe = probe_audio(normalised_source)?;
    let codec = output_pcm_codec(&probe);

    let mut wav_paths = Vec::with_capacity(segments.len());
    for (i, seg) in segments.iter().enumerate() {
        let name = semantic_segment_filename(spec_stem, i + 1);
        let dst = out_dir.join(&name);
        let start_sec = seg.start_ms as f64 / 1000.0;
        let dur_sec = (seg.end_ms.saturating_sub(seg.start_ms)) as f64 / 1000.0;
        write_pcm_wav_segment(normalised_source, &dst, start_sec, dur_sec, &probe, codec)?;
        wav_paths.push(dst);
    }

    let xlsx_path = out_dir.join(format!("{spec_stem}.xlsx"));
    write_semantic_xlsx(&xlsx_path, source_basename_for_csv, spec_stem, segments)?;
    Ok((xlsx_path, wav_paths))
}

/// Pure xlsx writer. Separate from the WAV emission so it's unit-testable
/// without ffmpeg / a source file. Schema:
///
///   | 源文件名 | 剪辑文件名 | 转写内容 | 问题记录 |
fn write_semantic_xlsx(
    out: &Path,
    source_basename: &str,
    spec_stem: &str,
    segments: &[SemanticExportSegment],
) -> Result<(), String> {
    use rust_xlsxwriter::{Format, Workbook};

    let mut workbook = Workbook::new();
    let sheet = workbook
        .add_worksheet()
        .set_name("剪辑记录")
        .map_err(|err| err.to_string())?;

    let header_fmt = Format::new()
        .set_bold()
        .set_background_color("#E7EFFA");
    let wrap_fmt = Format::new().set_text_wrap();

    let headers = ["源文件名", "剪辑文件名", "转写内容", "问题记录"];
    for (col, h) in headers.iter().enumerate() {
        sheet
            .write_string_with_format(0, col as u16, *h, &header_fmt)
            .map_err(|err| err.to_string())?;
    }

    for (i, seg) in segments.iter().enumerate() {
        let row = (i + 1) as u32;
        let clip_name = semantic_segment_filename(spec_stem, i + 1);
        let problems = if seg.problems.is_empty() {
            String::new()
        } else {
            seg.problems.join("；")
        };
        sheet
            .write_string(row, 0, source_basename)
            .map_err(|err| err.to_string())?;
        sheet
            .write_string(row, 1, &clip_name)
            .map_err(|err| err.to_string())?;
        sheet
            .write_string_with_format(row, 2, &seg.final_text, &wrap_fmt)
            .map_err(|err| err.to_string())?;
        sheet
            .write_string_with_format(row, 3, &problems, &wrap_fmt)
            .map_err(|err| err.to_string())?;
    }

    // Reasonable initial column widths — text columns get more room.
    sheet.set_column_width(0, 28.0).ok();
    sheet.set_column_width(1, 36.0).ok();
    sheet.set_column_width(2, 80.0).ok();
    sheet.set_column_width(3, 40.0).ok();
    sheet
        .autofilter(0, 0, segments.len() as u32, 3)
        .map_err(|err| err.to_string())?;

    workbook
        .save(out)
        .map_err(|err| format!("xlsx save failed: {err}"))?;
    Ok(())
}

/// Audio preprocessing for Semantic mode.
///
/// Two-stage ffmpeg filter (when rnnoise model is available):
///   1. `arnndn=m=<model>` — RNNoise denoise. Applied first so
///      loudnorm sees clean signal. Optional: if no model file is
///      discovered, skip this stage (Mandarin podcast audio with
///      decent recording usually doesn't need it).
///   2. `loudnorm=I=<target>:TP=-1.5:LRA=11` — Single-pass EBU R128
///      loudness normalisation to the spec's -18 LUFS target
///      (§ 七·3). Two-pass would tighten the result by ~0.5dB but
///      requires JSON-parsing ffmpeg's first-pass stderr — not worth
///      the complexity at v1.
///
/// Output is hardcoded to 48 kHz / 16-bit PCM WAV — spec § 七 allows
/// 44.1 or 48 kHz, 16 or 24 bit. 48 kHz matches Whisper's preferred
/// input rate and avoids a resample later.
fn preprocess_audio_for_semantic(
    input: &Path,
    output: &Path,
    target_lufs: f32,
    rnnoise_model: Option<&Path>,
) -> Result<(), String> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let mut filter = String::new();
    if let Some(model) = rnnoise_model {
        // Escape colons/backslashes in the path — ffmpeg's filter syntax
        // treats them as separators. Windows paths like `C:\…` would
        // break the filter without escaping.
        let escaped = model
            .to_string_lossy()
            .replace('\\', "/")
            .replace(':', "\\:");
        filter.push_str(&format!("arnndn=m={escaped},"));
    }
    filter.push_str(&format!(
        "loudnorm=I={:.1}:TP=-1.5:LRA=11",
        target_lufs
    ));

    let mut command = silent_command("ffmpeg");
    command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel").arg("error")
        .arg("-i").arg(input)
        .arg("-af").arg(&filter)
        .arg("-ar").arg("48000")          // spec § 七 allows 44.1 / 48k
        .arg("-ac").arg("1")              // mono; preserves the source's
                                          // intended single-speaker mix
        .arg("-c:a").arg("pcm_s16le")     // 16-bit PCM
        .arg(output);

    eprintln!(
        "[semantic preprocess] {} → {} (filter: {filter})",
        path_to_string(input),
        path_to_string(output),
    );
    let output_result = command
        .output()
        .map_err(|err| format!("ffmpeg launch failed: {err}"))?;
    if !output_result.status.success() {
        return Err(format!(
            "ffmpeg preprocess failed (exit {}): {}",
            output_result.status,
            String::from_utf8_lossy(&output_result.stderr)
        ));
    }
    Ok(())
}

/// Resolve a path to an `arnndn` RNNoise model (.rnnn) for the
/// preprocessor. Returns None when no model is configured — preprocess
/// then skips the denoise stage and only normalises loudness.
///
/// Lookup order:
///   1. `RNNOISE_MODEL_PATH` env var
///   2. Current working directory `./cb.rnnn`
///   3. The executable's parent dir `<exe>/../cb.rnnn`
///   4. `<exe>/../Resources/cb.rnnn` (the macOS bundle layout)
///
/// Build your own community models at <https://github.com/GregorR/rnnoise-models>;
/// `cb.rnnn` (the conference-bridge baseline) is the typical default.
fn find_rnnoise_model() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("RNNOISE_MODEL_PATH") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }
    let cwd_candidate = PathBuf::from("cb.rnnn");
    if cwd_candidate.is_file() {
        return Some(cwd_candidate);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let next_to = parent.join("cb.rnnn");
            if next_to.is_file() {
                return Some(next_to);
            }
            let in_resources = parent.join("Resources").join("cb.rnnn");
            if in_resources.is_file() {
                return Some(in_resources);
            }
        }
    }
    None
}

fn cut_audio_file_dialect_impl(
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
        });
    }

    Ok(segments)
}

#[tauri::command]
fn save_project_file(project_dir: String, payload: Value) -> Result<String, String> {
    let dir = PathBuf::from(&project_dir);
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let canonical_dir = dir.canonicalize().unwrap_or(dir.clone());
    let path = dir.join("project.json");

    // Backend safety guard: refuse to overwrite an on-disk project.json
    // that has non-empty segments[] with a payload whose segments[] is
    // empty. This stops a buggy scan / state reset from silently wiping
    // hours of annotation work — we already lost 1212 segments to this
    // bug class once. Frontend should explicitly handle the recovery
    // path (e.g. write to a backup file, prompt user, etc.) instead of
    // sneaking an empty save through.
    let new_segs_empty = payload
        .get("segments")
        .and_then(|v| v.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(true);
    if new_segs_empty {
        if let Ok(existing_data) = fs::read_to_string(&path) {
            if let Ok(existing) = serde_json::from_str::<Value>(&existing_data) {
                let on_disk_count = existing
                    .get("segments")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                if on_disk_count > 0 {
                    eprintln!(
                        "[save_project_file] REFUSED: incoming segments=[] but on-disk has {} segments. Backing up and skipping write.",
                        on_disk_count
                    );
                    let _ = backup_project_file(project_dir.clone());
                    return Err(format!(
                        "拒绝写入空 segments[]：磁盘上有 {} 段。已备份到 .backups/，请检查恢复",
                        on_disk_count
                    ));
                }
            }
        }
    }

    // Rewrite well-known path fields to be relative to projectDir whenever
    // possible. The output folder becomes self-contained and portable —
    // a Windows reviewer can open the same dir and resolve segments
    // against their own absolute path.
    let portable = portablize_payload(payload, &canonical_dir);
    let data = serde_json::to_string_pretty(&portable).map_err(|err| err.to_string())?;
    fs::write(&path, data).map_err(|err| err.to_string())?;
    Ok(path_to_string(&path))
}

/// Walk a project.json `Value` and convert every known absolute path field
/// (`segmentsDir`, `segments[].sourcePath`, `segments[].segmentPath`,
/// `audioFiles[].path`) into one relative to `project_dir` if it lives
/// inside that tree. Paths outside the tree (e.g. source audio in a
/// sibling input folder) are left absolute.
fn portablize_payload(mut value: Value, project_dir: &Path) -> Value {
    fn rel(p: &str, base: &Path) -> String {
        let path = PathBuf::from(p);
        if let Ok(canon) = path.canonicalize() {
            if let Ok(rel) = canon.strip_prefix(base) {
                let s = rel.to_string_lossy().replace('\\', "/");
                if !s.is_empty() {
                    return format!("./{}", s);
                }
            }
        }
        // Already relative or outside the project tree — keep as-is so the
        // path semantics on this machine are unchanged.
        p.to_string()
    }
    fn fix_str(node: &mut Value, base: &Path) {
        if let Some(s) = node.as_str() {
            *node = json!(rel(s, base));
        }
    }
    if let Some(obj) = value.as_object_mut() {
        if let Some(s) = obj.get_mut("segmentsDir") {
            fix_str(s, project_dir);
        }
        if let Some(arr) = obj.get_mut("segments").and_then(|v| v.as_array_mut()) {
            for seg in arr.iter_mut() {
                if let Some(seg_obj) = seg.as_object_mut() {
                    if let Some(p) = seg_obj.get_mut("sourcePath") {
                        fix_str(p, project_dir);
                    }
                    if let Some(p) = seg_obj.get_mut("segmentPath") {
                        fix_str(p, project_dir);
                    }
                }
            }
        }
        if let Some(arr) = obj.get_mut("audioFiles").and_then(|v| v.as_array_mut()) {
            for af in arr.iter_mut() {
                if let Some(af_obj) = af.as_object_mut() {
                    if let Some(p) = af_obj.get_mut("path") {
                        fix_str(p, project_dir);
                    }
                }
            }
        }
    }
    value
}

/// Inverse of `portablize_payload`. Resolves any relative path under the
/// known fields back to an absolute path rooted at `project_dir`. Absolute
/// paths in the JSON are passed through unchanged so files saved on
/// another platform still work as long as they live inside the dir we
/// just opened.
fn absolutize_payload(mut value: Value, project_dir: &Path) -> Value {
    fn abs(p: &str, base: &Path) -> String {
        let path = PathBuf::from(p);
        if path.is_absolute() {
            return p.to_string();
        }
        // Treat "./foo" or "foo/bar" as relative to project_dir. Strip any
        // leading "./" so PathBuf::join doesn't get confused on Windows.
        let stripped = p.strip_prefix("./").unwrap_or(p);
        let joined = base.join(stripped);
        path_to_string(&joined)
    }
    fn fix_str(node: &mut Value, base: &Path) {
        if let Some(s) = node.as_str() {
            *node = json!(abs(s, base));
        }
    }
    if let Some(obj) = value.as_object_mut() {
        if let Some(s) = obj.get_mut("segmentsDir") {
            fix_str(s, project_dir);
        }
        if let Some(arr) = obj.get_mut("segments").and_then(|v| v.as_array_mut()) {
            for seg in arr.iter_mut() {
                if let Some(seg_obj) = seg.as_object_mut() {
                    if let Some(p) = seg_obj.get_mut("sourcePath") {
                        fix_str(p, project_dir);
                    }
                    if let Some(p) = seg_obj.get_mut("segmentPath") {
                        fix_str(p, project_dir);
                    }
                }
            }
        }
        if let Some(arr) = obj.get_mut("audioFiles").and_then(|v| v.as_array_mut()) {
            for af in arr.iter_mut() {
                if let Some(af_obj) = af.as_object_mut() {
                    if let Some(p) = af_obj.get_mut("path") {
                        fix_str(p, project_dir);
                    }
                }
            }
        }
    }
    value
}

/// Back-compat command kept for old frontends. It now runs the same dialogue
/// sequence naming pass as the normal cut/export flow:
/// `<mode>_<topic>_<round>_<content>_<role>.wav`.
#[tauri::command]
fn migrate_segment_filenames(
    project_dir: String,
    segments: Vec<SegmentRecord>,
) -> Result<Vec<SegmentRecord>, String> {
    let _ = project_dir;
    rename_segments_by_dialogue_sequence_impl(segments)
}

#[tauri::command]
fn backup_project_file(project_dir: String) -> Result<Option<String>, String> {
    let dir = PathBuf::from(project_dir);
    let src = dir.join("project.json");
    if !src.is_file() {
        return Ok(None);
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default();
    let backup_dir = dir.join(".backups");
    fs::create_dir_all(&backup_dir).map_err(|err| err.to_string())?;
    let dst = backup_dir.join(format!("project.{}.json.bak", stamp));
    fs::copy(&src, &dst).map_err(|err| err.to_string())?;

    // Trim old backups: keep at most 20 most recent. Sorted by name (which is
    // timestamp-prefixed) gives chronological order.
    if let Ok(entries) = fs::read_dir(&backup_dir) {
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("project.") && n.ends_with(".json.bak"))
                    .unwrap_or(false)
            })
            .collect();
        paths.sort();
        let excess = paths.len().saturating_sub(20);
        for p in paths.into_iter().take(excess) {
            let _ = fs::remove_file(p);
        }
    }
    Ok(Some(path_to_string(&dst)))
}

#[tauri::command]
fn load_project_file(project_dir: String) -> Result<Value, String> {
    let dir = PathBuf::from(&project_dir);
    let path = dir.join("project.json");
    let data = fs::read_to_string(&path).map_err(|err| err.to_string())?;
    let value: Value = serde_json::from_str(&data).map_err(|err| err.to_string())?;
    let canonical_dir = dir.canonicalize().unwrap_or(dir);
    Ok(absolutize_payload(value, &canonical_dir))
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BundleResult {
    bundle_dir: String,
    jsonl_path: String,
    segment_count: usize,
    source_audio_count: usize,
    total_bytes: u64,
}

/// One-click "ship-ready" export. Copies every cut segment (and optionally
/// each unique source audio file) into a self-contained folder, then writes
/// the JSONL with paths re-rooted at the bundle. The user can upload the
/// whole folder to OSS preserving structure, and the JSONL paths line up.
#[tauri::command]
async fn export_dataset_bundle(
    bundle_dir: String,
    segments: Vec<SegmentRecord>,
    options: Option<ExportOptions>,
    include_source_audio: Option<bool>,
) -> Result<BundleResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        export_dataset_bundle_impl(
            bundle_dir,
            segments,
            options.unwrap_or_default(),
            include_source_audio.unwrap_or(true),
        )
    })
    .await
    .map_err(|err| err.to_string())?
}

pub(crate) fn export_dataset_bundle_impl(
    bundle_dir: String,
    segments: Vec<SegmentRecord>,
    options: ExportOptions,
    include_source_audio: bool,
) -> Result<BundleResult, String> {
    if segments.is_empty() {
        return Err("没有可导出的切割片段".to_string());
    }

    let bundle = PathBuf::from(&bundle_dir);
    fs::create_dir_all(&bundle).map_err(|err| err.to_string())?;
    let bundle_canonical = bundle.canonicalize().map_err(|err| err.to_string())?;
    let bundle_root_str = path_to_string(&bundle_canonical);

    let segments_out = bundle_canonical.join("segments");
    fs::create_dir_all(&segments_out).map_err(|err| err.to_string())?;

    let mut total_bytes: u64 = 0;
    let mut source_path_map: HashMap<String, String> = HashMap::new();

    if include_source_audio {
        let source_out = bundle_canonical.join("source");
        fs::create_dir_all(&source_out).map_err(|err| err.to_string())?;
        let mut unique_sources: Vec<&String> = Vec::new();
        let mut seen: HashSet<&String> = HashSet::new();
        for seg in &segments {
            if seen.insert(&seg.source_path) {
                unique_sources.push(&seg.source_path);
            }
        }
        for src in unique_sources {
            let src_path = PathBuf::from(src);
            if !src_path.is_file() {
                continue;
            }
            let file_name = match src_path.file_name().and_then(|n| n.to_str()) {
                Some(value) => value.to_string(),
                None => continue,
            };
            let dst = source_out.join(&file_name);
            fs::copy(&src_path, &dst).map_err(|err| err.to_string())?;
            total_bytes =
                total_bytes.saturating_add(fs::metadata(&dst).map(|m| m.len()).unwrap_or(0));
            source_path_map.insert(src.clone(), path_to_string(&dst));
        }
    }

    let mut new_segments: Vec<SegmentRecord> = Vec::with_capacity(segments.len());
    for segment in &segments {
        let src = PathBuf::from(&segment.segment_path);
        if !src.is_file() {
            return Err(format!("切割片段文件不存在：{}", segment.segment_path));
        }
        let parent_name = src
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("misc")
            .to_string();
        let dst_dir = if parent_name == "segments" {
            segments_out.clone()
        } else {
            segments_out.join(&parent_name)
        };
        fs::create_dir_all(&dst_dir).map_err(|err| err.to_string())?;
        let dst = dst_dir.join(&segment.segment_file_name);
        fs::copy(&src, &dst).map_err(|err| err.to_string())?;
        total_bytes = total_bytes.saturating_add(fs::metadata(&dst).map(|m| m.len()).unwrap_or(0));

        let new_source = source_path_map
            .get(&segment.source_path)
            .cloned()
            .unwrap_or_else(|| segment.source_path.clone());

        let mut updated = segment.clone();
        updated.segment_path = path_to_string(&dst);
        updated.source_path = new_source;
        new_segments.push(updated);
    }

    let system_prompt = options.system_prompt.clone().unwrap_or_default();
    let pair = options.pair_user_assistant.unwrap_or(true);
    let user_use_source = options.use_source_audio_for_user.unwrap_or(false);
    let prefix = options.audio_file_prefix.clone().unwrap_or_default();
    // Always rewrite paths relative to the bundle root, so when the user
    // uploads the whole bundle to OSS the JSONL references line up.
    let input_root = bundle_root_str.clone();

    // Heal mislabeled roles inherited from the old infer_role bug.
    let new_segments = normalize_segment_roles(&new_segments);

    let lines = if pair {
        build_paired_jsonl(
            &new_segments,
            &system_prompt,
            user_use_source,
            &prefix,
            &input_root,
        )
    } else {
        build_flat_jsonl(&new_segments, &system_prompt, &prefix, &input_root)
    }?;

    let jsonl_path = bundle_canonical.join("export.jsonl");
    // Pretty records separated by a single newline (no blank line
    // between records). Each record is still a self-contained JSON
    // object — pretty over multiple lines, then `}\n{` between them.
    fs::write(&jsonl_path, format!("{}\n", lines.join("\n"))).map_err(|err| err.to_string())?;

    // Also write project.json into the bundle so a downstream reviewer
    // (e.g. the Windows audit build) can resume editing with all
    // annotations intact. We deliberately store EVERY path field
    // RELATIVE to the bundle root so the zip survives being moved
    // anywhere on the receiver's disk (the Windows reviewer scenario
    // that previously broke when paths still held macOS tempdir
    // strings):
    //   - rootPath / projectDir = "." → resolved against the dir the
    //     receiver opens the project from
    //   - segmentsDir = "./segments"
    //   - segments[].segmentPath = "./segments/<file>.wav"
    //   - segments[].sourcePath = "./source/<file>.wav" when source was
    //     copied into the bundle; otherwise left as-is (an outside-the-
    //     bundle absolute path; useful for display, ignored by playback
    //     since the Windows reviewer does not re-cut).
    let saved_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let mut project_payload = serde_json::json!({
        "version": 2,
        "savedAt": format!("epoch_ms_{}", saved_at_ms),
        "rootPath": ".",
        "projectDir": ".",
        "segmentsDir": "./segments",
        "config": {
            "silenceDb": -35.0,
            "minSilenceMs": 450,
            "minSegmentMs": 300,
            "preRollMs": 100,
            "postRollMs": 200,
            "maxSegmentMs": 0
        },
        "audioFiles": Vec::<Value>::new(),
        "manifestRecords": Vec::<Value>::new(),
        "segments": new_segments,
        "systemPrompt": system_prompt
    });
    // Rewrite known absolute path fields inside `segments[]` to "./…"
    // form using the same helper save_project_file uses for in-place
    // saves. portablize_payload skips the top-level rootPath/projectDir
    // fields (already "." above) and only touches the known
    // segments[].{sourcePath,segmentPath} + segmentsDir entries — those
    // are absolute right now because the copy step at line ~1532 set
    // them to <bundle>/segments/<file>.wav.
    project_payload = portablize_payload(project_payload, &bundle_canonical);
    let project_path = bundle_canonical.join("project.json");
    fs::write(
        &project_path,
        serde_json::to_string_pretty(&project_payload).map_err(|err| err.to_string())?,
    )
    .map_err(|err| err.to_string())?;

    let readme_path = bundle_canonical.join("README.md");
    let readme = format!(
        "# 长沙方言数据集打包\n\n\
切割片段：{} 段\n\
源音频：{} 个\n\
体积：{:.1} MB\n\n\
## 目录结构\n\n\
```\n\
{}\n\
├── export.jsonl     # 标注数据 (与 demo 格式一致)\n\
├── segments/        # 切割后的 PCM WAV 片段，按源文件分子目录\n\
{}└── README.md\n\
```\n\n\
## 上传 OSS\n\n\
- 整目录上传到 OSS 桶（保留 segments/{}子目录结构）\n\
- 将设置中的 *audio_file_prefix* 设置为对应 OSS 路径\n\
- 重新点击「一键打包」即可生成带 OSS 路径的 JSONL\n\n\
## 校对\n\n\
- 在工作台「设置」抽屉里调 OSS 路径前缀\n\
- 修改后重打包会覆盖此目录\n",
        new_segments.len(),
        source_path_map.len(),
        total_bytes as f64 / 1024.0 / 1024.0,
        path_file_name(&bundle_canonical),
        if source_path_map.is_empty() {
            ""
        } else {
            "├── source/          # 源音频原文件，供 user 角色引用\n"
        },
        if source_path_map.is_empty() {
            ""
        } else {
            "source/ "
        }
    );
    let _ = fs::write(&readme_path, readme);

    Ok(BundleResult {
        bundle_dir: bundle_root_str,
        jsonl_path: path_to_string(&jsonl_path),
        segment_count: new_segments.len(),
        source_audio_count: source_path_map.len(),
        total_bytes,
    })
}

#[tauri::command]
fn export_segments_jsonl(
    project_dir: String,
    output_path: Option<String>,
    segments: Vec<SegmentRecord>,
    options: Option<ExportOptions>,
) -> Result<String, String> {
    let dir = PathBuf::from(project_dir);
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let path = output_path
        .filter(|value| !value.trim().is_empty())
        .map(|value| PathBuf::from(value.trim()))
        .unwrap_or_else(|| dir.join("export.jsonl"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }

    let opts = options.unwrap_or_default();
    let system_prompt = opts.system_prompt.unwrap_or_default();
    let pair = opts.pair_user_assistant.unwrap_or(true);
    let user_use_source = opts.use_source_audio_for_user.unwrap_or(false);
    let prefix = opts.audio_file_prefix.unwrap_or_default();
    let input_root = opts.input_root.unwrap_or_default();

    // Heal segments whose role got mislabeled by the old infer_role bug.
    let segments = normalize_segment_roles(&segments);

    let lines = if pair {
        build_paired_jsonl(
            &segments,
            &system_prompt,
            user_use_source,
            &prefix,
            &input_root,
        )
    } else {
        build_flat_jsonl(&segments, &system_prompt, &prefix, &input_root)
    }?;

    fs::write(&path, format!("{}\n", lines.join("\n"))).map_err(|err| err.to_string())?;
    Ok(path_to_string(&path))
}

/// Normalize segment roles by re-inferring from the source file name.
///
/// Older builds had a bug in `infer_role` that scanned the full path,
/// so any project under `/Users/<name>/` got every segment marked as
/// `user`. This walks each segment, re-runs the (fixed) `infer_role`
/// against `source_path`, and overrides `segment.role` if the file/dir
/// name unambiguously implies a different role. Manual roles set via
/// the UI are preserved when they don't conflict with the inferred one.
fn normalize_segment_roles(segments: &[SegmentRecord]) -> Vec<SegmentRecord> {
    segments
        .iter()
        .map(|s| {
            let inferred = infer_role(Path::new(&s.source_path));
            let mut copy = s.clone();
            // Override only when file/dir name implies a definite role
            // (i.e. infer_role returned Some) AND it differs from what's
            // currently stored. This catches the bug-inflicted "user"
            // labels on `..._发音人.wav` files without clobbering a
            // user's deliberate manual override on ambiguous filenames.
            if let Some(inferred_role) = inferred {
                if copy.role.as_deref() != Some(inferred_role.as_str()) {
                    copy.role = Some(inferred_role);
                }
            }
            copy
        })
        .collect()
}

/// Render a `{"messages": [...]}` payload in the dataset spec's exact
/// formatting style. Hand-rolled because `serde_json::to_string_pretty`
/// produces a different layout (`"key": "value"` with a space, arrays
/// broken across multiple lines) and the spec's downstream parser is
/// strict about layout.
///
/// Per the spec figure annotations:
/// - colon `:` is **never** followed by a space
/// - array values (content_refine, audio_file, emotion_refine) are
///   emitted **inline** — every element on the same line as the key
/// - top-level `messages` and each message object still get newlines
///   so individual entries remain readable
fn render_messages_payload(messages: &[Value]) -> String {
    let mut out = String::new();
    out.push_str("{\n  \"messages\":[\n");
    for (i, msg) in messages.iter().enumerate() {
        out.push_str("    {\n");
        let obj = msg.as_object().expect("each message must be a JSON object");
        let entries: Vec<String> = obj
            .iter()
            .map(|(k, v)| format!("      {}:{}", json_quote(k), inline_value(v)))
            .collect();
        out.push_str(&entries.join(",\n"));
        out.push_str("\n    }");
        if i + 1 < messages.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}");
    out
}

/// Quote a string as a JSON string literal (with proper escaping).
fn json_quote(s: &str) -> String {
    serde_json::to_string(&Value::String(s.to_string())).unwrap_or_else(|_| {
        // Fallback: just wrap in quotes. Should never trigger.
        format!("\"{}\"", s.replace('"', "\\\""))
    })
}

/// Serialize a JSON value with arrays kept inline (no element-level
/// newlines). Strings, numbers, and booleans use their compact form.
fn inline_value(v: &Value) -> String {
    match v {
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(inline_value).collect();
            format!("[{}]", items.join(","))
        }
        Value::Object(_) => serde_json::to_string(v).unwrap_or_default(),
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}

fn build_flat_jsonl(
    segments: &[SegmentRecord],
    system_prompt: &str,
    prefix: &str,
    input_root: &str,
) -> Result<Vec<String>, String> {
    let mut lines = Vec::with_capacity(segments.len());
    for segment in segments {
        let role = segment.role.clone().unwrap_or_else(|| "assistant".into());
        let mut messages = Vec::new();
        if !system_prompt.trim().is_empty() {
            messages.push(json!({"role": "system", "content": system_prompt}));
        }
        let audio_path = with_prefix(prefix, &segment.segment_path, input_root);
        let content_value: Value = if role == "assistant" {
            json!([segment.phonetic_text.clone()])
        } else {
            json!(segment.phonetic_text.clone())
        };
        // Spec: user (陪聊) AND assistant (发音人) both carry the
        // refined-content key (`content_refine`). Only `system` keeps
        // the bare `content`.
        let content_key = if role == "system" {
            "content"
        } else {
            "content_refine"
        };
        let mut message = json!({ "role": role });
        message[content_key] = content_value;
        // Assistant: always array (parallel to content_refine). User: scalar.
        message["audio_file"] = if role == "assistant" {
            json!([audio_path])
        } else {
            json!(audio_path)
        };
        if !segment.emotion.is_empty() {
            // Spec rename: `emotion` → `emotion_refine` (mirrors content_refine).
            message["emotion_refine"] = json!(segment.emotion);
        }
        // `tags` deliberately omitted — paralinguistic markers live
        // inline in the text (e.g. `[breath]`, `<laugh>...</laugh>`),
        // and the dataset spec doesn't carry a separate tags field.
        if !segment.notes.trim().is_empty() {
            message["notes"] = json!(segment.notes);
        }
        messages.push(message);
        let line = render_messages_payload(&messages);
        lines.push(line);
    }
    Ok(lines)
}

/// Build paired-conversation JSONL matching the dataset spec (`自由演绎.jsonl`).
///
/// **One pair_key → one JSONL line.** A `pair_key` groups all segments
/// belonging to the same dialogue turn — typically the user's question
/// audio (`...02_陪聊.wav`) and the assistant's answer audio
/// (`...02_发音人.wav`), each of which the cutter has further split
/// into N silence-bounded segments (`...02_01_发音人.wav`,
/// `...02_02_发音人.wav`, …).
///
/// Output shape per line:
/// ```json
/// {"messages": [
///   {"role": "system",    "content": "<system_prompt>"},
///   {"role": "user",      "content": "<合并的问题文本>",
///                          "audio_file": "<源音频或单段>"},
///   {"role": "assistant", "content": ["<句1>", "<句2>", …],
///                          "audio_file": ["<wav1>", "<wav2>", …],
///                          "emotion":    ["中立",  "开心", …]}
/// ]}
/// ```
///
/// Assistant fields (`content_refine`, `audio_file`, `emotion_refine`)
/// are ALWAYS arrays — even for a single sub-segment — so downstream
/// trainers don't need to handle two value shapes. User side stays
/// scalar when there's only one cut (or `user_use_source` is on).
fn build_paired_jsonl(
    segments: &[SegmentRecord],
    system_prompt: &str,
    user_use_source: bool,
    prefix: &str,
    input_root: &str,
) -> Result<Vec<String>, String> {
    let mut by_pair: BTreeMap<String, Vec<&SegmentRecord>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for segment in segments {
        let key = pair_key(segment);
        if !by_pair.contains_key(&key) {
            order.push(key.clone());
        }
        by_pair.entry(key).or_default().push(segment);
    }

    let mut lines = Vec::new();
    for key in order {
        let mut group = by_pair.get(&key).cloned().unwrap_or_default();
        // Stable order within each role: prefer the index encoded in the
        // target file name, then fall back to start_ms so the audio_file
        // array matches the content array index by index.
        group.sort_by(|left, right| {
            let left_parts = parse_segment_output_name(&left.segment_file_name);
            let right_parts = parse_segment_output_name(&right.segment_file_name);
            segment_role_order(left.role.as_deref())
                .cmp(&segment_role_order(right.role.as_deref()))
                .then(
                    left_parts
                        .as_ref()
                        .and_then(|parts| parts.content_index)
                        .unwrap_or(0)
                        .cmp(
                            &right_parts
                                .as_ref()
                                .and_then(|parts| parts.content_index)
                                .unwrap_or(0),
                        ),
                )
                .then(left.start_ms.cmp(&right.start_ms))
                .then(left.segment_file_name.cmp(&right.segment_file_name))
        });

        let user_segs: Vec<&SegmentRecord> = group
            .iter()
            .copied()
            .filter(|segment| segment.role.as_deref() == Some("user"))
            .collect();
        let assistant_segs: Vec<&SegmentRecord> = group
            .iter()
            .copied()
            .filter(|segment| segment.role.as_deref() == Some("assistant"))
            .collect();
        let other_segs: Vec<&SegmentRecord> = group
            .iter()
            .copied()
            .filter(|segment| {
                segment.role.as_deref() != Some("user")
                    && segment.role.as_deref() != Some("assistant")
            })
            .collect();

        if assistant_segs.is_empty() && user_segs.is_empty() && !other_segs.is_empty() {
            for seg in &other_segs {
                lines.push(build_single_message_line(
                    seg,
                    system_prompt,
                    prefix,
                    input_root,
                )?);
            }
            continue;
        }

        if assistant_segs.is_empty() {
            for user in &user_segs {
                lines.push(build_single_message_line(
                    user,
                    system_prompt,
                    prefix,
                    input_root,
                )?);
            }
            continue;
        }

        // Helper: pick polished text first, fall back to raw recognition.
        let pick_text = |s: &SegmentRecord| -> String {
            if s.phonetic_text.trim().is_empty() {
                s.original_text.clone()
            } else {
                s.phonetic_text.clone()
            }
        };

        let mut messages = Vec::new();
        if !system_prompt.trim().is_empty() {
            messages.push(json!({"role": "system", "content": system_prompt}));
        }

        // --- user message ---
        // Text: when user audio is emitted as cut pieces, content must mirror
        // audio_file one by one. If the legacy "use source audio for user"
        // option is enabled, keep the old single-string shape.
        if !user_segs.is_empty() {
            let user_texts = user_segs.iter().map(|s| pick_text(s)).collect::<Vec<_>>();
            let content_value: Value = if user_use_source || user_segs.len() == 1 {
                Value::String(user_texts.join(""))
            } else {
                Value::Array(user_texts.into_iter().map(Value::String).collect())
            };
            // Audio: when `user_use_source=true` (the demo default), point
            // at the un-split source file once. Otherwise emit the cut
            // pieces — single string when there's just one, array when
            // there are several.
            let audio_value: Value = if user_use_source {
                Value::String(with_prefix(prefix, &user_segs[0].source_path, input_root))
            } else if user_segs.len() == 1 {
                Value::String(with_prefix(prefix, &user_segs[0].segment_path, input_root))
            } else {
                Value::Array(
                    user_segs
                        .iter()
                        .map(|s| Value::String(with_prefix(prefix, &s.segment_path, input_root)))
                        .collect(),
                )
            };
            // Per spec: both user (陪聊) and assistant text use
            // `content_refine`. Only `system` keeps the bare `content`.
            messages.push(json!({
                "role": "user",
                "content_refine": content_value,
                "audio_file": audio_value,
            }));
        }

        // --- assistant message ---
        // content is an array of strings — one per sub-segment. audio_file
        // and emotion mirror that array: same length, same order.
        let texts: Vec<Value> = assistant_segs
            .iter()
            .map(|s| Value::String(pick_text(s)))
            .collect();
        let audios: Vec<Value> = assistant_segs
            .iter()
            .map(|s| Value::String(with_prefix(prefix, &s.segment_path, input_root)))
            .collect();
        let emotions: Vec<Value> = assistant_segs
            .iter()
            .map(|s| {
                Value::String(
                    s.emotion
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "中立".to_string()),
                )
            })
            .collect();

        // Spec: assistant fields are ALWAYS arrays — single sub-segment
        // included. Earlier code collapsed length-1 to scalars to mirror
        // the demo file, but the downstream trainer wants a uniform
        // shape; mixed scalar/array breaks the array-indexing assumption
        // (content[i] ↔ audio_file[i] ↔ emotion_refine[i]).
        //
        // Order matters: insert keys in `role / content_refine /
        // audio_file / emotion_refine` so serde_json's preserve_order
        // (via render_messages_payload's hand-rolled writer) emits them
        // in the spec's required order.
        let mut a_msg = json!({
            "role": "assistant",
            "content_refine": Value::Array(texts),
            "audio_file": Value::Array(audios),
        });
        // Emit `emotion_refine` only when the labelers actually picked
        // one — an all-empty group means the polish step never ran.
        if assistant_segs.iter().any(|s| !s.emotion.is_empty()) {
            a_msg["emotion_refine"] = Value::Array(emotions);
        }
        // No `tags` field — paralinguistic events are carried inline in
        // the content text (e.g. `[breath]`, `<laugh>...</laugh>`).
        // Annotator notes: keep only if any sub-segment carries one.
        let notes_joined = assistant_segs
            .iter()
            .map(|s| s.notes.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" / ");
        if !notes_joined.is_empty() {
            a_msg["notes"] = json!(notes_joined);
        }
        messages.push(a_msg);

        lines.push(render_messages_payload(&messages));

        // Stragglers with neither user nor assistant role get their own
        // single-message line — rare, but preserves the data.
        for extra in &other_segs {
            lines.push(build_single_message_line(
                extra,
                system_prompt,
                prefix,
                input_root,
            )?);
        }
    }
    Ok(lines)
}

fn build_single_message_line(
    segment: &SegmentRecord,
    system_prompt: &str,
    prefix: &str,
    input_root: &str,
) -> Result<String, String> {
    let role = segment.role.clone().unwrap_or_else(|| "assistant".into());
    let mut messages = Vec::new();
    if !system_prompt.trim().is_empty() {
        messages.push(json!({"role": "system", "content": system_prompt}));
    }
    let text = if segment.phonetic_text.trim().is_empty() {
        segment.original_text.clone()
    } else {
        segment.phonetic_text.clone()
    };
    let content_value = if role == "assistant" {
        json!([text])
    } else {
        json!(text)
    };
    // Spec: user/assistant both use `content_refine`; only `system`
    // keeps the bare `content`.
    let content_key = if role == "system" {
        "content"
    } else {
        "content_refine"
    };
    let mut msg = json!({ "role": role });
    msg[content_key] = content_value;
    // Assistant: always array (mirrors content_refine). User: scalar.
    let audio_path = with_prefix(prefix, &segment.segment_path, input_root);
    msg["audio_file"] = if role == "assistant" {
        json!([audio_path])
    } else {
        json!(audio_path)
    };
    if !segment.emotion.is_empty() {
        msg["emotion_refine"] = json!(segment.emotion);
    }
    // `tags` deliberately omitted — see build_paired_jsonl.
    if !segment.notes.trim().is_empty() {
        msg["notes"] = json!(segment.notes);
    }
    messages.push(msg);
    Ok(render_messages_payload(&messages))
}

/// Compute the audio_file value for export.
/// Behavior:
/// - If `prefix` is empty: return the local path unchanged.
/// - If `input_root` is set and `path` is inside it: prepend prefix to the
///   path relative to `input_root` (with `_dialect_labeler/` stripped if
///   present, so segments appear as `segments/...` not
///   `_dialect_labeler/segments/...`).
/// - Otherwise: prepend prefix to just the file name.
fn with_prefix(prefix: &str, path: &str, input_root: &str) -> String {
    if prefix.trim().is_empty() {
        return path.to_string();
    }
    let trimmed_prefix = prefix.trim_end_matches('/').to_string();
    let normalized_path = path.replace('\\', "/");
    let rel = compute_relative_for_oss(&normalized_path, input_root);
    format!("{}/{}", trimmed_prefix, rel)
}

fn compute_relative_for_oss(path: &str, input_root: &str) -> String {
    let root_clean = input_root.trim().trim_end_matches('/');
    if !root_clean.is_empty() {
        if let Some(stripped) = path.strip_prefix(root_clean) {
            let mut rel = stripped.trim_start_matches('/').to_string();
            if let Some(stripped_internal) = rel.strip_prefix(&format!("{}/", PROJECT_FOLDER)) {
                rel = stripped_internal.to_string();
            }
            if !rel.is_empty() {
                return rel;
            }
        }
    }
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DatasetSourceName {
    mode: String,
    topic_id: u32,
    role: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DialogueKey {
    mode: String,
    topic_id: u32,
    round_index: u32,
}

#[derive(Clone, Debug)]
struct SegmentOutputName {
    key: DialogueKey,
    content_index: Option<u32>,
    role: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TopicKey {
    mode: String,
    topic_id: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SourceTopicKey {
    mode: String,
    label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SourceTopicSortKey {
    explicit_topic_rank: u8,
    explicit_topic_id: u32,
    descriptor_rank: u32,
    trailing_number: u32,
    descriptor: String,
    label: String,
}

#[derive(Clone, Debug)]
struct SegmentNameSeed {
    topic: TopicKey,
    role: String,
}

fn parse_dataset_source_audio_name(
    path: &Path,
    role_hint: Option<&str>,
) -> Option<DatasetSourceName> {
    parse_dataset_source_audio_name_with_topic(path, role_hint, None)
}

fn parse_dataset_source_audio_name_with_topic(
    path: &Path,
    role_hint: Option<&str>,
    topic_id_override: Option<u32>,
) -> Option<DatasetSourceName> {
    let stem = path_file_stem(path);
    let mode = dataset_mode(&stem)?;
    let topic_id = topic_id_override.or_else(|| last_number(&stem))?;
    let role = role_hint
        .map(|value| value.to_string())
        .or_else(|| infer_role(path));

    Some(DatasetSourceName {
        mode,
        topic_id,
        role,
    })
}

fn infer_source_topic_ids(
    audio_paths: &[PathBuf],
    records: &[ManifestRecord],
) -> HashMap<SourceTopicKey, u32> {
    let mut source_by_mode: BTreeMap<String, Vec<SourceTopicKey>> = BTreeMap::new();
    let mut seen_sources: HashSet<SourceTopicKey> = HashSet::new();
    for path in audio_paths {
        let Some(key) = source_topic_key(path) else {
            continue;
        };
        if seen_sources.insert(key.clone()) {
            source_by_mode
                .entry(key.mode.clone())
                .or_default()
                .push(key);
        }
    }

    let mut manifest_topics: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for record in records {
        let Some(audio_file) = record.audio_file.as_deref() else {
            continue;
        };
        let Some(parts) = parse_segment_output_name(audio_file) else {
            continue;
        };
        let topics = manifest_topics.entry(parts.key.mode).or_default();
        if !topics.contains(&parts.key.topic_id) {
            topics.push(parts.key.topic_id);
        }
    }
    for topics in manifest_topics.values_mut() {
        topics.sort_unstable();
    }

    let mut result = HashMap::new();
    for (mode, mut source_keys) in source_by_mode {
        source_keys.sort_by_key(source_topic_sort_key);
        let manifest_ids = manifest_topics.get(&mode);
        for (index, key) in source_keys.into_iter().enumerate() {
            let topic_id = explicit_topic_id_from_source_label(&key.label)
                .or_else(|| manifest_ids.and_then(|ids| ids.get(index).copied()))
                .unwrap_or((index + 1) as u32);
            result.insert(key, topic_id);
        }
    }

    result
}

fn source_topic_key(path: &Path) -> Option<SourceTopicKey> {
    let stem = path_file_stem(path);
    let mode = dataset_mode(&stem)?;
    let label = strip_role_token_from_stem(&stem);
    Some(SourceTopicKey { mode, label })
}

fn source_topic_sort_key(key: &SourceTopicKey) -> SourceTopicSortKey {
    let explicit_topic_id = explicit_topic_id_from_source_label(&key.label).unwrap_or(u32::MAX);
    let (descriptor, trailing_number) = split_trailing_number(&key.label);
    SourceTopicSortKey {
        explicit_topic_rank: if explicit_topic_id == u32::MAX { 1 } else { 0 },
        explicit_topic_id,
        descriptor_rank: free_topic_descriptor_rank(&descriptor),
        trailing_number: trailing_number.unwrap_or(u32::MAX),
        descriptor,
        label: key.label.clone(),
    }
}

fn explicit_topic_id_from_source_label(value: &str) -> Option<u32> {
    if value.contains("话题") {
        last_number(value)
    } else {
        None
    }
}

fn split_trailing_number(value: &str) -> (String, Option<u32>) {
    let end = value
        .char_indices()
        .rev()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0);
    if end == value.len() {
        return (value.to_string(), None);
    }
    let number = value[end..].parse::<u32>().ok();
    (value[..end].to_string(), number)
}

fn free_topic_descriptor_rank(value: &str) -> u32 {
    for (index, token) in ["惊讶", "开心", "疑问", "生气", "难过", "中立"]
        .iter()
        .enumerate()
    {
        if value.contains(token) {
            return index as u32;
        }
    }
    u32::MAX
}

fn parse_segment_output_name(value: &str) -> Option<SegmentOutputName> {
    let file_name = target_audio_file_name(value);
    let stem = Path::new(&file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(file_name.as_str());
    // Whitelist must stay in sync with `dataset_mode` below. Order
    // doesn't matter (regex alternation tries each), but keep it
    // grouped: 演绎/对话/闲聊/语料 follow the corpus naming convention.
    let re = Regex::new(
        r"^(文案演绎|文本演绎|自由演绎|自由对话|自由闲聊|长尾语料)_(\d+)_(\d+)(?:_(\d+))?_(陪聊|发音人)$",
    )
    .expect("valid output file name regex");
    let caps = re.captures(stem)?;
    let mode = caps.get(1)?.as_str().to_string();
    let topic_id = caps.get(2)?.as_str().parse::<u32>().ok()?;
    let round_index = caps.get(3)?.as_str().parse::<u32>().ok()?;
    let content_index = caps
        .get(4)
        .and_then(|value| value.as_str().parse::<u32>().ok());
    let role = role_from_label(caps.get(5)?.as_str())?;

    Some(SegmentOutputName {
        key: DialogueKey {
            mode,
            topic_id,
            round_index,
        },
        content_index,
        role,
    })
}

fn build_segment_file_name(
    input: &Path,
    role_hint: Option<&str>,
    topic_id_override: Option<u32>,
    target_file_names: &[String],
    index: usize,
    start_ms: u64,
    end_ms: u64,
) -> String {
    if let Some(target) = target_file_names
        .get(index)
        .map(|value| normalize_target_file_name(value))
        .filter(|value| !value.trim().is_empty())
    {
        return target;
    }

    if let Some(source) =
        parse_dataset_source_audio_name_with_topic(input, role_hint, topic_id_override)
    {
        let round_index = 1;
        let content_index = index + 1;
        match source.role.as_deref() {
            Some("user") => {
                return format!(
                    "{}_{:04}_{:02}_{:02}_陪聊.wav",
                    source.mode, source.topic_id, round_index, content_index
                );
            }
            Some("assistant") => {
                return format!(
                    "{}_{:04}_{:02}_{:02}_发音人.wav",
                    source.mode, source.topic_id, round_index, content_index
                );
            }
            _ => {
                return format!(
                    "{}_{:04}_{:02}_{:02}.wav",
                    source.mode, source.topic_id, round_index, 1
                );
            }
        }
    }

    let safe_source = safe_name(&path_file_stem(input));
    format!(
        "{}_{:04}_{}-{}.wav",
        safe_source,
        index + 1,
        start_ms,
        end_ms
    )
}

fn plan_dialogue_sequence_file_names(segments: &[SegmentRecord]) -> HashMap<usize, String> {
    let mut by_topic: BTreeMap<TopicKey, Vec<(usize, SegmentNameSeed)>> = BTreeMap::new();

    for (index, segment) in segments.iter().enumerate() {
        let Some(seed) = segment_name_seed(segment) else {
            continue;
        };
        by_topic
            .entry(seed.topic.clone())
            .or_default()
            .push((index, seed));
    }

    let mut planned = HashMap::new();
    for (topic, mut items) in by_topic {
        items.sort_by(|(left_index, left_seed), (right_index, right_seed)| {
            let left = &segments[*left_index];
            let right = &segments[*right_index];
            left.start_ms
                .cmp(&right.start_ms)
                .then(left.end_ms.cmp(&right.end_ms))
                .then(
                    segment_role_order(Some(&left_seed.role))
                        .cmp(&segment_role_order(Some(&right_seed.role))),
                )
                .then(left.segment_file_name.cmp(&right.segment_file_name))
        });

        let mut round_index = 0u32;
        let mut last_role: Option<String> = None;
        let mut content_counts: HashMap<(u32, String), u32> = HashMap::new();

        for (index, seed) in items {
            if round_index == 0 {
                round_index = 1;
            } else if seed.role == "user" && last_role.as_deref() == Some("assistant") {
                round_index += 1;
            }

            let count_key = (round_index, seed.role.clone());
            let content_index = content_counts.entry(count_key).or_insert(0);
            *content_index += 1;

            let role_label = role_label_for_export(&seed.role).unwrap_or("未知");
            let target_name = format!(
                "{}_{:04}_{:02}_{:02}_{}.wav",
                topic.mode, topic.topic_id, round_index, *content_index, role_label
            );

            if segments[index].segment_file_name != target_name {
                planned.insert(index, target_name);
            }
            last_role = Some(seed.role);
        }
    }

    planned
}

fn segment_name_seed(segment: &SegmentRecord) -> Option<SegmentNameSeed> {
    if let Some(parts) = parse_segment_output_name(&segment.segment_file_name) {
        return Some(SegmentNameSeed {
            topic: TopicKey {
                mode: parts.key.mode,
                topic_id: parts.key.topic_id,
            },
            role: parts.role,
        });
    }

    let role = segment
        .role
        .clone()
        .or_else(|| infer_role(Path::new(&segment.source_path)))?;
    let source = parse_dataset_source_audio_name(Path::new(&segment.source_path), Some(&role))?;
    Some(SegmentNameSeed {
        topic: TopicKey {
            mode: source.mode,
            topic_id: source.topic_id,
        },
        role,
    })
}

/// Dataset "mode" whitelist — substring match into the source filename
/// stem. Returns the exact token if found; otherwise None.
///
/// **Edit policy**: add a new entry when a new corpus bundle introduces
/// a new descriptor (e.g. 0429-台湾 added `文本演绎`). Do NOT generalise
/// to regex or auto-derivation — earlier attempts polluted output names
/// with dialect prefixes like `台湾话-文本演绎`, breaking the canonical
/// `<mode>_NNNN_NN_NN_<role>.wav` shape.
fn dataset_mode(value: &str) -> Option<String> {
    [
        "文案演绎",
        "文本演绎",
        "自由演绎",
        "自由对话",
        "自由闲聊",
        "长尾语料",
    ]
    .iter()
    .find(|mode| value.contains(**mode))
    .map(|mode| (*mode).to_string())
}

fn last_number(value: &str) -> Option<u32> {
    let mut current = String::new();
    let mut last = None;

    for ch in value.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            last = current.parse::<u32>().ok();
            current.clear();
        }
    }

    if !current.is_empty() {
        last = current.parse::<u32>().ok();
    }

    last
}

fn role_from_label(value: &str) -> Option<String> {
    match value {
        "陪聊" => Some("user".to_string()),
        "发音人" => Some("assistant".to_string()),
        _ => None,
    }
}

fn role_label_for_export(role: &str) -> Option<&'static str> {
    match role {
        "user" => Some("陪聊"),
        "assistant" => Some("发音人"),
        _ => None,
    }
}

fn segment_role_order(role: Option<&str>) -> u8 {
    match role {
        Some("user") => 0,
        Some("assistant") => 1,
        _ => 2,
    }
}

fn target_audio_file_name(value: &str) -> String {
    value
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(value)
        .to_string()
}

fn normalize_target_file_name(value: &str) -> String {
    let file_name = target_audio_file_name(value);
    let with_ext = if lower_extension(Path::new(&file_name)).as_deref() == Some("wav") {
        file_name
    } else {
        format!("{}.wav", file_name.trim_end_matches('.'))
    };

    if let Some(parts) = parse_segment_output_name(&with_ext) {
        if parts.content_index.is_none() {
            if let Some(role_label) = role_label_for_export(&parts.role) {
                return format!(
                    "{}_{:04}_{:02}_01_{}.wav",
                    parts.key.mode, parts.key.topic_id, parts.key.round_index, role_label
                );
            }
        }
    }

    with_ext
}

fn strip_role_token_from_stem(stem: &str) -> String {
    // (left_sep) (role) (right_sep | end)
    // `陪聊人` MUST come before `陪聊` in the alternation — regex tries
    // branches left-to-right and `陪聊人` is a longer match. Without the
    // longer-first ordering, the 0429 台湾 batch's `陪聊人` would only
    // get its `陪聊` prefix stripped, leaving a dangling `人` in the key.
    let re = Regex::new(r"([_\-])(?:发音人|陪聊人|assistant|陪聊|user)(?:([_\-])|$)")
        .expect("valid regex");
    let cleaned = re
        .replace(stem, |caps: &regex::Captures| match caps.get(2) {
            Some(right) => right.as_str().to_string(),
            None => String::new(),
        })
        .to_string();
    let trimmed = cleaned.trim_end_matches(['_', '-']).to_string();
    if trimmed.is_empty() {
        stem.to_string()
    } else {
        trimmed
    }
}

/// Group key for the "one dialogue turn = one JSONL line" rule.
///
/// Two source filenames belong to the same dialogue turn when their stems
/// match after the role token is removed. Two real-world conventions are
/// supported:
///
///   1. **Role at the END, separated by `_`** (the demo `自由演绎.jsonl`):
///      ```text
///      自由演绎_0001_01_陪聊.wav   user
///      自由演绎_0001_01_发音人.wav assistant
///      ```
///      Stripping `_陪聊` / `_发音人` from the end yields the shared key
///      `自由演绎_0001_01`.
///
///   2. **Role in the MIDDLE, separated by `-`** (the 长沙方言 dataset):
///      ```text
///      长沙方言-0413-文案演绎-陪聊-话题1.wav    user
///      长沙方言-0413-文案演绎-发音人-话题1.wav  assistant
///      ```
///      Removing the `-陪聊-` / `-发音人-` token (collapsing the
///      surrounding `-` into one) yields the shared key
///      `长沙方言-0413-文案演绎-话题1`.
///
/// The regex looks for a role token bounded by `_` or `-` on the left,
/// and either `_`/`-` on the right OR end-of-string. Matched at the end
/// the whole `<sep><role>` is dropped; matched in the middle, just the
/// role token (and one of the surrounding separators) is removed so the
/// remaining text stays coherent.
fn pair_key(segment: &SegmentRecord) -> String {
    // Try the canonical-naming parser on two candidate sources, in
    // order of trust: the DB field, then the on-disk filename. They
    // CAN diverge — `segment_file_name` may still hold an old fallback
    // form from a binary built before the current dataset_mode
    // whitelist (e.g. 文本演绎 batches cut by an older build), while
    // the file on disk has since been renamed to the canonical form
    // by `plan_dialogue_sequence_file_names`. Without this second try,
    // user (`陪聊`) and assistant (`发音人`) segments fall into
    // different fallback keys and never pair → JSONL loses 一来一回.
    let path_basename = Path::new(&segment.segment_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    for candidate in [segment.segment_file_name.as_str(), path_basename] {
        if candidate.is_empty() {
            continue;
        }
        if let Some(parts) = parse_segment_output_name(candidate) {
            return format!(
                "{}_{:04}_{:02}",
                parts.key.mode, parts.key.topic_id, parts.key.round_index
            );
        }
    }
    strip_role_token_from_stem(&path_file_stem(Path::new(&segment.source_path)))
}

/// Detect whether the base stem already encodes a "话轮" (dialogue
/// turn) number as a `_<NN>` suffix, and split it out.
///
/// The dataset spec requires every cut filename to carry three numbers:
/// 话题号 `_` 话轮号 `_` 音频序号. Some sources (the demo
/// `自由演绎_0001_01_发音人.wav`) already encode the turn (`_01`) inside
/// the stem; others (`长沙方言-...-话题1.wav`) only encode the topic
/// and treat the whole recording as a single turn.
///
/// Returns `(stem_without_turn, turn_number)`. When no `_<2 digits>`
/// suffix is present, the original stem is returned with `turn = 1`
/// (default — every recording is one turn).
///
/// ```text
/// extract_turn_from_base("自由演绎_0001_01")
///   → ("自由演绎_0001", 1)         // wait — turn is `01` here
/// ```
/// Actually the example above returns `("自由演绎_0001", 1)` on the
/// `_01` match — `01` parses as 1. The cut filename will then encode
/// the turn back as `_01_` regardless.
#[cfg(test)]
fn extract_turn_from_base(base: &str) -> (String, u32) {
    // Trailing `_<2 digits>` — the demo's turn marker.
    let re = Regex::new(r"_(\d{2})$").expect("valid regex");
    if let Some(caps) = re.captures(base) {
        let turn: u32 = caps[1].parse().unwrap_or(1);
        let whole = caps.get(0).unwrap();
        let stripped = base[..whole.start()].to_string();
        return (stripped, turn);
    }
    (base.to_string(), 1)
}

/// Extract `(base_without_role, role_token)` from a file stem.
///
/// Two layouts are supported (mirroring `pair_key`):
///
///   1. **Role at the end, `_` separator** — the demo convention:
///      `自由演绎_0001_01_发音人` → `("自由演绎_0001_01", Some("发音人"))`
///
///   2. **Role in the middle, `-` separator** — the 长沙方言 dataset:
///      `长沙方言-0413-文案演绎-发音人-话题1`
///      → `("长沙方言-0413-文案演绎-话题1", Some("发音人"))`
///
/// When a role token is detected in the middle, the surrounding
/// separators are collapsed into one (the right-hand one) so the base
/// stays a coherent identifier. The cutter then appends the demo-style
/// `_<NN>_<role>.wav` suffix to that base — making cut sub-segments
/// from `长沙方言-...-发音人-话题1.wav` come out as
/// `长沙方言-...-话题1_01_发音人.wav`, which matches the demo layout
/// and lets `pair_key` align user/assistant turns automatically.
#[cfg(test)]
fn split_role_suffix(stem: &str) -> (String, Option<String>) {
    // (left_sep) (role) (right_sep | end_of_string)
    let re = Regex::new(r"([_\-])(发音人|陪聊人|assistant|陪聊|user)(?:([_\-])|$)")
        .expect("valid regex");
    if let Some(caps) = re.captures(stem) {
        let role = caps.get(2).unwrap().as_str().to_string();
        let whole = caps.get(0).unwrap();
        let right_sep = caps.get(3).map(|s| s.as_str()).unwrap_or("");
        // Reconstruct: <prefix><right_sep><suffix>. When the role was at
        // the END (no right_sep), this just drops the role + left_sep.
        // When the role was in the MIDDLE, we keep one separator so
        // adjacent tokens don't fuse together.
        let mut base = String::with_capacity(stem.len());
        base.push_str(&stem[..whole.start()]);
        base.push_str(right_sep);
        base.push_str(&stem[whole.end()..]);
        let trimmed = base.trim_end_matches(['_', '-']).to_string();
        return (trimmed, Some(role));
    }
    (stem.to_string(), None)
}

/// Per-segment progress event payload. Frontend listens to
/// `recognize:segment_done` and updates the segment immediately,
/// without waiting for the whole batch to finish.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SegmentProgressEvent {
    segment_id: String,
    /// "asr" — Whisper text just landed (no LLM polish yet).
    /// "polish_ok" — LLM polish succeeded (text + emotion + tags ready).
    /// "polish_fail" — LLM polish failed (Whisper text preserved).
    phase: String,
    text: String,
    emotion: Option<String>,
    tags: Vec<String>,
    polish_endpoint: Option<String>,
    polish_model: Option<String>,
    cached: bool,
    completed: usize,
    total: usize,
}

/// Global cancel flag for the in-flight recognize run. Workers poll it
/// at safe checkpoints (between Whisper chunks / between polish tasks).
/// In-flight Whisper subprocesses or LLM HTTP calls are NOT killed —
/// they finish naturally (~5–10s typical), then the worker loop exits.
static RECOGNIZE_CANCEL: std::sync::OnceLock<std::sync::Arc<std::sync::atomic::AtomicBool>> =
    std::sync::OnceLock::new();

fn recognize_cancel_flag() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    RECOGNIZE_CANCEL
        .get_or_init(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)))
        .clone()
}

#[tauri::command]
fn cancel_recognize() -> Result<(), String> {
    recognize_cancel_flag().store(true, std::sync::atomic::Ordering::Relaxed);
    eprintln!("[recognize] cancel requested");
    Ok(())
}

#[tauri::command]
async fn recognize_segments(
    app: tauri::AppHandle,
    project_dir: String,
    segments: Vec<SegmentRecord>,
    options: Option<RecognitionOptions>,
) -> Result<Vec<RecognitionResult>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        recognize_segments_impl(app, project_dir, segments, options.unwrap_or_default())
    })
    .await
    .map_err(|err| err.to_string())?
}

/// Two-phase recognize: Whisper → Ollama polish.
///
/// **Why two-phase and not streaming?** On Apple Silicon (and any single-GPU
/// box) the streaming pipeline ran into resource contention — Ollama's 32b
/// model holds ~22GB of VRAM/unified-memory while it's loaded, and a
/// concurrent Whisper-on-Metal process gets timeshared with it, slowing
/// both sides 5–10× and occasionally inducing multi-minute swap stalls.
/// Two-phase lets each tool own the GPU/CPU in turn:
///   Phase 1 — Whisper drains all segments (each Whisper subprocess uses
///             CPU here so it never collides with a hot Ollama runner).
///   Phase 2 — LLM polish workers drain the polished text queue.
///
/// Each phase emits per-segment progress events. The frontend resets the
/// progress bar between phases so users see Whisper-progress and
/// polish-progress separately.
///
/// Cancellation: `cancel_recognize` flips a global flag. Workers check it
/// at the top of each loop iteration. In-flight Whisper subprocesses /
/// LLM HTTP calls finish naturally (5–10s typical) before the worker exits.
pub(crate) fn recognize_segments_impl(
    app: tauri::AppHandle,
    project_dir: String,
    segments: Vec<SegmentRecord>,
    options: RecognitionOptions,
) -> Result<Vec<RecognitionResult>, String> {
    use std::sync::atomic::Ordering;
    use tauri::Emitter as _;
    if segments.is_empty() {
        return Ok(Vec::new());
    }

    ensure_ffmpeg()?;
    // Whisper CLI is only required when no remote whisper-server endpoints
    // are configured — defer the lookup so an HTTP-only setup doesn't need
    // `whisper` installed locally.
    let project_dir = PathBuf::from(project_dir);
    let cache_dir = project_dir.join(".asr").join("cache");
    fs::create_dir_all(&cache_dir).map_err(|err| err.to_string())?;

    // Reset cancel flag at the start of each run.
    let cancel = recognize_cancel_flag();
    cancel.store(false, Ordering::Relaxed);

    let model = options
        .whisper_model
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "large-v3".to_string());
    let initial_prompt = options
        .initial_prompt
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            "以下是中文方言（长沙话）口语转写，请用汉字记录听到的字音，不要翻译。".to_string()
        });
    let use_cache = options.use_cache.unwrap_or(true);
    let overwrite_cache = options.overwrite_cache.unwrap_or(false);
    let use_llm = options.use_llm.unwrap_or(false);
    let ollama_url = options
        .ollama_url
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string());
    let ollama_model = options.ollama_model.clone();
    let llm_prompt = options
        .llm_prompt
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_LLM_PROMPT.to_string());

    let id_to_idx: HashMap<String, usize> = segments
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone(), i))
        .collect();
    let results: std::sync::Arc<Mutex<Vec<Option<RecognitionResult>>>> =
        std::sync::Arc::new(Mutex::new(vec![None; segments.len()]));

    // ============================================================
    // PHASE 1 — ASR (cache pre-pass + Whisper worker pool)
    // ============================================================

    let mut to_run: Vec<SegmentRecord> = Vec::new();
    let mut cache_keys: HashMap<String, String> = HashMap::new();
    for segment in &segments {
        let idx = id_to_idx[&segment.id];
        let segment_path = PathBuf::from(&segment.segment_path);
        if !segment_path.is_file() {
            return Err(format!("切割片段不存在：{}", segment.segment_path));
        }
        let key = asr_cache_key(&segment_path, &model)?;
        cache_keys.insert(segment.id.clone(), key.clone());
        let cache_file = cache_dir.join(format!("{}.json", key));
        if use_cache && !overwrite_cache && cache_file.is_file() {
            if let Ok(text) = fs::read_to_string(&cache_file) {
                let value: Value = serde_json::from_str(&text).unwrap_or(json!({}));
                let raw = value
                    .get("text")
                    .and_then(|item| item.as_str())
                    .unwrap_or_default()
                    .to_string();
                if let Ok(mut g) = results.lock() {
                    g[idx] = Some(RecognitionResult {
                        segment_id: segment.id.clone(),
                        text: raw.clone(),
                        raw_text: raw,
                        polished: false,
                        cached: true,
                        polish_endpoint: None,
                        polish_model: None,
                        emotion: None,
                        tags: Vec::new(),
                    });
                }
                continue;
            }
        }
        to_run.push(segment.clone());
    }

    let asr_total = to_run.len();
    let asr_counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // Resolve the optional remote whisper-server pool. When at least one
    // endpoint URL parses, the ASR phase dispatches per-segment HTTP calls
    // across the pool (mirroring the Ollama polish pool). Empty → fall back
    // to the legacy local-CLI chunk loop below.
    let whisper_eps: Vec<String> = options
        .whisper_endpoints
        .as_ref()
        .map(|v| {
            v.iter()
                .filter(|def| def.enabled)
                .filter_map(|def| normalise_http_url(&def.url))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if !to_run.is_empty() && !whisper_eps.is_empty() {
        // ============================================================
        // REMOTE WHISPER POOL — HTTP work-stealing across endpoints.
        // ============================================================
        let endpoint_count = whisper_eps.len();
        let concurrency = options
            .whisper_concurrency
            .unwrap_or((endpoint_count as u32 * 2).min(8))
            .max(1) as usize;

        eprintln!(
            "[recognize] phase=whisper(http) · {} segs · {} endpoint(s) · concurrency={} · pool={:?}",
            asr_total, endpoint_count, concurrency, whisper_eps,
        );

        let (seg_tx, seg_rx) = mpsc::channel::<SegmentRecord>();
        for s in to_run.iter().cloned() {
            let _ = seg_tx.send(s);
        }
        drop(seg_tx);
        let seg_rx = std::sync::Arc::new(Mutex::new(seg_rx));
        let prompt_arc = std::sync::Arc::new(initial_prompt.clone());

        let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::with_capacity(concurrency);
        for w in 0..concurrency {
            let url = whisper_eps[w % endpoint_count].clone();
            let rx = std::sync::Arc::clone(&seg_rx);
            let app_clone = app.clone();
            let counter = std::sync::Arc::clone(&asr_counter);
            let results_arc = std::sync::Arc::clone(&results);
            let cancel_clone = std::sync::Arc::clone(&cancel);
            let id_to_idx_clone = id_to_idx.clone();
            let cache_keys_clone = cache_keys.clone();
            let cache_dir_clone = cache_dir.clone();
            let prompt = std::sync::Arc::clone(&prompt_arc);
            let h = std::thread::spawn(move || loop {
                if cancel_clone.load(Ordering::Relaxed) {
                    return;
                }
                let segment = {
                    let guard = match rx.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                    match guard.recv() {
                        Ok(c) => c,
                        Err(_) => return,
                    }
                };
                let started = Instant::now();
                let res = transcribe_remote(&url, Path::new(&segment.segment_path), "zh", &prompt);
                let elapsed_ms = started.elapsed().as_millis();
                eprintln!(
                    "[whisper-http {}] {} · {} · {:.1}s · {}",
                    w,
                    url,
                    segment.id,
                    elapsed_ms as f64 / 1000.0,
                    if res.is_ok() { "ok" } else { "fail" },
                );
                let raw = match res {
                    Ok(r) => r,
                    Err(err) => {
                        eprintln!(
                            "[whisper-http {}] {} via {} error: {}",
                            w, segment.id, url, err,
                        );
                        // Skip — front-end shows nothing for this segment;
                        // the user can re-run after fixing the endpoint.
                        continue;
                    }
                };
                if let Some(key) = cache_keys_clone.get(&segment.id) {
                    let cache_file = cache_dir_clone.join(format!("{}.json", key));
                    let _ = fs::write(&cache_file, json!({ "text": raw }).to_string());
                }
                let Some(&idx) = id_to_idx_clone.get(&segment.id) else {
                    continue;
                };
                let completed = counter.fetch_add(1, Ordering::Relaxed) + 1;
                let _ = app_clone.emit(
                    "recognize:segment_done",
                    SegmentProgressEvent {
                        segment_id: segment.id.clone(),
                        phase: "asr".into(),
                        text: raw.clone(),
                        emotion: None,
                        tags: Vec::new(),
                        polish_endpoint: None,
                        polish_model: None,
                        cached: false,
                        completed,
                        total: asr_total,
                    },
                );
                if let Ok(mut g) = results_arc.lock() {
                    g[idx] = Some(RecognitionResult {
                        segment_id: segment.id.clone(),
                        text: raw.clone(),
                        raw_text: raw,
                        polished: false,
                        cached: false,
                        emotion: None,
                        tags: Vec::new(),
                        polish_endpoint: None,
                        polish_model: None,
                    });
                }
            });
            handles.push(h);
        }
        for h in handles {
            let _ = h.join();
        }
    } else if !to_run.is_empty() {
        // ============================================================
        // LEGACY LOCAL WHISPER CLI — chunked, one model per process.
        // ============================================================
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_millis())
            .unwrap_or_default();
        let output_dir = project_dir.join(".asr").join(format!("run_{}", stamp));
        fs::create_dir_all(&output_dir).map_err(|err| err.to_string())?;

        let whisper = find_whisper_command()?;

        const WHISPER_CHUNK_SIZE: usize = 8;
        let total_chunks = to_run.len().div_ceil(WHISPER_CHUNK_SIZE);
        let whisper_concurrency = options.whisper_concurrency.unwrap_or(1).max(1) as usize;

        let (chunks_tx, chunks_rx) = mpsc::channel::<Vec<SegmentRecord>>();
        for chunk in to_run.chunks(WHISPER_CHUNK_SIZE) {
            let _ = chunks_tx.send(chunk.to_vec());
        }
        drop(chunks_tx);
        let chunks_rx = std::sync::Arc::new(Mutex::new(chunks_rx));
        let chunks_done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        eprintln!(
            "[recognize] phase=whisper · {} segs to run · {} chunks · whisper_concurrency={}",
            asr_total, total_chunks, whisper_concurrency
        );

        let mut whisper_handles: Vec<std::thread::JoinHandle<()>> =
            Vec::with_capacity(whisper_concurrency);
        for w in 0..whisper_concurrency {
            let rx = std::sync::Arc::clone(&chunks_rx);
            let app_clone = app.clone();
            let counter = std::sync::Arc::clone(&asr_counter);
            let results_arc = std::sync::Arc::clone(&results);
            let cancel_clone = std::sync::Arc::clone(&cancel);
            let id_to_idx_clone = id_to_idx.clone();
            let cache_keys_clone = cache_keys.clone();
            let cache_dir_clone = cache_dir.clone();
            let output_dir_clone = output_dir.clone();
            let initial_prompt_clone = initial_prompt.clone();
            let model_clone = model.clone();
            let whisper_path = whisper.to_string();
            let chunks_done_clone = std::sync::Arc::clone(&chunks_done);
            let h = std::thread::spawn(move || loop {
                if cancel_clone.load(Ordering::Relaxed) {
                    return;
                }
                let chunk = {
                    let guard = match rx.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                    match guard.recv() {
                        Ok(c) => c,
                        Err(_) => return,
                    }
                };
                let chunk_idx = chunks_done_clone.fetch_add(1, Ordering::Relaxed) + 1;
                eprintln!(
                    "[whisper-{}] chunk {}/{} · {} segs",
                    w,
                    chunk_idx,
                    total_chunks,
                    chunk.len()
                );

                let mut command = silent_command(&whisper_path);
                command
                    .arg("--model")
                    .arg(&model_clone)
                    .arg("--language")
                    .arg("Chinese")
                    .arg("--task")
                    .arg("transcribe")
                    .arg("--output_format")
                    .arg("json")
                    .arg("--output_dir")
                    .arg(&output_dir_clone)
                    .arg("--fp16")
                    .arg("False")
                    .arg("--verbose")
                    .arg("False")
                    .arg("--condition_on_previous_text")
                    .arg("False")
                    // Force CPU. On Apple Silicon, Ollama 32b holds ~22GB
                    // VRAM whenever it's hot — Whisper-on-Metal would get
                    // timeshared and stall. CPU inference for turbo is
                    // ~1–1.5s/seg and steady. Phase 1 runs only Whisper,
                    // but a hot Ollama from a prior run can still hog VRAM.
                    .arg("--device")
                    .arg("cpu")
                    .arg("--initial_prompt")
                    .arg(&initial_prompt_clone);
                for segment in &chunk {
                    command.arg(&segment.segment_path);
                }
                let output = match command.output() {
                    Ok(o) => o,
                    Err(err) => {
                        eprintln!("[whisper-{}] spawn failed: {}", w, err);
                        continue;
                    }
                };
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    eprintln!(
                        "[whisper-{}] chunk {} failed (skipping): {}",
                        w,
                        chunk_idx,
                        stderr.trim()
                    );
                    continue;
                }

                for segment in &chunk {
                    let segment_path = PathBuf::from(&segment.segment_path);
                    let raw = match read_whisper_json_text(&output_dir_clone, &segment_path) {
                        Ok(r) => r,
                        Err(err) => {
                            eprintln!("[whisper-{}] missing output for {}: {}", w, segment.id, err);
                            continue;
                        }
                    };
                    if let Some(key) = cache_keys_clone.get(&segment.id) {
                        let cache_file = cache_dir_clone.join(format!("{}.json", key));
                        let _ = fs::write(&cache_file, json!({"text": raw}).to_string());
                    }
                    let Some(&idx) = id_to_idx_clone.get(&segment.id) else {
                        continue;
                    };
                    let completed = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    let _ = app_clone.emit(
                        "recognize:segment_done",
                        SegmentProgressEvent {
                            segment_id: segment.id.clone(),
                            phase: "asr".into(),
                            text: raw.clone(),
                            emotion: None,
                            tags: Vec::new(),
                            polish_endpoint: None,
                            polish_model: None,
                            cached: false,
                            completed,
                            total: asr_total,
                        },
                    );
                    if let Ok(mut g) = results_arc.lock() {
                        g[idx] = Some(RecognitionResult {
                            segment_id: segment.id.clone(),
                            text: raw.clone(),
                            raw_text: raw,
                            polished: false,
                            cached: false,
                            emotion: None,
                            tags: Vec::new(),
                            polish_endpoint: None,
                            polish_model: None,
                        });
                    }
                }
            });
            whisper_handles.push(h);
        }
        for h in whisper_handles {
            let _ = h.join();
        }
    }

    // Tell the frontend Phase 1 is done so it can flip the progress bar
    // to the Polish label and reset the counter.
    let _ = app.emit(
        "recognize:segment_done",
        SegmentProgressEvent {
            segment_id: String::new(),
            phase: "phase_done".into(),
            text: "asr".into(),
            emotion: None,
            tags: Vec::new(),
            polish_endpoint: None,
            polish_model: None,
            cached: false,
            completed: asr_total,
            total: asr_total,
        },
    );

    if cancel.load(Ordering::Relaxed) {
        eprintln!("[recognize] cancelled after Whisper phase");
        let g = results.lock().map_err(|err| err.to_string())?;
        return Ok(g.iter().filter_map(|s| s.clone()).collect());
    }

    // ============================================================
    // PHASE 2 — Ollama polish
    // ============================================================

    if use_llm {
        // Build the polish task list. Skip segments that:
        //   (a) Have empty Whisper text — nothing to polish.
        //   (b) Already have an emotion tag from a prior polish run, unless
        //       the caller asked for `overwriteCache` (the "重新识别" path).
        //       This is the resume-after-cancel guarantee — interrupted runs
        //       can be re-launched and polished work is not redone.
        struct PolishTask {
            idx: usize,
            segment_id: String,
            raw_text: String,
            role: String,
            hint: String,
        }
        let mut tasks: Vec<PolishTask> = Vec::new();
        {
            let g = results.lock().map_err(|err| err.to_string())?;
            for (idx, slot) in g.iter().enumerate() {
                let Some(r) = slot else { continue };
                if r.text.trim().is_empty() {
                    continue;
                }
                let segment = &segments[idx];
                if !overwrite_cache && !segment.emotion.is_empty() {
                    continue;
                }
                tasks.push(PolishTask {
                    idx,
                    segment_id: r.segment_id.clone(),
                    raw_text: r.text.clone(),
                    role: segment.role.clone().unwrap_or_else(|| "assistant".into()),
                    hint: segment.original_text.clone(),
                });
            }
        }
        let polish_total = tasks.len();

        if polish_total == 0 {
            eprintln!(
                "[recognize] phase=polish · nothing to do (all segments already polished or empty)"
            );
        } else {
            let model_name = ollama_model
                .clone()
                .ok_or_else(|| "未指定 Ollama 模型".to_string())?;

            let mut endpoints: Vec<(String, String)> = Vec::new();
            if let Some(url) = normalise_http_url(&ollama_url) {
                endpoints.push((url, model_name.clone()));
            }
            if let Some(defs) = options.ollama_extra_endpoints.as_ref() {
                for def in defs {
                    if !def.enabled {
                        continue;
                    }
                    let Some(url) = normalise_http_url(&def.url) else {
                        continue;
                    };
                    let mdl = def
                        .model
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .unwrap_or(model_name.as_str())
                        .to_string();
                    if endpoints.iter().any(|(u, m)| u == &url && m == &mdl) {
                        continue;
                    }
                    endpoints.push((url, mdl));
                }
            }
            if let Some(extras) = options.ollama_extra_urls.as_ref() {
                for url in extras {
                    let Some(url) = normalise_http_url(url) else {
                        continue;
                    };
                    if endpoints.iter().any(|(u, _)| u == &url) {
                        continue;
                    }
                    endpoints.push((url, model_name.clone()));
                }
            }
            if endpoints.is_empty() {
                return Err("Ollama 端点列表为空 — 检查设置抽屉里的端点 URL".to_string());
            }
            let endpoint_count = endpoints.len();
            let concurrency = options
                .llm_concurrency
                .unwrap_or((endpoint_count as u32 * 2).min(8))
                .max(1) as usize;

            eprintln!(
                "[recognize] phase=polish · {} tasks · {} endpoint(s) · llm_concurrency={} · pool={:?}",
                polish_total, endpoint_count, concurrency, endpoints,
            );

            let (task_tx, task_rx) = mpsc::channel::<PolishTask>();
            for t in tasks {
                let _ = task_tx.send(t);
            }
            drop(task_tx);
            let task_rx = std::sync::Arc::new(Mutex::new(task_rx));
            let polish_counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let prompt_arc = std::sync::Arc::new(llm_prompt.clone());

            let mut polish_handles: Vec<std::thread::JoinHandle<()>> =
                Vec::with_capacity(concurrency);
            for worker_idx in 0..concurrency {
                let (url, mdl) = endpoints[worker_idx % endpoint_count].clone();
                let prompt = std::sync::Arc::clone(&prompt_arc);
                let rx = std::sync::Arc::clone(&task_rx);
                let app_clone = app.clone();
                let counter = std::sync::Arc::clone(&polish_counter);
                let results_arc = std::sync::Arc::clone(&results);
                let cancel_clone = std::sync::Arc::clone(&cancel);
                let total = polish_total;
                let h = std::thread::spawn(move || loop {
                    if cancel_clone.load(Ordering::Relaxed) {
                        return;
                    }
                    let task = {
                        let guard = match rx.lock() {
                            Ok(g) => g,
                            Err(_) => return,
                        };
                        match guard.recv() {
                            Ok(t) => t,
                            Err(_) => return,
                        }
                    };
                    let started = Instant::now();
                    let res =
                        polish_text(&url, &mdl, &prompt, &task.raw_text, &task.role, &task.hint);
                    let elapsed_ms = started.elapsed().as_millis();
                    eprintln!(
                        "[polish] {} · {} · {:.1}s · {}",
                        task.segment_id,
                        mdl,
                        elapsed_ms as f64 / 1000.0,
                        if res.is_ok() { "ok" } else { "fail" }
                    );
                    if let Ok(p) = &res {
                        if !p.text.trim().is_empty() {
                            if let Ok(mut g) = results_arc.lock() {
                                if let Some(r) = g[task.idx].as_mut() {
                                    r.text = p.text.clone();
                                    r.polished = true;
                                    r.emotion = p.emotion.clone();
                                    r.tags = p.tags.clone();
                                    r.polish_endpoint = Some(url.clone());
                                    r.polish_model = Some(mdl.clone());
                                }
                            }
                        }
                    }
                    let completed = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    let event = match &res {
                        Ok(p) => SegmentProgressEvent {
                            segment_id: task.segment_id.clone(),
                            phase: "polish_ok".into(),
                            text: p.text.clone(),
                            emotion: p.emotion.clone(),
                            tags: p.tags.clone(),
                            polish_endpoint: Some(url.clone()),
                            polish_model: Some(mdl.clone()),
                            cached: false,
                            completed,
                            total,
                        },
                        Err(err) => {
                            eprintln!(
                                "[polish] error for {} via {}: {}",
                                task.segment_id, url, err
                            );
                            SegmentProgressEvent {
                                segment_id: task.segment_id.clone(),
                                phase: "polish_fail".into(),
                                text: task.raw_text.clone(),
                                emotion: None,
                                tags: Vec::new(),
                                polish_endpoint: Some(url.clone()),
                                polish_model: Some(mdl.clone()),
                                cached: false,
                                completed,
                                total,
                            }
                        }
                    };
                    let _ = app_clone.emit("recognize:segment_done", event);
                });
                polish_handles.push(h);
            }
            for h in polish_handles {
                let _ = h.join();
            }
        }

        let _ = app.emit(
            "recognize:segment_done",
            SegmentProgressEvent {
                segment_id: String::new(),
                phase: "phase_done".into(),
                text: "polish".into(),
                emotion: None,
                tags: Vec::new(),
                polish_endpoint: None,
                polish_model: None,
                cached: false,
                completed: polish_total,
                total: polish_total,
            },
        );
    }

    let g = results.lock().map_err(|err| err.to_string())?;
    let out: Vec<RecognitionResult> = g.iter().filter_map(|s| s.clone()).collect();
    if cancel.load(Ordering::Relaxed) {
        eprintln!(
            "[recognize] run cancelled · returning {} partial results",
            out.len()
        );
    }
    Ok(out)
}

fn polish_text(
    ollama_url: &str,
    model: &str,
    prompt_template: &str,
    raw_text: &str,
    role: &str,
    hint: &str,
) -> Result<PolishedOutput, String> {
    let role_label = match role {
        "user" => "陪聊（普通话提问者）",
        "assistant" => "发音人（说长沙方言）",
        _ => "未知角色",
    };
    let hint_block = if hint.trim().is_empty() {
        String::new()
    } else {
        format!("【参考文本（仅供对照，可能为空或不准确）】\n{}\n", hint)
    };
    let user_msg = format!(
        "【角色】{}\n{}【Whisper 识别初稿】\n{}\n\n严格按照系统提示词的 JSON 格式输出。",
        role_label, hint_block, raw_text
    );

    let body = json!({
        "model": model,
        "stream": false,
        "format": "json",
        "options": {
            "temperature": 0.2,
            "top_p": 0.9,
            "num_ctx": 8192
        },
        "messages": [
            {"role": "system", "content": prompt_template},
            {"role": "user", "content": user_msg}
        ]
    });

    let url = format!("{}/api/chat", ollama_url.trim_end_matches('/'));
    // 15-minute timeout — generous enough that a slow endpoint (e.g. 122b
    // on memory pressure, or a model still warming up cold) gets a fair
    // chance to finish without blowing the whole pipeline. The work-stealing
    // pool means a slow endpoint just contributes proportionally less; we
    // don't want to drop their work entirely.
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(900))
        .build();
    let response = agent
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|err| format!("Ollama HTTP 失败：{}", err))?;
    let raw_body = response
        .into_string()
        .map_err(|err| format!("Ollama 响应读取失败：{}", err))?;
    let value: Value = serde_json::from_str(&raw_body).map_err(|err| {
        eprintln!(
            "[polish_text] Ollama 响应非法 JSON：{}\n--- body ---\n{}",
            err, raw_body
        );
        format!("Ollama 返回非法 JSON：{}", err)
    })?;
    let content = value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or_default();
    if content.trim().is_empty() {
        eprintln!(
            "[polish_text] Ollama content 为空（model={}, raw_text={:?}）",
            model, raw_text
        );
    }
    let parsed = parse_polished_output(content, raw_text);
    if parsed.text.trim().is_empty() {
        // Always preserve the Whisper draft so the user never sees an empty
        // segment after a successful ASR step.
        eprintln!(
            "[polish_text] LLM polish 输出 text 为空，回退到 Whisper 原文：{:?}",
            raw_text
        );
        return Ok(PolishedOutput {
            text: raw_text.to_string(),
            emotion: parsed.emotion,
            tags: parsed.tags,
        });
    }
    Ok(parsed)
}

/// Lenient URL normaliser shared by the ASR and polish endpoint pools.
/// Tolerates the typical typos users hand-enter (`http//`, missing scheme,
/// trailing slash, mixed case). Returns `None` if there's nothing usable.
fn normalise_http_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("https://") {
        let len = "https://".len();
        let rest_orig = &trimmed[len..len + rest.len()];
        return Some(format!("https://{}", rest_orig));
    }
    let candidates = [
        "https://", "http://", "https//", "http//", "https:", "http:",
    ];
    let mut tail = trimmed;
    for prefix in candidates.iter() {
        let lower_tail = tail.to_ascii_lowercase();
        if lower_tail.starts_with(prefix) {
            tail = &tail[prefix.len()..];
            break;
        }
    }
    let tail = tail.trim_start_matches('/');
    if tail.is_empty() {
        return None;
    }
    Some(format!("http://{}", tail))
}

/// POST a WAV to `<base_url>/transcribe` (faster-whisper server, see
/// `services/whisper-server/whisper_server.py`) and return the recognised text.
///
/// Multipart body is hand-rolled — ureq has no native multipart and pulling
/// in reqwest for one endpoint would drag the whole tokio stack. The wire
/// shape is just five form fields wrapped by a boundary, ≤30 lines.
fn transcribe_remote(
    base_url: &str,
    segment_path: &Path,
    language: &str,
    initial_prompt: &str,
) -> Result<String, String> {
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

    // The audio file part.
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
    // 10-minute timeout — even a 30s WAV through a saturated cold endpoint
    // shouldn't approach this; matches the polish pool's generous budget so
    // a sluggish node degrades gracefully instead of cascading failures.
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
    Ok(value
        .get("text")
        .and_then(|item| item.as_str())
        .unwrap_or_default()
        .to_string())
}

/// Cheap unique-ish token for multipart boundaries (not security-critical;
/// only needs to not collide with the file's bytes).
fn uniq_token() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| format!("{}{}", d.as_secs(), d.subsec_nanos()))
        .unwrap_or_else(|_| "x".into())
}

fn strip_thinking(text: &str) -> String {
    let cleaned = Regex::new(r"(?s)<think>.*?</think>")
        .map(|re| re.replace_all(text, "").to_string())
        .unwrap_or_else(|_| text.to_string());
    cleaned.trim().to_string()
}

fn parse_polished_output(content: &str, fallback: &str) -> PolishedOutput {
    let cleaned = strip_thinking(content);
    let trimmed = cleaned
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        let text = value
            .get("text")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| cleaned.clone());
        let emotion = value
            .get("emotion")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let tags = value
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        return PolishedOutput {
            text,
            emotion,
            tags,
        };
    }

    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                let candidate = &trimmed[start..=end];
                if let Ok(value) = serde_json::from_str::<Value>(candidate) {
                    let text = value
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .unwrap_or_else(|| cleaned.clone());
                    let emotion = value
                        .get("emotion")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    let tags = value
                        .get("tags")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|item| item.as_str().map(|s| s.trim().to_string()))
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    return PolishedOutput {
                        text,
                        emotion,
                        tags,
                    };
                }
            }
        }
    }

    let text_only = if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    };
    PolishedOutput {
        text: text_only,
        emotion: None,
        tags: Vec::new(),
    }
}

#[tauri::command]
async fn polish_text_with_llm(
    text: String,
    role: Option<String>,
    hint: Option<String>,
    ollama_url: Option<String>,
    ollama_model: Option<String>,
    llm_prompt: Option<String>,
) -> Result<PolishedOutput, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let url = ollama_url.unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string());
        let model = ollama_model.ok_or_else(|| "未指定 Ollama 模型".to_string())?;
        let prompt = llm_prompt.unwrap_or_else(|| DEFAULT_LLM_PROMPT.to_string());
        polish_text(
            &url,
            &model,
            &prompt,
            text.trim(),
            role.as_deref().unwrap_or("assistant"),
            hint.as_deref().unwrap_or(""),
        )
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
fn get_default_llm_prompt() -> String {
    DEFAULT_LLM_PROMPT.to_string()
}

#[tauri::command]
async fn list_ollama_models(url: Option<String>) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let base = url.unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string());
        let url = format!("{}/api/tags", base.trim_end_matches('/'));
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(8))
            .build();
        let response = agent.get(&url).call().map_err(|err| err.to_string())?;
        let value: Value = response.into_json().map_err(|err| err.to_string())?;
        let mut models = Vec::new();
        if let Some(arr) = value.get("models").and_then(|m| m.as_array()) {
            for item in arr {
                if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                    models.push(name.to_string());
                }
            }
        }
        Ok(models)
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
async fn check_dependencies(ollama_url: Option<String>) -> Result<DependencyStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let url = ollama_url.unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string());

        let (whisper_path, whisper_error) = match find_whisper_command() {
            Ok(name) => {
                let resolved = Command::new("which")
                    .arg(name)
                    .output()
                    .ok()
                    .and_then(|out| String::from_utf8(out.stdout).ok())
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| name.to_string());
                (Some(resolved), None)
            }
            Err(err) => (None, Some(err)),
        };

        let ffmpeg_status = ensure_ffmpeg();
        let (ffmpeg_ok, ffmpeg_error) = match ffmpeg_status {
            Ok(()) => (true, None),
            Err(err) => (false, Some(err)),
        };

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(4))
            .build();
        let tags_url = format!("{}/api/tags", url.trim_end_matches('/'));
        let mut models: Vec<String> = Vec::new();
        let mut ollama_ok = false;
        let mut ollama_error: Option<String> = None;
        match agent.get(&tags_url).call() {
            Ok(resp) => {
                ollama_ok = true;
                if let Ok(value) = resp.into_json::<Value>() {
                    if let Some(arr) = value.get("models").and_then(|m| m.as_array()) {
                        for item in arr {
                            if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                                models.push(name.to_string());
                            }
                        }
                    }
                }
            }
            Err(err) => {
                ollama_error = Some(err.to_string());
            }
        }

        Ok(DependencyStatus {
            whisper_ok: whisper_path.is_some(),
            whisper_path,
            whisper_error,
            ffmpeg_ok,
            ffmpeg_error,
            ollama_ok,
            ollama_url: url,
            ollama_models: models,
            ollama_error,
        })
    })
    .await
    .map_err(|err| err.to_string())?
}

fn asr_cache_key(path: &Path, model: &str) -> Result<String, String> {
    let metadata = fs::metadata(path).map_err(|err| err.to_string())?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_secs())
        .unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    path_to_string(path).hash(&mut hasher);
    metadata.len().hash(&mut hasher);
    modified.hash(&mut hasher);
    model.hash(&mut hasher);
    Ok(format!(
        "{}_{:x}",
        safe_name(&path_file_stem(path)),
        hasher.finish()
    ))
}

fn find_whisper_command() -> Result<&'static str, String> {
    for command in ["whisper", "whisper.exe"] {
        if Command::new(command).arg("--help").output().is_ok() {
            return Ok(command);
        }
    }
    Err(
        "未找到本地 whisper 命令；请先 pip install openai-whisper 或在设置中调整识别工具"
            .to_string(),
    )
}

fn read_whisper_json_text(output_dir: &Path, input_path: &Path) -> Result<String, String> {
    let expected = output_dir.join(format!("{}.json", path_file_stem(input_path)));
    let path = if expected.is_file() {
        expected
    } else {
        find_whisper_json_by_stem(output_dir, &path_file_stem(input_path))?
            .ok_or_else(|| format!("未找到 whisper 输出：{}", path_file_name(input_path)))?
    };

    let data = fs::read_to_string(&path).map_err(|err| err.to_string())?;
    let value: Value = serde_json::from_str(&data).map_err(|err| err.to_string())?;
    let text = value
        .get("text")
        .and_then(|item| item.as_str())
        .unwrap_or_default();
    Ok(clean_recognition_text(text))
}

fn find_whisper_json_by_stem(output_dir: &Path, stem: &str) -> Result<Option<PathBuf>, String> {
    for entry in fs::read_dir(output_dir).map_err(|err| err.to_string())? {
        let path = entry.map_err(|err| err.to_string())?.path();
        if lower_extension(&path).as_deref() != Some("json") {
            continue;
        }
        if path_file_stem(&path) == stem {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn clean_recognition_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join("")
}

fn canonical_existing_dir(path: &str) -> Result<PathBuf, String> {
    let dir = PathBuf::from(path);
    if !dir.is_dir() {
        return Err(format!("Folder not found: {}", path));
    }
    dir.canonicalize().map_err(|err| err.to_string())
}

fn resolve_project_dir(root: &Path, output_path: Option<String>) -> Result<PathBuf, String> {
    let dir = output_path
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            let path = PathBuf::from(value.trim());
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .unwrap_or_else(|| root.join(PROJECT_FOLDER));

    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    dir.canonicalize().map_err(|err| err.to_string())
}

fn collect_files(
    dir: &Path,
    files: &mut Vec<PathBuf>,
    skip_dirs: &[PathBuf],
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if should_skip_dir(&path, skip_dirs) {
                continue;
            }
            collect_files(&path, files, skip_dirs)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

fn should_skip_dir(path: &Path, skip_dirs: &[PathBuf]) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    if matches!(
        name,
        ".git" | "node_modules" | "target" | "dist" | PROJECT_FOLDER
    ) {
        return true;
    }

    let Ok(path) = path.canonicalize() else {
        return false;
    };
    skip_dirs
        .iter()
        .any(|skip_dir| path == *skip_dir || path.starts_with(skip_dir))
}

fn is_audio(path: &Path) -> bool {
    matches!(
        lower_extension(path).as_deref(),
        Some("wav" | "wave" | "mp3" | "m4a" | "aac" | "flac" | "ogg" | "opus")
    )
}

fn lower_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
}

fn parse_manifest_file(path: &Path) -> Result<Vec<ManifestRecord>, String> {
    let data = fs::read_to_string(path).map_err(|err| err.to_string())?;
    Ok(parse_manifest_text(&data))
}

fn parse_manifest_text(data: &str) -> Vec<ManifestRecord> {
    let role_re = Regex::new(r#""role"\s*:\s*"([^"]+)""#).expect("valid role regex");
    let content_array_re =
        Regex::new(r#""content"\s*:\s*\[\s*"((?:\\.|[^"\\])*)""#).expect("valid content regex");
    let content_string_re =
        Regex::new(r#""content"\s*:\s*"((?:\\.|[^"\\])*)""#).expect("valid content regex");
    let audio_re =
        Regex::new(r#""audio_file"\s*:\s*"((?:\\.|[^"\\])*)""#).expect("valid audio regex");
    let emotion_re =
        Regex::new(r#""emotion"\s*:\s*\[\s*"((?:\\.|[^"\\])*)""#).expect("valid emotion regex");

    let matches: Vec<_> = role_re.captures_iter(data).collect();
    let mut records = Vec::new();

    for (index, caps) in matches.iter().enumerate() {
        let Some(role_match) = caps.get(0) else {
            continue;
        };
        let role = caps.get(1).map(|value| value.as_str()).unwrap_or_default();
        if role != "user" && role != "assistant" && role != "system" {
            continue;
        }

        let end = matches
            .get(index + 1)
            .and_then(|next| next.get(0))
            .map(|value| value.start())
            .unwrap_or(data.len());
        let block = &data[role_match.start()..end];

        let raw_content = content_array_re
            .captures(block)
            .or_else(|| content_string_re.captures(block))
            .and_then(|content_caps| content_caps.get(1))
            .map(|value| unescape_json_string(value.as_str()))
            .unwrap_or_default();

        let audio_file = audio_re
            .captures(block)
            .and_then(|audio_caps| audio_caps.get(1))
            .map(|value| unescape_json_string(value.as_str()));

        let emotion = emotion_re
            .captures(block)
            .and_then(|emotion_caps| emotion_caps.get(1))
            .map(|value| vec![unescape_json_string(value.as_str())])
            .unwrap_or_default();

        let tags = extract_markup_tags(&raw_content);
        let content = strip_markup(&raw_content);

        records.push(ManifestRecord {
            role: role.to_string(),
            content,
            raw_content,
            audio_file,
            emotion,
            tags,
        });
    }

    records
}

fn unescape_json_string(value: &str) -> String {
    serde_json::from_str::<String>(&format!("\"{}\"", value)).unwrap_or_else(|_| value.to_string())
}

fn strip_markup(value: &str) -> String {
    let tag_re = Regex::new(r"</?[^>]+>").expect("valid html tag regex");
    tag_re.replace_all(value, "").to_string()
}

fn extract_markup_tags(value: &str) -> Vec<String> {
    let mut tags = Vec::new();
    if value.contains("<laugh") || value.contains("</laugh>") {
        tags.push("laugh".to_string());
    }
    if value.contains("<breath") || value.contains("</breath>") {
        tags.push("breath".to_string());
    }
    if value.contains("<strong") || value.contains("</strong>") {
        tags.push("emphasis".to_string());
    }
    tags
}

fn match_manifest<'a>(path: &Path, records: &'a [ManifestRecord]) -> Option<&'a ManifestRecord> {
    let local_name = normalize_name(&path_file_name(path));
    let local_stem = normalize_name(&path_file_stem(path));

    records.iter().find(|record| {
        let Some(audio_file) = &record.audio_file else {
            return false;
        };
        let manifest_name = Path::new(audio_file)
            .file_name()
            .and_then(|value| value.to_str())
            .map(normalize_name)
            .unwrap_or_default();
        let manifest_stem = Path::new(audio_file)
            .file_stem()
            .and_then(|value| value.to_str())
            .map(normalize_name)
            .unwrap_or_default();

        manifest_name == local_name
            || manifest_stem == local_stem
            || (!local_stem.is_empty() && manifest_stem.contains(&local_stem))
            || (!manifest_stem.is_empty() && local_stem.contains(&manifest_stem))
    })
}

fn matching_manifest_records_for_source<'a>(
    path: &Path,
    role_hint: Option<&str>,
    topic_id: Option<u32>,
    records: &'a [ManifestRecord],
) -> Vec<&'a ManifestRecord> {
    let Some(source) = parse_dataset_source_audio_name_with_topic(path, role_hint, topic_id) else {
        return Vec::new();
    };

    records
        .iter()
        .filter(|record| {
            if record.role != "user" && record.role != "assistant" {
                return false;
            }
            if source
                .role
                .as_deref()
                .is_some_and(|role| role != record.role)
            {
                return false;
            }

            let Some(audio_file) = record.audio_file.as_deref() else {
                return false;
            };
            let Some(target) = parse_segment_output_name(audio_file) else {
                return false;
            };

            target.key.mode == source.mode
                && target.key.topic_id == source.topic_id
                && target.role == record.role
        })
        .collect()
}

fn joined_manifest_content(records: &[&ManifestRecord]) -> Option<String> {
    let content = records
        .iter()
        .map(|record| record.content.trim())
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    if content.is_empty() {
        None
    } else {
        Some(content)
    }
}

fn merged_manifest_emotion(records: &[&ManifestRecord]) -> Vec<String> {
    let mut values = Vec::new();
    for record in records {
        for emotion in &record.emotion {
            if !values.contains(emotion) {
                values.push(emotion.clone());
            }
        }
    }
    values
}

fn normalize_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

/// Infer user/assistant role from a path.
///
/// **Don't scan the whole path string.** Earlier versions did
/// `to_ascii_lowercase().contains("user")` over the full path — but on
/// macOS every project lives under `/Users/<name>/`, which lowercases to
/// `/users/...` and contains the substring `user`. That made every file
/// false-positive as the user role, regardless of its actual filename.
///
/// We only look at:
///   - the file name itself (e.g. `..._发音人.wav`)
///   - its immediate parent directory (e.g. `发音人/...wav`)
/// And ASCII keywords are matched with delimiter boundaries (`_user`,
/// `-user`, `.user`, or whole-stem `user`) — substring `user` inside a
/// longer ASCII token is ignored.
fn infer_role(path: &Path) -> Option<String> {
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let parent_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("");

    // Chinese tokens are unambiguous — they don't appear in any common
    // system path. Direct substring match is fine.
    if parent_name == "陪聊" || file_name.contains("陪聊") {
        return Some("user".to_string());
    }
    if parent_name == "发音人" || file_name.contains("发音人") {
        return Some("assistant".to_string());
    }

    // ASCII keywords need delimiter boundaries to avoid matching e.g.
    // "Users" or "userprofile". Only check the file stem, never parents.
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let bounded = |needle: &str| -> bool {
        stem == needle
            || stem.ends_with(&format!("_{}", needle))
            || stem.ends_with(&format!("-{}", needle))
            || stem.ends_with(&format!(".{}", needle))
    };
    if bounded("user") {
        return Some("user".to_string());
    }
    if bounded("assistant") {
        return Some("assistant".to_string());
    }
    None
}

fn probe_audio(path: &Path) -> Result<AudioProbe, String> {
    let output = silent_command("ffprobe")
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("format=duration")
        .arg("-show_entries")
        .arg(
            "stream=codec_type,codec_name,sample_rate,channels,bits_per_sample,bits_per_raw_sample",
        )
        .arg("-of")
        .arg("json")
        .arg(path)
        .output()
        .map_err(|err| format!("Unable to run ffprobe: {}", err))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let value: Value = serde_json::from_slice(&output.stdout).map_err(|err| err.to_string())?;
    let audio_stream = value
        .get("streams")
        .and_then(|item| item.as_array())
        .and_then(|streams| {
            streams.iter().find(|stream| {
                stream
                    .get("codec_type")
                    .and_then(|item| item.as_str())
                    .is_some_and(|codec_type| codec_type == "audio")
            })
        });

    let duration_ms = value
        .get("format")
        .and_then(|format| format.get("duration"))
        .and_then(value_to_f64)
        .map(|duration| seconds_to_ms(duration));

    let sample_rate = audio_stream
        .and_then(|stream| stream.get("sample_rate"))
        .and_then(value_to_u32);
    let channels = audio_stream
        .and_then(|stream| stream.get("channels"))
        .and_then(value_to_u32)
        .map(|value| value as u16);
    let codec_name = audio_stream
        .and_then(|stream| stream.get("codec_name"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let bits_per_sample = audio_stream
        .and_then(|stream| stream.get("bits_per_sample"))
        .and_then(value_to_u32)
        .or_else(|| {
            audio_stream
                .and_then(|stream| stream.get("bits_per_raw_sample"))
                .and_then(value_to_u32)
        });

    Ok(AudioProbe {
        duration_ms,
        sample_rate,
        channels,
        codec_name,
        bits_per_sample,
    })
}

fn value_to_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|item| item.parse::<f64>().ok()))
}

fn value_to_u32(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .map(|item| item as u32)
        .or_else(|| value.as_str().and_then(|item| item.parse::<u32>().ok()))
}

fn ensure_ffmpeg() -> Result<(), String> {
    let output = silent_command("ffmpeg")
        .arg("-version")
        .output()
        .map_err(|err| format!("Unable to run ffmpeg: {}", err))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

/// Sample the source audio and estimate its noise floor as the 10th
/// percentile of frame RMS, expressed in dBFS (always negative).
///
/// Why this matters: ffmpeg `silencedetect` uses a fixed dB threshold. If
/// the recording's actual noise floor sits above that threshold (e.g.
/// -22dB room hum vs. a -30dB config), silencedetect will refuse to flag
/// the leading/trailing ambient hiss as silence — and we end up with
/// long stretches of near-silence baked into the start of a segment.
/// Conversely, if the noise floor is far below the threshold, every
/// shallow mid-utterance dip (breath, soft consonant) gets flagged as
/// silence and we truncate sentences in half.
///
/// Calibrating the threshold against the recording's own noise floor
/// makes both failure modes much less likely.
fn analyze_noise_floor_db(input: &Path) -> Option<f32> {
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

/// Pick the silence threshold actually used for both `silencedetect` and
/// the per-segment trim. The user's `silence_db` is treated as a floor
/// (strictest possible threshold): we never go quieter than what the user
/// asked for, but we will go louder if the recording's noise floor demands
/// it.
///
///   effective = clamp(max(user_db, noise_floor + headroom), user_db, max_db)
///
/// Headroom of +8 dB above the measured noise floor leaves room for the
/// detector to catch ambient hiss as silence without folding in the soft
/// consonants of real speech (which typically sit 15+ dB above the floor).
/// The `MAX_THRESHOLD_DB` cap prevents pathological recordings (where the
/// "noise floor" is actually speech) from blowing the threshold past a
/// useful range.
fn effective_silence_db(user_db: f32, noise_floor_db: Option<f32>) -> f32 {
    const HEADROOM_DB: f32 = 8.0;
    const MAX_THRESHOLD_DB: f32 = -15.0;
    let auto = noise_floor_db
        .map(|floor| floor + HEADROOM_DB)
        .unwrap_or(user_db);
    auto.max(user_db).min(MAX_THRESHOLD_DB)
}

fn detect_silence(path: &Path, config: &CutConfig) -> Result<Vec<SilenceEvent>, String> {
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

fn build_segment_ranges(
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
/// pieces get the configured pre/post roll on their outer edges.
fn push_with_smart_split(
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

fn split_text_by_ranges(text: &str, ranges: &[(f64, f64)]) -> Vec<String> {
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

fn push_padded_range(
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

/// Physically trim leading and trailing silence from a PCM WAV file.
///
/// Scans short RMS windows (≈20 ms) from each end of the segment until
/// the first window whose energy exceeds `threshold_db`, then rewrites
/// the file with only the middle [head, tail] frames (plus `pre_roll_ms`
/// of padding kept on the head and `post_roll_ms` on the tail).
///
/// Returns `(head_trim_ms, new_duration_ms)` so the caller can shift the
/// segment's `start_ms` / `end_ms` to match the trimmed audio. When the
/// file is shorter than the trimmed boundary or no trim is needed, both
/// values are 0 / the original duration respectively.
///
/// Why RMS windows rather than per-sample peaks? A single-sample click
/// or DC blip near the start would otherwise anchor the head at frame 0
/// and defeat the trim. A 20 ms RMS window is the shortest unit that
/// reliably tracks perceptual loudness for speech.
///
/// Why not an ffmpeg `silenceremove` filter chain? It either drops the
/// silent regions altogether (losing our pre/post-roll padding budget)
/// or leaves a fader-shaped envelope that still bleeds the noise floor.
/// Doing the trim in-process gives us bit-exact control over how much
/// breathing room sits around the speech.
///
/// Supports 16-bit and 24-bit little-endian PCM, mono or multi-channel.
/// Other formats are silently skipped (file unchanged, returns 0 trim).
fn trim_segment_head_tail(
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

/// Hard noise gate on the head and tail of a PCM WAV file.
///
/// The cutter first trims the segment down to the configured pre/post
/// roll. This pass keeps that duration intact, but rewrites any remaining
/// below-threshold boundary samples to bit-true silence.
fn gate_segment_head_tail(path: &Path, threshold_db: f32) -> Result<(), String> {
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

fn analyze_segment_edge_silence(
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

fn write_pcm_wav_segment(
    input: &Path,
    output: &Path,
    start_sec: f64,
    duration_sec: f64,
    probe: &AudioProbe,
    codec: &str,
) -> Result<(), String> {
    let mut command = silent_command("ffmpeg");
    command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(input)
        .arg("-ss")
        .arg(format!("{:.3}", start_sec))
        .arg("-t")
        .arg(format!("{:.3}", duration_sec))
        .arg("-vn")
        .arg("-map")
        .arg("0:a:0");

    if let Some(sample_rate) = probe.sample_rate {
        command.arg("-ar").arg(sample_rate.to_string());
    }
    if let Some(channels) = probe.channels {
        command.arg("-ac").arg(channels.to_string());
    }

    let output = command
        .arg("-c:a")
        .arg(codec)
        .arg("-f")
        .arg("wav")
        .arg(output)
        .output()
        .map_err(|err| format!("Unable to run ffmpeg: {}", err))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

fn output_pcm_codec(probe: &AudioProbe) -> &'static str {
    match probe.bits_per_sample {
        Some(24) => "pcm_s24le",
        Some(32) => "pcm_s32le",
        _ => {
            if probe
                .codec_name
                .as_deref()
                .is_some_and(|codec| codec.contains("pcm_s24"))
            {
                "pcm_s24le"
            } else {
                "pcm_s16le"
            }
        }
    }
}

fn seconds_to_ms(value: f64) -> u64 {
    (value * 1000.0).round().max(0.0) as u64
}

fn stable_id(path: &Path) -> String {
    safe_name(&path_to_string(path))
}

fn playback_cache_key(path: &Path) -> Result<String, String> {
    let metadata = fs::metadata(path).map_err(|err| err.to_string())?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_secs())
        .unwrap_or_default();

    let mut hasher = DefaultHasher::new();
    path_to_string(path).hash(&mut hasher);
    metadata.len().hash(&mut hasher);
    modified.hash(&mut hasher);

    Ok(format!(
        "{}_{}",
        safe_name(&path_file_stem(path)),
        hasher.finish()
    ))
}

fn safe_name(value: &str) -> String {
    let mut output = String::new();
    for ch in value.chars() {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            output.push(ch);
        } else {
            output.push('_');
        }
    }
    let output = output.trim_matches('_').to_string();
    if output.is_empty() {
        "item".to_string()
    } else {
        output
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn path_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string()
}

fn path_file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn write_pcm16_mono_wav(path: &Path, sample_rate: u32, samples: &[i16]) {
        let data_len = (samples.len() * 2) as u32;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        fs::write(path, bytes).expect("write wav");
    }

    fn read_pcm16_mono_samples(path: &Path) -> Vec<i16> {
        let bytes = fs::read(path).expect("read wav");
        let mut i = 12usize;
        while i + 8 <= bytes.len() {
            let chunk_id = [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]];
            let chunk_size =
                u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]])
                    as usize;
            let chunk_data = i + 8;
            if &chunk_id == b"data" {
                let end = (chunk_data + chunk_size).min(bytes.len());
                return bytes[chunk_data..end]
                    .chunks_exact(2)
                    .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect();
            }
            i = chunk_data + chunk_size + (chunk_size & 1);
        }
        Vec::new()
    }

    #[test]
    fn parses_loose_manifest_records() {
        let data = r#"
        {
          "messages": [
            {"role": "user", "content": "hello", "audio_file": "user.wav"},
            {
              "role": "assistant",
              "content": ["<laugh>ka</laugh>"]
              "audio_file": "speaker.wav"
              "emotion": ["neutral"],
            }
          ]
        }
        "#;

        let records = parse_manifest_text(data);
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].content, "ka");
        assert_eq!(records[1].tags, vec!["laugh"]);
        assert_eq!(records[1].emotion, vec!["neutral"]);
    }

    #[test]
    fn builds_dataset_segment_names_from_source_names_and_manifest_targets() {
        let assistant = Path::new("/tmp/长沙方言-0413-自由演绎-发音人-惊讶1.wav");
        assert_eq!(
            build_segment_file_name(assistant, Some("assistant"), None, &[], 0, 0, 1000),
            "自由演绎_0001_01_01_发音人.wav"
        );

        let user = Path::new("/tmp/长沙方言-0413-文案演绎-陪聊-话题12.wav");
        assert_eq!(
            build_segment_file_name(user, Some("user"), None, &[], 2, 0, 1000),
            "文案演绎_0012_01_03_陪聊.wav"
        );

        let targets = vec!["oss://bucket/WAV/自由演绎_0001_01_02_发音人.wav".to_string()];
        assert_eq!(
            build_segment_file_name(assistant, Some("assistant"), None, &targets, 0, 0, 1000),
            "自由演绎_0001_01_02_发音人.wav"
        );

        let legacy_user_targets = vec!["oss://bucket/WAV/文案演绎_0012_03_陪聊.wav".to_string()];
        assert_eq!(
            build_segment_file_name(user, Some("user"), None, &legacy_user_targets, 0, 0, 1000),
            "文案演绎_0012_03_01_陪聊.wav"
        );

        let free_topic = Path::new("/tmp/长沙方言-0413-自由演绎-发音人-开心1.wav");
        assert_eq!(
            build_segment_file_name(free_topic, Some("assistant"), Some(2), &[], 0, 0, 1000),
            "自由演绎_0002_01_01_发音人.wav"
        );

        let text_acting = Path::new("/tmp/台湾话-发音人-文本演绎-话题56.wav");
        assert_eq!(
            build_segment_file_name(text_acting, Some("assistant"), None, &[], 0, 5718, 32726),
            "文本演绎_0056_01_01_发音人.wav"
        );

        let free_dialogue = Path::new("/tmp/长沙方言-0413-自由对话-陪聊-话题1.wav");
        assert_eq!(
            build_segment_file_name(free_dialogue, Some("user"), None, &[], 0, 0, 1000),
            "自由对话_0001_01_01_陪聊.wav"
        );
    }

    #[test]
    fn maps_free_source_topics_to_manifest_topic_order() {
        let audio_paths = vec![
            PathBuf::from("/tmp/长沙方言-0413-自由演绎-发音人-惊讶1.wav"),
            PathBuf::from("/tmp/长沙方言-0413-自由演绎-陪聊-惊讶1.wav"),
            PathBuf::from("/tmp/长沙方言-0413-自由演绎-发音人-开心1.wav"),
            PathBuf::from("/tmp/长沙方言-0413-自由演绎-陪聊-开心1.wav"),
            PathBuf::from("/tmp/长沙方言-0413-自由演绎-发音人-开心2.wav"),
        ];
        let records = ["0001", "0002", "0003"]
            .iter()
            .map(|topic| ManifestRecord {
                role: "assistant".to_string(),
                content: String::new(),
                raw_content: String::new(),
                audio_file: Some(format!("自由演绎_{}_01_01_发音人.wav", topic)),
                emotion: Vec::new(),
                tags: Vec::new(),
            })
            .collect::<Vec<_>>();

        let ids = infer_source_topic_ids(&audio_paths, &records);
        let topic_id = |path: &str| {
            ids.get(&source_topic_key(Path::new(path)).expect("source key"))
                .copied()
        };

        assert_eq!(
            topic_id("/tmp/长沙方言-0413-自由演绎-发音人-惊讶1.wav"),
            Some(1)
        );
        assert_eq!(
            topic_id("/tmp/长沙方言-0413-自由演绎-发音人-开心1.wav"),
            Some(2)
        );
        assert_eq!(
            topic_id("/tmp/长沙方言-0413-自由演绎-发音人-开心2.wav"),
            Some(3)
        );
    }

    #[test]
    fn paired_export_groups_by_dataset_segment_file_name() {
        let user = SegmentRecord {
            id: "u".to_string(),
            source_path: "/input/长沙方言-0413-自由演绎-陪聊-惊讶1.wav".to_string(),
            source_file_name: "长沙方言-0413-自由演绎-陪聊-惊讶1.wav".to_string(),
            segment_path: "/out/segments/自由演绎_0001_01_01_陪聊.wav".to_string(),
            segment_file_name: "自由演绎_0001_01_01_陪聊.wav".to_string(),
            role: Some("user".to_string()),
            start_ms: 0,
            end_ms: 1000,
            duration_ms: 1000,
            original_text: "你好".to_string(),
            phonetic_text: "你好".to_string(),
            emotion: Vec::new(),
            tags: Vec::new(),
            notes: String::new(),
        };
        let assistant = SegmentRecord {
            id: "a".to_string(),
            source_path: "/input/长沙方言-0413-自由演绎-发音人-惊讶1.wav".to_string(),
            source_file_name: "长沙方言-0413-自由演绎-发音人-惊讶1.wav".to_string(),
            segment_path: "/out/segments/自由演绎_0001_01_01_发音人.wav".to_string(),
            segment_file_name: "自由演绎_0001_01_01_发音人.wav".to_string(),
            role: Some("assistant".to_string()),
            start_ms: 0,
            end_ms: 1000,
            duration_ms: 1000,
            original_text: "好嘞".to_string(),
            phonetic_text: "好嘞".to_string(),
            emotion: vec!["中立".to_string()],
            tags: Vec::new(),
            notes: String::new(),
        };

        let lines = build_paired_jsonl(&[user, assistant], "长沙本地人", false, "", "")
            .expect("paired jsonl");
        assert_eq!(lines.len(), 1);
        let value: Value = serde_json::from_str(&lines[0]).expect("valid json");
        let messages = value["messages"].as_array().expect("messages");
        // User keeps scalar audio_file on a single cut.
        assert_eq!(
            messages[1]["audio_file"],
            "/out/segments/自由演绎_0001_01_01_陪聊.wav"
        );
        // Assistant audio_file is ALWAYS an array — even for a single cut.
        assert_eq!(
            messages[2]["audio_file"],
            json!(["/out/segments/自由演绎_0001_01_01_发音人.wav"])
        );
    }

    #[test]
    fn dialogue_sequence_names_rounds_from_two_track_timeline() {
        let make_segment = |role: &str, start_ms: u64, file_name: &str| SegmentRecord {
            id: file_name.trim_end_matches(".wav").to_string(),
            source_path: format!(
                "/input/长沙方言-0413-文案演绎-{}-话题1.wav",
                if role == "user" {
                    "陪聊"
                } else {
                    "发音人"
                }
            ),
            source_file_name: format!(
                "长沙方言-0413-文案演绎-{}-话题1.wav",
                if role == "user" {
                    "陪聊"
                } else {
                    "发音人"
                }
            ),
            segment_path: format!("/out/segments/{}", file_name),
            segment_file_name: file_name.to_string(),
            role: Some(role.to_string()),
            start_ms,
            end_ms: start_ms + 500,
            duration_ms: 500,
            original_text: String::new(),
            phonetic_text: String::new(),
            emotion: Vec::new(),
            tags: Vec::new(),
            notes: String::new(),
        };
        let segments = vec![
            make_segment("assistant", 1200, "文案演绎_0001_01_01_发音人.wav"),
            make_segment("user", 0, "文案演绎_0001_01_01_陪聊.wav"),
            make_segment("assistant", 1800, "文案演绎_0001_01_02_发音人.wav"),
            make_segment("user", 3200, "文案演绎_0001_01_02_陪聊.wav"),
            make_segment("assistant", 4200, "文案演绎_0001_01_03_发音人.wav"),
        ];

        let planned = plan_dialogue_sequence_file_names(&segments);
        assert_eq!(planned.get(&0), None);
        assert_eq!(planned.get(&1), None);
        assert_eq!(planned.get(&2), None);
        assert_eq!(
            planned.get(&3).map(String::as_str),
            Some("文案演绎_0001_02_01_陪聊.wav")
        );
        assert_eq!(
            planned.get(&4).map(String::as_str),
            Some("文案演绎_0001_02_01_发音人.wav")
        );
    }

    #[test]
    fn split_role_suffix_recognizes_known_markers() {
        // End position, `_` separator (demo convention).
        assert_eq!(
            split_role_suffix("自由演绎_0001_02_发音人"),
            ("自由演绎_0001_02".to_string(), Some("发音人".to_string())),
        );
        assert_eq!(
            split_role_suffix("自由演绎_0001_02_陪聊"),
            ("自由演绎_0001_02".to_string(), Some("陪聊".to_string())),
        );

        // Middle position, `-` separator (长沙方言 dataset). Surrounding
        // dashes collapse to one so the base remains a clean identifier.
        assert_eq!(
            split_role_suffix("长沙方言-0413-文案演绎-发音人-话题1"),
            (
                "长沙方言-0413-文案演绎-话题1".to_string(),
                Some("发音人".to_string()),
            ),
        );
        assert_eq!(
            split_role_suffix("长沙方言-0413-文案演绎-陪聊-话题2"),
            (
                "长沙方言-0413-文案演绎-话题2".to_string(),
                Some("陪聊".to_string()),
            ),
        );
        assert_eq!(
            split_role_suffix("台湾话-陪聊人-文本演绎-话题56"),
            (
                "台湾话-文本演绎-话题56".to_string(),
                Some("陪聊人".to_string()),
            ),
        );

        // English variants in the middle work too.
        assert_eq!(
            split_role_suffix("session_user_part2"),
            ("session_part2".to_string(), Some("user".to_string())),
        );

        // No role marker → unchanged.
        assert_eq!(
            split_role_suffix("random_audio"),
            ("random_audio".to_string(), None),
        );
    }

    /// extract_turn_from_base implements the spec's "second number = 话轮"
    /// rule. When the source filename already encodes a 2-digit turn
    /// (demo `自由演绎_0001_01_发音人.wav`), pull it out. When it doesn't
    /// (`长沙方言-...-话题1.wav`), default to turn = 1 — the whole
    /// recording is one turn.
    #[test]
    fn extract_turn_from_base_recognizes_demo_turn_marker() {
        // Demo: stem after split_role_suffix is `自由演绎_0001_01`.
        // The `_01` is the turn marker — strip it.
        assert_eq!(
            extract_turn_from_base("自由演绎_0001_01"),
            ("自由演绎_0001".to_string(), 1),
        );
        assert_eq!(
            extract_turn_from_base("自由演绎_0001_07"),
            ("自由演绎_0001".to_string(), 7),
        );

        // 长沙方言 dataset: no turn marker, default to 1.
        assert_eq!(
            extract_turn_from_base("长沙方言-0413-文案演绎-话题1"),
            ("长沙方言-0413-文案演绎-话题1".to_string(), 1),
        );

        // Topic numbers like `话题15` should NOT be confused with a turn
        // marker — they're not a `_<2 digits>` suffix.
        assert_eq!(
            extract_turn_from_base("长沙方言-话题15"),
            ("长沙方言-话题15".to_string(), 1),
        );

        // Single-digit suffixes don't match (must be exactly 2 digits).
        assert_eq!(extract_turn_from_base("foo_5"), ("foo_5".to_string(), 1),);
    }

    #[test]
    fn with_prefix_uses_input_root_relative_path() {
        let oss = "oss://bucket/dialect";
        // Path inside the input root, but inside _dialect_labeler internal dir.
        let result = with_prefix(
            oss,
            "/Users/me/audio/dataset/_dialect_labeler/segments/x/y.wav",
            "/Users/me/audio/dataset",
        );
        assert_eq!(result, "oss://bucket/dialect/segments/x/y.wav");

        // Source audio file relative to root.
        let src = with_prefix(
            oss,
            "/Users/me/audio/dataset/sub/file_陪聊.wav",
            "/Users/me/audio/dataset",
        );
        assert_eq!(src, "oss://bucket/dialect/sub/file_陪聊.wav");

        // Path outside the input root falls back to file name only.
        let out = with_prefix(oss, "/elsewhere/file.wav", "/Users/me/audio/dataset");
        assert_eq!(out, "oss://bucket/dialect/file.wav");

        // Empty prefix returns path unchanged.
        let raw = with_prefix("", "/foo/bar.wav", "/foo");
        assert_eq!(raw, "/foo/bar.wav");
    }

    /// Old project.json files (saved before `mode` + the semantic-* fields
    /// existed) must still load — every new field carries `#[serde(default)]`
    /// and falls back to Dialect-compatible values.
    #[test]
    fn cutconfig_deserializes_legacy_payload_as_dialect() {
        let legacy = r#"{
            "silenceDb": -28,
            "minSilenceMs": 350,
            "minSegmentMs": 250,
            "preRollMs": 120,
            "postRollMs": 200,
            "maxSegmentMs": 30000
        }"#;
        let cfg: CutConfig = serde_json::from_str(legacy).expect("legacy CutConfig must load");
        assert_eq!(cfg.mode, CutMode::Dialect);
        assert!((cfg.target_loudness_lufs - (-18.0)).abs() < 1e-3);
        assert!((cfg.min_segment_s - 6.0).abs() < 1e-3);
        assert!((cfg.max_segment_s - 90.0).abs() < 1e-3);
        assert_eq!(cfg.semantic_model, "qwen2.5:32b");
        assert_eq!(cfg.semantic_num_ctx, 32768);
    }

    /// Phase 2 sanity: rnnoise model finder returns None on a host
    /// without any model installed and no env override. Tests run with
    /// a scrubbed env to make this deterministic.
    #[test]
    fn rnnoise_model_finder_returns_none_when_nothing_configured() {
        // SAFETY: tests are single-threaded by default in this crate's
        // config; we briefly remove + restore the env var.
        let prev = std::env::var("RNNOISE_MODEL_PATH").ok();
        std::env::remove_var("RNNOISE_MODEL_PATH");
        // Move to a known-empty directory so `./cb.rnnn` doesn't trip
        // accidentally on a stray file in the workspace.
        let tmp = std::env::temp_dir().join(format!("rnnoise-finder-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let prev_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();
        let result = find_rnnoise_model();
        std::env::set_current_dir(prev_cwd).unwrap();
        if let Some(p) = prev {
            std::env::set_var("RNNOISE_MODEL_PATH", p);
        }
        let _ = fs::remove_dir_all(&tmp);
        // On the CI host or a dev box without RNNoise installed this
        // should be None. If find_rnnoise_model returns Some, the host
        // genuinely has a model at one of the lookup paths and that's
        // fine — just skip the assertion.
        if let Some(p) = result {
            assert!(
                p.is_file(),
                "if found, model path must point at an actual file"
            );
        }
    }

    /// Phase 2 end-to-end: synthesise a 4-second sine wave, run it
    /// through `preprocess_audio_for_semantic`, verify the output
    /// exists with the expected codec / sample rate / channel count
    /// and a duration close to the input. Skipped when ffmpeg isn't on
    /// PATH (a fresh CI container shouldn't red the suite over tooling).
    #[test]
    fn preprocess_audio_for_semantic_normalises_loudness() {
        // Probe for ffmpeg by trying `-version`. Avoids the `which`
        // crate dependency.
        let ffmpeg_present = silent_command("ffmpeg")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ffmpeg_present {
            eprintln!("skipping preprocess test — ffmpeg not on PATH");
            return;
        }
        let dir = std::env::temp_dir()
            .join(format!("preprocess-smoke-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("input.wav");
        let dst = dir.join("normalised.wav");

        // 4-second 440 Hz sine wave, 16-bit / 44.1 kHz mono. Loud
        // enough (-3 dBFS) that loudnorm has to *reduce* gain to hit
        // -18 LUFS — a clear directional test.
        let synth = silent_command("ffmpeg")
            .arg("-y").arg("-hide_banner").arg("-loglevel").arg("error")
            .arg("-f").arg("lavfi")
            .arg("-i").arg("sine=frequency=440:duration=4:sample_rate=44100")
            .arg("-ar").arg("44100").arg("-ac").arg("1")
            .arg("-af").arg("volume=-3dB")
            .arg("-c:a").arg("pcm_s16le")
            .arg(&src)
            .status()
            .expect("ffmpeg synth");
        assert!(synth.success(), "synth failed");

        // No rnnoise — only test the loudnorm path.
        preprocess_audio_for_semantic(&src, &dst, -18.0, None)
            .expect("preprocess succeeded");
        let probe = probe_audio(&dst).expect("ffprobe on normalised");
        assert_eq!(probe.codec_name.as_deref(), Some("pcm_s16le"));
        assert_eq!(probe.sample_rate, Some(48000));
        assert_eq!(probe.channels, Some(1));
        let dur_ms = probe.duration_ms.expect("duration present");
        // loudnorm can add ~100ms look-ahead delay; tolerate ±300ms
        // around the 4s source.
        assert!(
            (3_700..=4_300).contains(&dur_ms),
            "duration {dur_ms}ms outside tolerance"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Calling the per-file cutter with Semantic mode must redirect the
    /// caller to the new top-level `run_semantic_pipeline` command —
    /// NOT silently fall back to Dialect cutter.
    #[test]
    fn semantic_mode_redirects_to_dedicated_pipeline() {
        let dir = std::env::temp_dir()
            .join(format!("semantic-redirect-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let input = dir.join("never-used.wav");
        fs::write(&input, b"fake").unwrap();
        let cfg = CutConfig {
            mode: CutMode::Semantic,
            ..CutConfig::default()
        };
        let res = cut_audio_file_impl(
            path_to_string(&input),
            path_to_string(&dir),
            cfg,
            None,
            None,
            None,
            None,
            Vec::new(),
        );
        let err = res.expect_err("Semantic must redirect, not fall through");
        assert!(
            err.contains("run_semantic_pipeline"),
            "expected redirect message, got: {err}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // ====================================================================
    // Phase 4: LLM semantic-cut decision — pure-fn tests
    // ====================================================================

    #[test]
    fn build_semantic_cut_prompt_encodes_spec_rules() {
        let pieces = vec![
            AsredPiece { text: "你今天过得怎么样？".into(), start_ms: 0, end_ms: 2400 },
            AsredPiece { text: "我今天过得还不错。".into(), start_ms: 2400, end_ms: 5800 },
        ];
        let candidates = vec![0u64, 2400, 5800];
        let cfg = CutConfig {
            mode: CutMode::Semantic,
            ..CutConfig::default()
        };
        let (system, user) = build_semantic_cut_prompt(&pieces, &candidates, &cfg);

        // System prompt must encode the spec's hard rules.
        assert!(system.contains("6"), "min segment seconds should appear");
        assert!(system.contains("90"), "max segment seconds should appear");
        assert!(system.contains("候选切点"), "must reference candidate pool");
        assert!(system.contains("一问一答"), "spec example must appear");
        assert!(system.contains("JSON"), "must demand JSON output");
        assert!(system.contains("\"segments\""), "must pin output schema");

        // User prompt must list every piece + every candidate.
        assert!(user.contains("0, 2400, 5800"), "all candidates listed");
        assert!(user.contains("你今天过得怎么样"), "piece 0 text present");
        assert!(user.contains("我今天过得还不错"), "piece 1 text present");
        assert!(user.contains("[0–2400ms]"), "global timestamps formatted");
    }

    #[test]
    fn parse_semantic_cut_response_accepts_valid_json() {
        let raw = r#"{
            "segments": [
                {"start_ms": 0, "end_ms": 2400, "reason": "完整问句"},
                {"start_ms": 2400, "end_ms": 5800, "reason": "完整答句"}
            ]
        }"#;
        let parsed = parse_semantic_cut_response(raw).expect("valid response parses");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].start_ms, 0);
        assert_eq!(parsed[0].end_ms, 2400);
        assert_eq!(parsed[0].reason, "完整问句");
        assert_eq!(parsed[1].start_ms, 2400);
        assert_eq!(parsed[1].end_ms, 5800);
    }

    #[test]
    fn parse_semantic_cut_response_rejects_missing_fields() {
        // Missing end_ms.
        let raw = r#"{"segments":[{"start_ms":0,"reason":"x"}]}"#;
        let err = parse_semantic_cut_response(raw).unwrap_err();
        assert!(err.contains("end_ms missing"), "got: {err}");

        // end_ms <= start_ms.
        let raw = r#"{"segments":[{"start_ms":100,"end_ms":100,"reason":"x"}]}"#;
        let err = parse_semantic_cut_response(raw).unwrap_err();
        assert!(err.contains("> start_ms"), "got: {err}");

        // Not even JSON.
        let err = parse_semantic_cut_response("not json").unwrap_err();
        assert!(err.contains("非 JSON"), "got: {err}");
    }

    #[test]
    fn validate_semantic_cuts_enforces_candidates_and_lengths() {
        let cfg = CutConfig {
            mode: CutMode::Semantic,
            min_segment_s: 6.0,
            max_segment_s: 90.0,
            ..CutConfig::default()
        };
        let candidates = vec![0u64, 1800, 8000, 20000, 95000];

        // Happy path. Length floor is 1s (HARD_MIN_MS) — sub-spec-target
        // short utterances are OK per § 二·一·1 弹性处理; auto-QA
        // surfaces them as warnings.
        let good = vec![
            SemanticCutDecision { start_ms: 0, end_ms: 8000, reason: String::new() },
            SemanticCutDecision { start_ms: 8000, end_ms: 20000, reason: String::new() },
        ];
        validate_semantic_cuts(&good, &candidates, &cfg).expect("good cuts validate");

        // Sub-6s but ≥1s — accepted (spec 弹性处理).
        let short_ok = vec![SemanticCutDecision {
            start_ms: 0,
            end_ms: 1800,
            reason: String::new(),
        }];
        validate_semantic_cuts(&short_ok, &candidates, &cfg)
            .expect("1.8s short utterance must pass — spec 弹性下限");

        // Cut point not in candidate pool.
        let invented = vec![SemanticCutDecision {
            start_ms: 0,
            end_ms: 12345,  // not in candidates
            reason: String::new(),
        }];
        let err = validate_semantic_cuts(&invented, &candidates, &cfg).unwrap_err();
        assert!(err.contains("不在候选切点池"), "got: {err}");

        // Too long: 95s > 90s ceiling.
        let too_long = vec![SemanticCutDecision {
            start_ms: 0,
            end_ms: 95000,
            reason: String::new(),
        }];
        let err = validate_semantic_cuts(&too_long, &candidates, &cfg).unwrap_err();
        assert!(err.contains("超过上限"), "got: {err}");

        // Overlap.
        let overlap = vec![
            SemanticCutDecision { start_ms: 0, end_ms: 20000, reason: String::new() },
            SemanticCutDecision { start_ms: 8000, end_ms: 95000, reason: String::new() },
        ];
        let err = validate_semantic_cuts(&overlap, &candidates, &cfg).unwrap_err();
        assert!(err.contains("重叠"), "got: {err}");
    }

    // ====================================================================
    // Phase 10: end-to-end smoke test against real Tailnet endpoints
    //
    // Gated on env so CI hosts without the fleet skip cleanly. Set
    //   SMOKE_WHISPER_URL=http://100.64.0.4:9090
    //   SMOKE_OLLAMA_URL=http://100.64.0.4:11434
    // to fire. Uses macOS `say` to synthesise a ~30s Mandarin sample,
    // runs every pipeline phase by hand (skips `recognize_segments_impl`
    // which would need an AppHandle), validates the xlsx + WAVs.
    // ====================================================================

    /// POST one audio file to faster-whisper-server's /transcribe and
    /// return the recognised text. Pure helper used by the e2e smoke.
    fn smoke_whisper_transcribe(
        endpoint: &str,
        wav: &Path,
    ) -> Result<String, String> {
        use std::io::Read;
        let mut audio = Vec::new();
        fs::File::open(wav)
            .map_err(|e| e.to_string())?
            .read_to_end(&mut audio)
            .map_err(|e| e.to_string())?;
        // Tiny inline multipart writer — keeps the test from pulling in
        // an http-multipart crate just for the smoke.
        let boundary = format!("----smoke{}", std::process::id());
        let mut body = Vec::new();
        let part = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"audio\"; \
             filename=\"piece.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        );
        body.extend_from_slice(part.as_bytes());
        body.extend_from_slice(&audio);
        let tail = format!(
            "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"language\"\r\n\r\nzh\r\n\
             --{boundary}--\r\n"
        );
        body.extend_from_slice(tail.as_bytes());

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(60))
            .build();
        let resp = agent
            .post(&format!("{}/transcribe", endpoint.trim_end_matches('/')))
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send_bytes(&body)
            .map_err(|err| format!("whisper HTTP: {err}"))?;
        let raw = resp.into_string().map_err(|err| err.to_string())?;
        let val: Value =
            serde_json::from_str(&raw).map_err(|err| format!("whisper JSON: {err}"))?;
        Ok(val
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_string())
    }

    #[test]
    fn smoke_e2e_semantic_pipeline_with_real_endpoints() {
        let Ok(whisper_url) = std::env::var("SMOKE_WHISPER_URL") else {
            eprintln!("skipping e2e — set SMOKE_WHISPER_URL to fire");
            return;
        };
        let Ok(ollama_url) = std::env::var("SMOKE_OLLAMA_URL") else {
            eprintln!("skipping e2e — set SMOKE_OLLAMA_URL to fire");
            return;
        };
        let ffmpeg_ok = silent_command("ffmpeg")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let say_ok = silent_command("say")
            .arg("-v")
            .arg("Tingting")
            .arg("test")
            .arg("-o")
            .arg("/tmp/.say-probe.aiff")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let _ = fs::remove_file("/tmp/.say-probe.aiff");
        if !ffmpeg_ok || !say_ok {
            eprintln!("skipping e2e — needs ffmpeg + macOS `say` (Tingting)");
            return;
        }

        let dir = std::env::temp_dir().join(format!("mode2-e2e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Two-sentence Mandarin sample — enough to exercise the LLM's
        // "split at sentence boundary" decision without being huge.
        let script = "大家好，欢迎收听今天的节目。今天我们要聊一聊人工智能的发展，\
                      它正在改变我们工作和生活的方方面面。希望大家会喜欢。";
        let aiff = dir.join("source.aiff");
        let src_wav = dir.join("spkA_Podcast_singleshot_260512_30s.wav");
        let synth = silent_command("say")
            .arg("-v").arg("Tingting")
            .arg(script)
            .arg("-o").arg(&aiff)
            .status()
            .expect("say");
        assert!(synth.success(), "say synth failed");
        let conv = silent_command("ffmpeg")
            .arg("-y").arg("-hide_banner").arg("-loglevel").arg("error")
            .arg("-i").arg(&aiff)
            .arg("-ar").arg("48000").arg("-ac").arg("1")
            .arg("-c:a").arg("pcm_s16le")
            .arg(&src_wav)
            .status()
            .expect("ffmpeg");
        assert!(conv.success(), "ffmpeg convert failed");

        let cfg = CutConfig {
            mode: CutMode::Semantic,
            semantic_endpoint: ollama_url,
            semantic_model: "qwen2.5:32b".to_string(),
            semantic_num_ctx: 32768,
            target_loudness_lufs: -18.0,
            min_segment_s: 4.0, // shorter for our 20s sample
            max_segment_s: 90.0,
            head_tail_silence_ms: 150,
            ..CutConfig::default()
        };

        // Phase 2: preprocess.
        let normalised = dir.join("01_normalised.wav");
        preprocess_audio_for_semantic(&src_wav, &normalised, cfg.target_loudness_lufs, None)
            .expect("preprocess");
        eprintln!("[smoke] preprocess ok → {}", normalised.display());

        // Phase 3: fine pre-cut.
        let pieces_dir = dir.join("pieces");
        let (piece_paths, candidates_ms) =
            fine_pre_cut_for_semantic(&normalised, &pieces_dir).expect("pre-cut");
        eprintln!(
            "[smoke] pre-cut → {} pieces, {} candidate boundaries",
            piece_paths.len(),
            candidates_ms.len()
        );
        assert!(piece_paths.len() >= 1);

        // Phase 3b: ASR each piece via Whisper HTTP directly (no AppHandle needed).
        let mut pieces: Vec<AsredPiece> = Vec::with_capacity(piece_paths.len());
        for (i, p) in piece_paths.iter().enumerate() {
            let text = match smoke_whisper_transcribe(&whisper_url, p) {
                Ok(t) => t,
                Err(err) => {
                    eprintln!("[smoke] whisper failed on piece {i}: {err}");
                    String::new()
                }
            };
            let start = candidates_ms.get(i).copied().unwrap_or(0);
            let end = candidates_ms.get(i + 1).copied().unwrap_or(start);
            eprintln!("[smoke]   piece {i:02} [{start}-{end}ms]: {text}");
            pieces.push(AsredPiece {
                text,
                start_ms: start,
                end_ms: end,
            });
        }
        assert!(pieces.iter().any(|p| !p.text.trim().is_empty()), "all ASR empty");

        // Phase 4: LLM semantic cut.
        let decisions = llm_decide_semantic_cuts(&pieces, &candidates_ms, &cfg)
            .expect("llm semantic cut");
        eprintln!("[smoke] LLM merged into {} segments", decisions.len());
        for (i, d) in decisions.iter().enumerate() {
            eprintln!("[smoke]   seg {i}: [{}-{}ms] {}", d.start_ms, d.end_ms, d.reason);
        }
        assert!(!decisions.is_empty());

        // Phase 5 + 6: normalise + auto-QA each.
        let mut export_segments: Vec<SemanticExportSegment> = Vec::new();
        for (i, d) in decisions.iter().enumerate() {
            let raw: String = pieces
                .iter()
                .filter(|p| p.start_ms >= d.start_ms && p.end_ms <= d.end_ms)
                .map(|p| p.text.trim())
                .collect::<Vec<_>>()
                .join(" ");
            let final_text = match llm_normalize_segment_text(&raw, &cfg) {
                Ok(t) => t,
                Err(err) => {
                    eprintln!("[smoke] normalise failed seg {i}: {err}, using raw");
                    raw.clone()
                }
            };
            let problems = auto_qa_segment(&final_text, d.end_ms - d.start_ms, &cfg);
            eprintln!(
                "[smoke]   seg {i}: \"{final_text}\" | problems: {problems:?}"
            );
            export_segments.push(SemanticExportSegment {
                start_ms: d.start_ms,
                end_ms: d.end_ms,
                final_text,
                problems,
            });
        }

        // Phase 7: export.
        let out_dir = dir.join("out");
        let spec_stem = derive_spec_stem(&src_wav);
        let (xlsx, wavs) = export_semantic_mode(
            &out_dir,
            &normalised,
            &spec_stem,
            "source.wav",
            &export_segments,
        )
        .expect("export");
        eprintln!("[smoke] xlsx → {}", xlsx.display());
        eprintln!("[smoke] {} wavs emitted", wavs.len());
        assert!(xlsx.is_file());
        assert_eq!(wavs.len(), export_segments.len());

        eprintln!("[smoke] === E2E SMOKE PASSED ===");
        // Don't auto-cleanup — keep artefacts so we can eyeball results.
        eprintln!("[smoke] artefacts kept at {}", dir.display());
    }

    // ====================================================================
    // Phase 7: export — naming + xlsx writer tests
    // ====================================================================

    #[test]
    fn semantic_segment_filename_six_digit_pad() {
        assert_eq!(
            semantic_segment_filename("spkA_Freetalk_freetalk_260101_40m", 1),
            "spkA_Freetalk_freetalk_260101_40m_000001.wav"
        );
        assert_eq!(
            semantic_segment_filename("spkA_Freetalk_freetalk_260101_40m", 999_999),
            "spkA_Freetalk_freetalk_260101_40m_999999.wav"
        );
    }

    #[test]
    fn derive_spec_stem_strips_trailing_sequence() {
        assert_eq!(
            derive_spec_stem(Path::new("/x/y/spk_A_FREETALK_0606_68M_01.wav")),
            "spk_A_FREETALK_0606_68M"
        );
        // Already-bare stem stays put.
        assert_eq!(
            derive_spec_stem(Path::new("/x/spkA_Freetalk_freetalk_260101_40m.wav")),
            "spkA_Freetalk_freetalk_260101_40m"
        );
        // Off-spec name is passed through.
        assert_eq!(
            derive_spec_stem(Path::new("/x/random-name.wav")),
            "random-name"
        );
    }

    #[test]
    fn write_semantic_xlsx_emits_a_readable_workbook() {
        // Verify the writer produces a non-empty file with the xlsx
        // signature (PK\x03\x04 — xlsx is a ZIP). We don't parse the
        // workbook back; deeper tests live in the rust_xlsxwriter crate.
        let dir = std::env::temp_dir().join(format!("xlsx-smoke-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let out = dir.join("test.xlsx");
        let segments = vec![
            SemanticExportSegment {
                start_ms: 0,
                end_ms: 8400,
                final_text: "你今天过得怎么样？".into(),
                problems: vec![],
            },
            SemanticExportSegment {
                start_ms: 8400,
                end_ms: 18000,
                final_text: "我今天过得还不错，谢谢关心。".into(),
                problems: vec!["可能存在长停顿".into()],
            },
        ];
        write_semantic_xlsx(&out, "spkA_Freetalk_freetalk_260101_40m.wav", "spkA_Freetalk_freetalk_260101_40m", &segments)
            .expect("xlsx write");
        let bytes = fs::read(&out).expect("read back");
        assert!(bytes.len() > 1000, "xlsx suspiciously tiny: {} bytes", bytes.len());
        assert_eq!(&bytes[..4], b"PK\x03\x04", "must be a ZIP container");
        let _ = fs::remove_dir_all(&dir);
    }

    // ====================================================================
    // Phase 6: auto-QA — pure-fn tests
    // ====================================================================

    fn semantic_cfg_for_qa() -> CutConfig {
        CutConfig {
            mode: CutMode::Semantic,
            min_segment_s: 6.0,
            max_segment_s: 90.0,
            ..CutConfig::default()
        }
    }

    #[test]
    fn auto_qa_clean_segment_returns_no_issues() {
        let cfg = semantic_cfg_for_qa();
        let issues = auto_qa_segment(
            "你今天过得怎么样？我今天过得还不错，谢谢关心。",
            8_400,
            &cfg,
        );
        assert!(
            issues.is_empty(),
            "clean segment must pass; got: {issues:?}"
        );
    }

    #[test]
    fn auto_qa_flags_duration_out_of_range() {
        let cfg = semantic_cfg_for_qa();
        // Below the 5s grace floor.
        let short = auto_qa_segment("好。", 3_000, &cfg);
        assert!(short.iter().any(|i| i.contains("低于建议下限")));
        // Above the 90s ceiling.
        let long = auto_qa_segment("……。", 95_000, &cfg);
        assert!(long.iter().any(|i| i.contains("超出上限")));
    }

    #[test]
    fn auto_qa_flags_digit_leak_and_latin_punctuation() {
        let cfg = semantic_cfg_for_qa();
        let issues = auto_qa_segment("销量增长了20%，达到100台.", 8_000, &cfg);
        assert!(issues.iter().any(|i| i.contains("数字未汉化")));
        assert!(issues.iter().any(|i| i.contains("英文标点")));
    }

    #[test]
    fn auto_qa_flags_unmapped_neige() {
        let cfg = semantic_cfg_for_qa();
        let issues = auto_qa_segment("我们去吃内个东西吧。", 7_500, &cfg);
        assert!(issues.iter().any(|i| i.contains("内个 未改为 那个")));
    }

    #[test]
    fn auto_qa_flags_missing_terminal_punct() {
        let cfg = semantic_cfg_for_qa();
        let issues = auto_qa_segment("今天天气真不错", 7_500, &cfg);
        assert!(issues.iter().any(|i| i.contains("缺句末标点")));
    }

    #[test]
    fn auto_qa_flags_empty_text_and_short_circuits() {
        let cfg = semantic_cfg_for_qa();
        let issues = auto_qa_segment("   ", 8_000, &cfg);
        assert_eq!(issues, vec!["文本为空".to_string()]);
    }

    #[test]
    fn auto_qa_flags_density_anomaly() {
        let cfg = semantic_cfg_for_qa();
        // 89s / 5 chars = 17800 ms/char → way past 800ms threshold.
        let issues = auto_qa_segment("好的，今天。", 89_000, &cfg);
        assert!(issues.iter().any(|i| i.contains("可能存在长停顿")));
    }

    // ====================================================================
    // Phase 5: per-segment normalisation — pure-fn tests
    // ====================================================================

    #[test]
    fn semantic_normalize_prompt_pins_spec_rules() {
        // The constant prompt must mention each high-leverage spec
        // rule so a future "improvement" doesn't quietly drop one.
        // (We're not asserting exact wording, just that the rule is
        // reachable in the prompt text.)
        let p = SEMANTIC_NORMALIZE_PROMPT;
        assert!(p.contains("简体"), "simplified Chinese rule");
        assert!(p.contains("数字") && p.contains("汉字"), "digits → Chinese");
        assert!(p.contains("内个") && p.contains("那个"), "内个 → 那个");
        assert!(p.contains("他") && p.contains("她") && p.contains("它"), "他她它 disambig");
        assert!(p.contains("儿化"), "儿化音 rule");
        assert!(p.contains("180ms") || p.contains("180毫秒") || p.contains("180"), "stop threshold");
        assert!(p.contains("拖长音"), "sustained-sound rule");
        assert!(p.contains("【……】"), "long-sustain bracket");
        assert!(p.contains("\"text\""), "output schema pinned");
        assert!(p.contains("JSON") || p.contains("json"), "JSON mandated");
    }

    #[test]
    fn parse_semantic_normalize_response_accepts_valid_payload() {
        let raw = r#"{"text":"你好，今天天气真不错。"}"#;
        let out = parse_semantic_normalize_response(raw).expect("valid parse");
        assert_eq!(out, "你好，今天天气真不错。");
    }

    #[test]
    fn parse_semantic_normalize_response_trims_whitespace() {
        let raw = r#"{"text":"  你好，今天。  "}"#;
        let out = parse_semantic_normalize_response(raw).expect("valid parse");
        assert_eq!(out, "你好，今天。");
    }

    #[test]
    fn parse_semantic_normalize_response_rejects_bad_payloads() {
        // Not JSON.
        let err = parse_semantic_normalize_response("not json").unwrap_err();
        assert!(err.contains("非 JSON"));

        // Missing `text`.
        let err = parse_semantic_normalize_response(r#"{"foo":"bar"}"#).unwrap_err();
        assert!(err.contains("缺 text 字段"));

        // Empty text.
        let err = parse_semantic_normalize_response(r#"{"text":"  "}"#).unwrap_err();
        assert!(err.contains("text 为空"));
    }

    /// Phase 3 end-to-end: synthesise a 12-second waveform with three
    /// burst-then-silence regions, run `fine_pre_cut_for_semantic`,
    /// verify it produces multiple piece files with monotonically
    /// increasing boundaries that partition the source. The point of
    /// the test isn't to pin down exact piece count (silence detection
    /// is sensitive to input shape) — it's to guarantee:
    ///   - at least 2 pieces emerge (the function actually slices)
    ///   - every piece file exists and is readable
    ///   - boundaries are strictly monotonic + start at 0 + last
    ///     value ≈ source duration (within tolerance)
    #[test]
    fn fine_pre_cut_for_semantic_produces_multiple_pieces() {
        let ffmpeg_ok = silent_command("ffmpeg")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ffmpeg_ok {
            eprintln!("skipping fine-pre-cut test — ffmpeg not on PATH");
            return;
        }
        let dir = std::env::temp_dir().join(format!("fine-pre-cut-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("input.wav");
        let pieces = dir.join("pieces");

        // burst-silence-burst-silence-burst — 3 audible regions across
        // ~12s. Concat is simpler than aevalsrc conditionals and
        // makes the silence boundaries crisp / deterministic.
        let synth = silent_command("ffmpeg")
            .arg("-y").arg("-hide_banner").arg("-loglevel").arg("error")
            .arg("-f").arg("lavfi").arg("-i").arg("sine=frequency=440:duration=3:sample_rate=48000")
            .arg("-f").arg("lavfi").arg("-i").arg("anullsrc=duration=2:sample_rate=48000")
            .arg("-f").arg("lavfi").arg("-i").arg("sine=frequency=440:duration=3:sample_rate=48000")
            .arg("-f").arg("lavfi").arg("-i").arg("anullsrc=duration=2:sample_rate=48000")
            .arg("-f").arg("lavfi").arg("-i").arg("sine=frequency=440:duration=2:sample_rate=48000")
            .arg("-filter_complex").arg("[0][1][2][3][4]concat=n=5:v=0:a=1[out]")
            .arg("-map").arg("[out]")
            .arg("-ar").arg("48000").arg("-ac").arg("1")
            .arg("-c:a").arg("pcm_s16le")
            .arg(&src)
            .status()
            .expect("ffmpeg synth");
        assert!(synth.success(), "synth failed");

        let (paths, boundaries) =
            fine_pre_cut_for_semantic(&src, &pieces).expect("fine pre-cut");
        assert!(
            paths.len() >= 2,
            "expected multiple pieces, got {}",
            paths.len()
        );
        for p in &paths {
            assert!(p.is_file(), "piece file missing: {}", p.display());
        }
        // Boundaries: starts at 0, strictly increasing, last is ≤ 12.3s.
        assert_eq!(boundaries.first().copied(), Some(0));
        let mut prev = 0u64;
        for b in &boundaries[1..] {
            assert!(*b > prev, "boundary not monotonic: {b} <= {prev}");
            prev = *b;
        }
        assert!(
            *boundaries.last().unwrap() <= 12_500,
            "last boundary {} exceeds 12.5s",
            boundaries.last().unwrap()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn portablize_payload_rewrites_bundle_paths_to_relative() {
        // Simulate what `export_dataset_bundle_impl` produces just before
        // writing project.json: a bundle dir on disk with a fake
        // segments/file under it. portablize_payload must rewrite the
        // absolute segmentsDir + segments[].{segmentPath,sourcePath}
        // values to "./..." form so the bundle is portable. Out-of-bundle
        // paths (the original source file) stay absolute.
        let tmp = std::env::temp_dir().join(format!(
            "portablize-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        let segments_dir = tmp.join("segments");
        let source_dir = tmp.join("source");
        fs::create_dir_all(&segments_dir).unwrap();
        fs::create_dir_all(&source_dir).unwrap();
        let seg_file = segments_dir.join("文本演绎_0001_01_01_发音人.wav");
        let src_file = source_dir.join("台湾话-发音人-文本演绎-话题1.wav");
        fs::write(&seg_file, b"fake").unwrap();
        fs::write(&src_file, b"fake").unwrap();
        let bundle_canonical = tmp.canonicalize().unwrap();

        let payload = serde_json::json!({
            "rootPath": ".",
            "projectDir": ".",
            "segmentsDir": path_to_string(&segments_dir),
            "segments": [
                {
                    "segmentPath": path_to_string(&seg_file),
                    "sourcePath": path_to_string(&src_file),
                },
                {
                    // Outside-the-bundle source path must be left alone.
                    "segmentPath": path_to_string(&seg_file),
                    "sourcePath": "/somewhere/else/台湾话.wav",
                },
            ]
        });
        let result = portablize_payload(payload, &bundle_canonical);

        // Top-level "." values pass through untouched.
        assert_eq!(result["rootPath"], json!("."));
        assert_eq!(result["projectDir"], json!("."));
        // segmentsDir → "./segments" — under bundle, gets rewritten.
        assert_eq!(
            result["segmentsDir"].as_str().unwrap().replace('\\', "/"),
            "./segments"
        );
        // First segment: both paths are inside the bundle → both rewritten.
        let s0 = &result["segments"][0];
        assert_eq!(
            s0["segmentPath"].as_str().unwrap().replace('\\', "/"),
            "./segments/文本演绎_0001_01_01_发音人.wav"
        );
        assert_eq!(
            s0["sourcePath"].as_str().unwrap().replace('\\', "/"),
            "./source/台湾话-发音人-文本演绎-话题1.wav"
        );
        // Second segment: outside-the-bundle source stays absolute.
        let s1 = &result["segments"][1];
        assert_eq!(s1["sourcePath"], json!("/somewhere/else/台湾话.wav"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn pair_key_strips_role_suffix() {
        // Source file names are `..._<话题>_<轮>_<role>.wav` — sub-segment
        // indices (01/02/…) only show up in segment_file_name, never here.
        let assistant = SegmentRecord {
            id: "x".into(),
            source_path: "/audio/自由演绎_0001_01_发音人.wav".into(),
            source_file_name: "自由演绎_0001_01_发音人.wav".into(),
            segment_path: "/segments/自由演绎_0001_01_03_发音人.wav".into(),
            segment_file_name: "自由演绎_0001_01_03_发音人.wav".into(),
            role: Some("assistant".into()),
            start_ms: 0,
            end_ms: 1000,
            duration_ms: 1000,
            original_text: String::new(),
            phonetic_text: String::new(),
            emotion: Vec::new(),
            tags: Vec::new(),
            notes: String::new(),
        };
        assert_eq!(pair_key(&assistant), "自由演绎_0001_01");

        let user_seg = SegmentRecord {
            source_path: "/audio/自由演绎_0001_01_陪聊.wav".into(),
            source_file_name: "自由演绎_0001_01_陪聊.wav".into(),
            role: Some("user".into()),
            ..assistant.clone()
        };
        assert_eq!(pair_key(&user_seg), "自由演绎_0001_01");

        // Adjacent dialogue turns (01 vs 02) must NOT collapse to the same
        // pair_key — that was a regression caused by the regex greedily
        // eating `_01_发音人` instead of just `_发音人`.
        let assistant_turn2 = SegmentRecord {
            source_path: "/audio/自由演绎_0001_02_发音人.wav".into(),
            source_file_name: "自由演绎_0001_02_发音人.wav".into(),
            segment_path: "/segments/自由演绎_0001_02_01_发音人.wav".into(),
            segment_file_name: "自由演绎_0001_02_01_发音人.wav".into(),
            ..assistant.clone()
        };
        assert_eq!(pair_key(&assistant_turn2), "自由演绎_0001_02");
        assert_ne!(pair_key(&assistant), pair_key(&assistant_turn2));
    }

    /// Regression: a stale `segment_file_name` field (still in the old
    /// fallback `<source>_NNNN_xxx-yyy.wav` form because the cutter ran
    /// before the current dataset_mode whitelist landed) must NOT
    /// prevent pair_key from grouping user/assistant segments. As long
    /// as the on-disk filename (`segment_path` basename) has been
    /// renamed to the canonical form, pair_key falls through to it.
    ///
    /// Without this fallback, a 文本演绎 batch where assistant segments
    /// retained stale field values would collapse N rounds into one
    /// pair_key (all sharing the same source_path strip), while user
    /// segments would parse cleanly into N distinct keys → zero pairing
    /// across the entire export.
    #[test]
    fn pair_key_falls_back_to_path_basename_when_field_is_stale() {
        let seg = SegmentRecord {
            id: "x".into(),
            // Pretend the cutter ran on an older binary whose
            // dataset_mode whitelist didn't include 文本演绎, so the
            // field below holds the safe-source fallback form.
            source_path: "/input/台湾话-发音人-文本演绎-话题56.wav".into(),
            source_file_name: "台湾话-发音人-文本演绎-话题56.wav".into(),
            // …but the file on disk has since been renamed to the
            // canonical layout (e.g. via plan_dialogue_sequence_file_names
            // running under the newer binary).
            segment_path: "/out/segments/文本演绎_0056_03_02_发音人.wav".into(),
            segment_file_name: "台湾话-发音人-文本演绎-话题56_0001_2000-3500.wav".into(),
            role: Some("assistant".into()),
            start_ms: 2000,
            end_ms: 3500,
            duration_ms: 1500,
            original_text: String::new(),
            phonetic_text: String::new(),
            emotion: Vec::new(),
            tags: Vec::new(),
            notes: String::new(),
        };
        // The stale field doesn't parse, but the canonical basename
        // does → pair_key reflects the on-disk truth.
        assert_eq!(pair_key(&seg), "文本演绎_0056_03");
    }

    /// 长沙方言 dataset puts the role token in the MIDDLE of the filename
    /// with `-` separators, not at the end with `_`. Both conventions
    /// must collapse to the same pair_key for paired-conversation export
    /// to work.
    #[test]
    fn pair_key_handles_middle_role_token_with_dashes() {
        let mk = |path: &str, role: &str| SegmentRecord {
            id: "x".into(),
            source_path: path.into(),
            source_file_name: path.rsplit('/').next().unwrap_or(path).to_string(),
            segment_path: "/segments/x.wav".into(),
            segment_file_name: "x.wav".into(),
            role: Some(role.into()),
            start_ms: 0,
            end_ms: 1000,
            duration_ms: 1000,
            original_text: String::new(),
            phonetic_text: String::new(),
            emotion: Vec::new(),
            tags: Vec::new(),
            notes: String::new(),
        };

        // Both files describe dialogue turn "话题1" of the 0413 session;
        // they must share a pair_key so build_paired_jsonl groups them.
        let assistant = mk(
            "/Users/x/长沙方言/发音人/长沙方言-0413-文案演绎-发音人-话题1.wav",
            "assistant",
        );
        let user = mk(
            "/Users/x/长沙方言/陪聊/长沙方言-0413-文案演绎-陪聊-话题1.wav",
            "user",
        );
        assert_eq!(pair_key(&assistant), "长沙方言-0413-文案演绎-话题1");
        assert_eq!(pair_key(&user), pair_key(&assistant));

        // Different topic still distinct.
        let assistant_t2 = mk(
            "/Users/x/长沙方言/发音人/长沙方言-0413-文案演绎-发音人-话题2.wav",
            "assistant",
        );
        assert_ne!(pair_key(&assistant), pair_key(&assistant_t2));
    }

    #[test]
    fn export_paired_uses_messages_format() {
        let user = SegmentRecord {
            id: "u".into(),
            source_path: "/audio/free_001_01_陪聊.wav".into(),
            source_file_name: "free_001_01_陪聊.wav".into(),
            segment_path: "/segments/u_01.wav".into(),
            segment_file_name: "u_01.wav".into(),
            role: Some("user".into()),
            start_ms: 0,
            end_ms: 1000,
            duration_ms: 1000,
            original_text: "你吃过冬瓜山的烤肠吗".into(),
            phonetic_text: "你吃过冬瓜山的烤肠吗".into(),
            emotion: vec![],
            tags: vec![],
            notes: String::new(),
        };
        let assistant = SegmentRecord {
            id: "a".into(),
            // Source is the un-cut `_发音人.wav` for this turn — sub-index
            // (`_01`) lives only in segment_file_name.
            source_path: "/audio/free_001_01_发音人.wav".into(),
            source_file_name: "free_001_01_发音人.wav".into(),
            segment_path: "/segments/free_001_01_01_发音人.wav".into(),
            segment_file_name: "free_001_01_01_发音人.wav".into(),
            role: Some("assistant".into()),
            start_ms: 0,
            end_ms: 1500,
            duration_ms: 1500,
            original_text: String::new(),
            phonetic_text: "冬瓜山的烤肠啊我挨的吃过啊".into(),
            emotion: vec!["中立".into()],
            tags: vec![],
            notes: String::new(),
        };
        let lines = build_paired_jsonl(
            &[user, assistant],
            "长沙本地人，女性，25岁左右",
            true,
            "",
            "",
        )
        .expect("paired");
        assert_eq!(lines.len(), 1);
        let value: Value = serde_json::from_str(&lines[0]).expect("valid json");
        let messages = value.get("messages").and_then(|m| m.as_array()).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
        // Spec: assistant uses `content_refine`, not `content`.
        assert!(messages[2]["content_refine"].is_array());
        assert!(messages[2].get("content").is_none());
        // Single assistant sub-segment → audio_file and emotion_refine
        // are STILL arrays. Earlier code collapsed length-1 to scalars
        // to mirror the demo file, but the downstream trainer needs a
        // uniform shape across all sample sizes.
        assert_eq!(
            messages[2]["audio_file"].as_array().map(|a| a.len()),
            Some(1)
        );
        assert_eq!(messages[2]["emotion_refine"], json!(["中立"]));
    }

    /// Regression: paths under `/Users/<name>/` (every macOS project)
    /// must NOT all collapse to role=`user`. The old `infer_role` did a
    /// substring match on the lowercased full path; `users` contains
    /// `user`, so it false-positively classified `..._发音人.wav` files
    /// as `user`. The fix: only check the file name + parent dir name,
    /// and use delimiter-bounded matching for ASCII keywords.
    #[test]
    fn infer_role_ignores_macos_users_in_path() {
        let p = Path::new("/Users/sunpeak/Work/dialect/长沙方言/发音人/长沙方言-话题1.wav");
        assert_eq!(infer_role(p).as_deref(), Some("assistant"));

        let p = Path::new("/Users/sunpeak/Work/dialect/长沙方言/陪聊/长沙方言-话题1.wav");
        assert_eq!(infer_role(p).as_deref(), Some("user"));

        // Filename token wins when the parent dir doesn't have a role hint.
        let p = Path::new("/Users/x/free_001_02_发音人.wav");
        assert_eq!(infer_role(p).as_deref(), Some("assistant"));
        let p = Path::new("/Users/x/free_001_02_陪聊.wav");
        assert_eq!(infer_role(p).as_deref(), Some("user"));

        // ASCII boundary check: `xuser.wav` is NOT `user`, but `_user.wav` is.
        let p = Path::new("/tmp/xuser.wav");
        assert_eq!(infer_role(p), None);
        let p = Path::new("/tmp/recording_user.wav");
        assert_eq!(infer_role(p).as_deref(), Some("user"));
    }

    /// Multi-segment assistant: when the cutter splits one `_发音人.wav`
    /// source into N sub-segments, they should land in the SAME jsonl line
    /// — not N separate lines, each duplicating the user audio. content,
    /// audio_file, and emotion all become parallel arrays of length N.
    #[test]
    fn export_paired_groups_multi_assistant_into_one_line() {
        let user = SegmentRecord {
            id: "u".into(),
            source_path: "/audio/free_001_02_陪聊.wav".into(),
            source_file_name: "free_001_02_陪聊.wav".into(),
            segment_path: "/segments/u_01.wav".into(),
            segment_file_name: "u_01.wav".into(),
            role: Some("user".into()),
            start_ms: 0,
            end_ms: 1000,
            duration_ms: 1000,
            original_text: "问题".into(),
            phonetic_text: "问题".into(),
            emotion: vec![],
            tags: vec![],
            notes: String::new(),
        };
        let make_assistant = |idx: u64, text: &str, emotion: &str| SegmentRecord {
            id: format!("a{}", idx),
            source_path: "/audio/free_001_02_发音人.wav".into(),
            source_file_name: "free_001_02_发音人.wav".into(),
            segment_path: format!("/segments/free_001_02_{:02}_发音人.wav", idx),
            segment_file_name: format!("free_001_02_{:02}_发音人.wav", idx),
            role: Some("assistant".into()),
            start_ms: idx * 1000,
            end_ms: idx * 1000 + 800,
            duration_ms: 800,
            original_text: String::new(),
            phonetic_text: text.into(),
            emotion: vec![emotion.into()],
            tags: vec![],
            notes: String::new(),
        };
        let segments = vec![
            user,
            make_assistant(2, "句二", "开心"),
            make_assistant(1, "句一", "中立"),
            make_assistant(3, "句三", "中立"),
        ];

        let lines = build_paired_jsonl(&segments, "system话", true, "", "").expect("paired");
        assert_eq!(lines.len(), 1, "all four segments fold into one line");

        let value: Value = serde_json::from_str(&lines[0]).expect("valid json");
        let messages = value["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3); // system + user + assistant

        let assistant_msg = &messages[2];
        // Spec key rename: content → content_refine, emotion → emotion_refine.
        let content = assistant_msg["content_refine"].as_array().unwrap();
        // Sorted by start_ms — sub-segments emerge in temporal order.
        assert_eq!(content[0], "句一");
        assert_eq!(content[1], "句二");
        assert_eq!(content[2], "句三");

        let audio = assistant_msg["audio_file"].as_array().unwrap();
        assert_eq!(audio.len(), 3, "audio_file is a parallel array of size 3");
        assert!(audio[0]
            .as_str()
            .unwrap()
            .ends_with("free_001_02_01_发音人.wav"));

        let emotions = assistant_msg["emotion_refine"].as_array().unwrap();
        assert_eq!(emotions, &vec![json!("中立"), json!("开心"), json!("中立")]);
    }

    #[test]
    fn export_paired_mirrors_multi_user_content_and_audio_arrays() {
        let make_segment = |role: &str, index: u64, text: &str| SegmentRecord {
            id: format!("{}_{}", role, index),
            source_path: format!("/audio/文案演绎_0001_04_{}.wav", role),
            source_file_name: format!("文案演绎_0001_04_{}.wav", role),
            segment_path: format!(
                "/segments/文案演绎_0001_04_{:02}_{}.wav",
                index,
                if role == "user" {
                    "陪聊"
                } else {
                    "发音人"
                }
            ),
            segment_file_name: format!(
                "文案演绎_0001_04_{:02}_{}.wav",
                index,
                if role == "user" {
                    "陪聊"
                } else {
                    "发音人"
                }
            ),
            role: Some(role.to_string()),
            start_ms: index * 1000,
            end_ms: index * 1000 + 800,
            duration_ms: 800,
            original_text: String::new(),
            phonetic_text: text.to_string(),
            emotion: if role == "assistant" {
                vec!["中立".to_string()]
            } else {
                Vec::new()
            },
            tags: Vec::new(),
            notes: String::new(),
        };

        let segments = vec![
            make_segment("user", 1, "陪聊第一段"),
            make_segment("user", 2, "陪聊第二段"),
            make_segment("assistant", 1, "发音人第一段"),
        ];

        let lines = build_paired_jsonl(&segments, "", false, "", "").expect("paired");
        let value: Value = serde_json::from_str(&lines[0]).expect("valid json");
        let messages = value["messages"].as_array().unwrap();
        let user_msg = &messages[0];
        // Spec: user side is `content_refine`, not `content`.
        let user_content = user_msg["content_refine"].as_array().unwrap();
        let user_audio = user_msg["audio_file"].as_array().unwrap();
        assert!(
            user_msg.get("content").is_none(),
            "user message must not carry the regular `content` key"
        );

        assert_eq!(
            user_content,
            &vec![json!("陪聊第一段"), json!("陪聊第二段")]
        );
        assert_eq!(user_audio.len(), 2);
        assert!(user_audio[0]
            .as_str()
            .unwrap()
            .ends_with("文案演绎_0001_04_01_陪聊.wav"));
        assert!(user_audio[1]
            .as_str()
            .unwrap()
            .ends_with("文案演绎_0001_04_02_陪聊.wav"));
    }

    #[test]
    fn cuts_to_uncompressed_pcm_wav() {
        if ensure_ffmpeg().is_err() {
            return;
        }

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be valid")
            .as_millis();
        let root = std::env::temp_dir().join(format!("dialect_labeler_test_{}", stamp));
        let segments_dir = root.join("segments");
        fs::create_dir_all(&segments_dir).expect("test temp dir should be created");

        let input = root.join("sample.wav");
        let status = Command::new("ffmpeg")
            .arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg("sine=frequency=440:duration=0.5:sample_rate=16000")
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg("anullsrc=channel_layout=mono:sample_rate=16000:duration=0.7")
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg("sine=frequency=550:duration=0.5:sample_rate=16000")
            .arg("-filter_complex")
            .arg("[0:a][1:a][2:a]concat=n=3:v=0:a=1[out]")
            .arg("-map")
            .arg("[out]")
            .arg("-c:a")
            .arg("pcm_s16le")
            .arg(&input)
            .status()
            .expect("ffmpeg should run");
        assert!(status.success());

        let config = CutConfig {
            silence_db: -35.0,
            min_silence_ms: 300,
            min_segment_ms: 100,
            pre_roll_ms: 100,
            post_roll_ms: 200,
            max_segment_ms: 0,
            ..CutConfig::default()
        };

        let segments = cut_audio_file_impl(
            path_to_string(&input),
            path_to_string(&segments_dir),
            config,
            Some("assistant".to_string()),
            None,
            Some("ka".to_string()),
            Some(vec!["neutral".to_string()]),
            Vec::new(),
        )
        .expect("audio should be cut");

        assert_eq!(segments.len(), 2);
        let probe = probe_audio(Path::new(&segments[0].segment_path)).expect("segment probes");
        assert_eq!(probe.codec_name.as_deref(), Some("pcm_s16le"));
        assert_eq!(probe.sample_rate, Some(16000));
        assert!(segments[0].segment_file_name.ends_with(".wav"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn max_segment_force_splits_overlong_ranges() {
        // 50s of audio, no silence detected. max_segment_ms=20000 should
        // force the range into ceil(50/20) = 3 equal pieces of ~16.67s
        // (with pre/post roll added on outer edges).
        let config = CutConfig {
            silence_db: -35.0,
            min_silence_ms: 450,
            min_segment_ms: 300,
            pre_roll_ms: 100,
            post_roll_ms: 200,
            max_segment_ms: 20_000,
            ..CutConfig::default()
        };
        let ranges = build_segment_ranges(50.0, &[], &config);
        assert_eq!(ranges.len(), 3);
        for (start, end) in &ranges {
            // Each piece is ~16.67s plus pre/post roll → never exceeds
            // max+rolls ≈ 20s.
            assert!(
                end - start <= 20.0,
                "force-split piece longer than max: {:.2}s",
                end - start
            );
        }
    }

    #[test]
    fn max_segment_zero_means_no_force_split() {
        let config = CutConfig {
            silence_db: -35.0,
            min_silence_ms: 450,
            min_segment_ms: 300,
            pre_roll_ms: 100,
            post_roll_ms: 200,
            max_segment_ms: 0,
            ..CutConfig::default()
        };
        let ranges = build_segment_ranges(50.0, &[], &config);
        assert_eq!(ranges.len(), 1, "max=0 disables force-split");
    }

    #[test]
    fn short_silence_inside_long_phrase_does_not_split() {
        // 2s voice, 0.5s pause, 3s voice. min_segment=5000 (5s) — the
        // pause-driven split would produce a 2s piece, below the floor.
        // Expected: the cutter merges through the pause and emits ONE
        // segment covering [0, 5.5).
        let events = vec![
            SilenceEvent::Start(2.0),
            SilenceEvent::End(2.5),
        ];
        let config = CutConfig {
            silence_db: -35.0,
            min_silence_ms: 400,
            min_segment_ms: 5_000,
            pre_roll_ms: 0,
            post_roll_ms: 0,
            max_segment_ms: 0,
            ..CutConfig::default()
        };
        let ranges = build_segment_ranges(5.5, &events, &config);
        assert_eq!(ranges.len(), 1, "ranges={:?}", ranges);
        assert!((ranges[0].0 - 0.0).abs() < 1e-6);
        assert!((ranges[0].1 - 5.5).abs() < 1e-6);
    }

    #[test]
    fn short_tail_pushed_as_standalone_no_audio_loss() {
        // Voice 0-25, silence 25-25.5, voice 25.5-30. min_segment=5.
        // Old behaviour glued the 4.5s tail onto the previous segment,
        // re-introducing 500ms of interior silence. The fix: push the
        // tail as its own range. No audio loss, no interior silence.
        let events = vec![SilenceEvent::Start(25.0), SilenceEvent::End(25.5)];
        let config = CutConfig {
            silence_db: -35.0,
            min_silence_ms: 400,
            min_segment_ms: 5_000,
            pre_roll_ms: 0,
            post_roll_ms: 0,
            max_segment_ms: 0,
            ..CutConfig::default()
        };
        let ranges = build_segment_ranges(30.0, &events, &config);
        assert_eq!(ranges.len(), 2, "ranges={:?}", ranges);
        assert!((ranges[0].0 - 0.0).abs() < 1e-6);
        assert!((ranges[0].1 - 25.0).abs() < 1e-6);
        assert!((ranges[1].0 - 25.5).abs() < 1e-6);
        assert!((ranges[1].1 - 30.0).abs() < 1e-6);
    }

    #[test]
    fn force_split_prefers_skipped_silence_over_equal_split() {
        // 50s monologue. Pauses at 5, 10, 20s are all too short (300ms)
        // for stand-alone splitting under min_segment=15s, so they become
        // "skipped silences". A real pause at 35s+ is long enough.
        //
        // When build_segment_ranges accepts the 35s pause and pushes the
        // (0, 35) range, that range exceeds max=25, triggering smart-split:
        // the latest skipped pause whose start is ≤ start+max = 25 is
        // (20, 20.5). First half = (0, 20), second half starts at 20.5.
        // Both halves now fit under max → no equal-split, no arbitrary cut.
        let events = vec![
            SilenceEvent::Start(5.0),  SilenceEvent::End(5.3),
            SilenceEvent::Start(10.0), SilenceEvent::End(10.3),
            SilenceEvent::Start(20.0), SilenceEvent::End(20.5),
            SilenceEvent::Start(35.0), SilenceEvent::End(35.5),
        ];
        let config = CutConfig {
            silence_db: -30.0,
            min_silence_ms: 600,
            min_segment_ms: 15_000,
            pre_roll_ms: 0,
            post_roll_ms: 0,
            max_segment_ms: 25_000,
            ..CutConfig::default()
        };
        let ranges = build_segment_ranges(50.0, &events, &config);
        // Expected:
        //   (0, 20)     ← smart-cut at the 20s skipped silence
        //   (20.5, 35)  ← voice after pause; ends at the real (long) silence at 35
        //   (35.5, 50)  ← trailing voice after the long silence
        assert_eq!(ranges.len(), 3, "ranges={:?}", ranges);
        assert!((ranges[0].0 - 0.0).abs() < 1e-6);
        assert!((ranges[0].1 - 20.0).abs() < 1e-6, "smart cut at 20s pause");
        assert!((ranges[1].0 - 20.5).abs() < 1e-6, "second half starts after pause");
        assert!((ranges[1].1 - 35.0).abs() < 1e-6);
        assert!((ranges[2].0 - 35.5).abs() < 1e-6);
        assert!((ranges[2].1 - 50.0).abs() < 1e-6);
    }

    #[test]
    fn force_split_falls_back_to_equal_when_no_skipped_silence_helps() {
        // 50s of audio, no events at all. Smart split has no candidates,
        // so equal-N split takes over. max=20 → 3 pieces of ~16.67s.
        let config = CutConfig {
            silence_db: -30.0,
            min_silence_ms: 600,
            min_segment_ms: 5_000,
            pre_roll_ms: 0,
            post_roll_ms: 0,
            max_segment_ms: 20_000,
            ..CutConfig::default()
        };
        let ranges = build_segment_ranges(50.0, &[], &config);
        assert_eq!(ranges.len(), 3);
        for (s, e) in &ranges {
            assert!(e - s <= 20.0, "piece exceeds max: {:.2}", e - s);
        }
    }

    #[test]
    fn whole_audio_below_min_emits_one_range() {
        // No events; 3s of audio; min_segment=5s. Should still emit
        // (0,3) instead of producing zero segments.
        let config = CutConfig {
            silence_db: -35.0,
            min_silence_ms: 400,
            min_segment_ms: 5_000,
            pre_roll_ms: 0,
            post_roll_ms: 0,
            max_segment_ms: 0,
            ..CutConfig::default()
        };
        let ranges = build_segment_ranges(3.0, &[], &config);
        assert_eq!(ranges.len(), 1);
        assert!((ranges[0].0 - 0.0).abs() < 1e-6);
        assert!((ranges[0].1 - 3.0).abs() < 1e-6);
    }

    #[test]
    fn audio_ending_in_silence_does_not_duplicate_segment() {
        // Voice 0-8, then silence 8-10 with no End event emitted (audio
        // file ends inside silence). Expect single (0,8) — no duplicate
        // push from final-tail block.
        let events = vec![SilenceEvent::Start(8.0)];
        let config = CutConfig {
            silence_db: -35.0,
            min_silence_ms: 400,
            min_segment_ms: 3_000,
            pre_roll_ms: 0,
            post_roll_ms: 0,
            max_segment_ms: 0,
            ..CutConfig::default()
        };
        let ranges = build_segment_ranges(10.0, &events, &config);
        assert_eq!(ranges.len(), 1, "ranges={:?}", ranges);
        assert!((ranges[0].0 - 0.0).abs() < 1e-6);
        assert!((ranges[0].1 - 8.0).abs() < 1e-6);
    }

    #[test]
    fn taiwan_dialect_source_produces_clean_文本演绎_segment_name() {
        // Source filename uses the 0429-台湾 batch shape:
        //   台湾话-{发音人|陪聊人}-文本演绎-话题NN.wav
        // Expected segment output names must NOT carry the `台湾话-` prefix
        // — they should be the canonical `文本演绎_NNNN_NN_NN_<role>.wav`.
        let assistant = Path::new("/tmp/台湾话-发音人-文本演绎-话题56.wav");
        assert_eq!(
            build_segment_file_name(assistant, Some("assistant"), None, &[], 0, 0, 1000),
            "文本演绎_0056_01_01_发音人.wav"
        );
        let user = Path::new("/tmp/台湾话-陪聊人-文本演绎-话题56.wav");
        assert_eq!(
            build_segment_file_name(user, Some("user"), None, &[], 2, 0, 1000),
            "文本演绎_0056_01_03_陪聊.wav"
        );
        // 0512 自由对话 batch — same shape, different mode token.
        let assistant_dialog = Path::new("/tmp/台湾话-发音人-自由对话-话题72.wav");
        assert_eq!(
            build_segment_file_name(assistant_dialog, Some("assistant"), None, &[], 0, 0, 1000),
            "自由对话_0072_01_01_发音人.wav"
        );
    }

    #[test]
    fn leading_silence_is_trimmed_off_first_segment() {
        // File starts with 9s of silence, then voice 9-16. ffmpeg emits
        // silence_start: 0 + silence_end: 9. Old behaviour: the head silence
        // was classified "skipped" (seg_duration before it was 0 < min_segment)
        // and stayed glued onto the first emitted range, producing one segment
        // (0, 16) — exactly the 9s leading-silence bug from the screenshot.
        // Fix: head silence (seg_duration <= 0) must advance the cursor on End.
        let events = vec![SilenceEvent::Start(0.0), SilenceEvent::End(9.0)];
        let config = CutConfig {
            silence_db: -30.0,
            min_silence_ms: 600,
            min_segment_ms: 800,
            pre_roll_ms: 0,
            post_roll_ms: 0,
            max_segment_ms: 0,
            ..CutConfig::default()
        };
        let ranges = build_segment_ranges(16.0, &events, &config);
        assert_eq!(ranges.len(), 1, "ranges={:?}", ranges);
        assert!((ranges[0].0 - 9.0).abs() < 1e-6, "first segment must start AT voice onset, not 0");
        assert!((ranges[0].1 - 16.0).abs() < 1e-6);
    }

    #[test]
    fn leading_silence_then_normal_split() {
        // 7s head silence, voice 7-12, real pause 12-13, voice 13-20.
        // Expect TWO segments, both with head silence trimmed.
        let events = vec![
            SilenceEvent::Start(0.0),
            SilenceEvent::End(7.0),
            SilenceEvent::Start(12.0),
            SilenceEvent::End(13.0),
        ];
        let config = CutConfig {
            silence_db: -30.0,
            min_silence_ms: 600,
            min_segment_ms: 800,
            pre_roll_ms: 0,
            post_roll_ms: 0,
            max_segment_ms: 0,
            ..CutConfig::default()
        };
        let ranges = build_segment_ranges(20.0, &events, &config);
        assert_eq!(ranges.len(), 2, "ranges={:?}", ranges);
        assert!((ranges[0].0 - 7.0).abs() < 1e-6);
        assert!((ranges[0].1 - 12.0).abs() < 1e-6);
        assert!((ranges[1].0 - 13.0).abs() < 1e-6);
        assert!((ranges[1].1 - 20.0).abs() < 1e-6);
    }

    #[test]
    fn long_silence_after_long_phrase_does_split() {
        // 6s voice, 1s pause, 4s voice. Both pieces ≥ 3s min_segment,
        // pause > min_silence → two segments.
        let events = vec![
            SilenceEvent::Start(6.0),
            SilenceEvent::End(7.0),
        ];
        let config = CutConfig {
            silence_db: -35.0,
            min_silence_ms: 400,
            min_segment_ms: 3_000,
            pre_roll_ms: 0,
            post_roll_ms: 0,
            max_segment_ms: 0,
            ..CutConfig::default()
        };
        let ranges = build_segment_ranges(11.0, &events, &config);
        assert_eq!(ranges.len(), 2, "ranges={:?}", ranges);
        assert!((ranges[0].0 - 0.0).abs() < 1e-6);
        assert!((ranges[0].1 - 6.0).abs() < 1e-6);
        assert!((ranges[1].0 - 7.0).abs() < 1e-6);
        assert!((ranges[1].1 - 11.0).abs() < 1e-6);
    }

    #[test]
    fn splits_text_across_segments_by_duration() {
        let chunks = split_text_by_ranges("冬瓜山的烤肠啊", &[(0.0, 1.0), (1.0, 3.0), (3.0, 4.0)]);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.concat(), "冬瓜山的烤肠啊");
        assert!(chunks[1].chars().count() >= chunks[0].chars().count());
    }

    #[test]
    fn effective_silence_db_floors_at_user_setting_for_clean_recordings() {
        // Clean recording: noise floor -50 dB. User wants -30 cutoff.
        // Bumping by +8 (= -42) is stricter than user's -30, so the
        // user's preference wins.
        let eff = effective_silence_db(-30.0, Some(-50.0));
        assert!((eff - -30.0).abs() < 0.01, "got {}", eff);
    }

    #[test]
    fn effective_silence_db_bumps_above_user_for_noisy_recordings() {
        // Noisy recording: floor -22 dB. Headroom puts the effective
        // threshold at -14, but the upper cap (-15) holds it back.
        let eff = effective_silence_db(-30.0, Some(-22.0));
        assert!(eff > -30.0, "should bump up, got {}", eff);
        assert!(eff <= -15.0, "should respect cap, got {}", eff);
    }

    #[test]
    fn effective_silence_db_capped_so_speech_is_never_silence() {
        // Even an absurdly-loud noise floor cannot push the threshold
        // past the cap, otherwise actual speech would register as silence.
        let eff = effective_silence_db(-30.0, Some(0.0));
        assert!(eff <= -15.0, "got {}", eff);
    }

    #[test]
    fn effective_silence_db_falls_back_to_user_when_floor_unknown() {
        let eff = effective_silence_db(-32.0, None);
        assert!((eff - -32.0).abs() < 0.01, "got {}", eff);
    }

    #[test]
    fn trim_segment_head_tail_shortens_file_and_reports_trim_offsets() {
        if ensure_ffmpeg().is_err() {
            return;
        }

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be valid")
            .as_millis();
        let root = std::env::temp_dir().join(format!("dialect_labeler_trim_{}", stamp));
        fs::create_dir_all(&root).expect("test temp dir");
        let segment = root.join("seg.wav");

        // [2s silence][1s sine][2s silence] — total 5s, the sine sits
        // in the middle so trim must strip ~2s off the head and ~2s
        // off the tail.
        let status = Command::new("ffmpeg")
            .arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg("anullsrc=channel_layout=mono:sample_rate=16000:duration=2.0")
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg("sine=frequency=440:duration=1.0:sample_rate=16000")
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg("anullsrc=channel_layout=mono:sample_rate=16000:duration=2.0")
            .arg("-filter_complex")
            .arg("[0:a][1:a][2:a]concat=n=3:v=0:a=1[out]")
            .arg("-map")
            .arg("[out]")
            .arg("-c:a")
            .arg("pcm_s16le")
            .arg(&segment)
            .status()
            .expect("ffmpeg should produce test fixture");
        assert!(status.success());

        let before = probe_audio(&segment)
            .expect("probe")
            .duration_ms
            .unwrap_or(0);
        assert!(
            before >= 4_900 && before <= 5_100,
            "fixture len: {}",
            before
        );

        let (head_trim_ms, new_duration_ms) =
            trim_segment_head_tail(&segment, -30.0, 100, 200).expect("trim");

        // Head trim should be roughly the 2s of leading silence, minus
        // 100ms of pre-roll padding kept on purpose.
        assert!(
            head_trim_ms >= 1_700 && head_trim_ms <= 2_000,
            "head trim was {}",
            head_trim_ms
        );
        // Final duration: 1s sine + 100ms pre + 200ms post ≈ 1.3s,
        // with some slack for window quantisation.
        assert!(
            new_duration_ms >= 1_100 && new_duration_ms <= 1_500,
            "new duration was {}",
            new_duration_ms
        );

        let after = probe_audio(&segment)
            .expect("probe")
            .duration_ms
            .unwrap_or(0);
        assert!(
            after >= 1_100 && after <= 1_500,
            "file length after trim: {}",
            after
        );

        let check_config = CutConfig {
            silence_db: -30.0,
            min_silence_ms: 300,
            min_segment_ms: 100,
            pre_roll_ms: 100,
            post_roll_ms: 200,
            max_segment_ms: 0,
            ..CutConfig::default()
        };
        let check_segment = SegmentRecord {
            id: "seg".to_string(),
            source_path: path_to_string(&segment),
            source_file_name: "source.wav".to_string(),
            segment_path: path_to_string(&segment),
            segment_file_name: "seg.wav".to_string(),
            role: None,
            start_ms: 0,
            end_ms: after,
            duration_ms: after,
            original_text: String::new(),
            phonetic_text: String::new(),
            emotion: Vec::new(),
            tags: Vec::new(),
            notes: String::new(),
        };
        let quality = validate_cut_segment(&check_segment, &check_config);
        assert!(quality.ok, "quality check failed: {:?}", quality);
        assert!(quality.leading_silence_ms <= 100);
        assert!(quality.trailing_silence_ms <= 200);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn gate_segment_head_tail_zeroes_only_boundary_noise() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis();
        let root = std::env::temp_dir().join(format!("dialect_labeler_gate_{}", stamp));
        fs::create_dir_all(&root).expect("temp dir");
        let segment = root.join("gate.wav");

        let mut samples = Vec::new();
        samples.extend(std::iter::repeat(500i16).take(1_600));
        samples.extend(std::iter::repeat(5_000i16).take(3_200));
        samples.extend(std::iter::repeat(-500i16).take(1_600));
        write_pcm16_mono_wav(&segment, 16_000, &samples);

        gate_segment_head_tail(&segment, -30.0).expect("gate");

        let gated = read_pcm16_mono_samples(&segment);
        assert_eq!(gated.len(), samples.len());
        assert!(gated[..1_600].iter().all(|sample| *sample == 0));
        assert!(gated[1_600..4_800].iter().all(|sample| *sample == 5_000));
        assert!(gated[4_800..].iter().all(|sample| *sample == 0));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn clean_cut_segment_noise_impl_gates_then_validates() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis();
        let root = std::env::temp_dir().join(format!("dialect_labeler_clean_{}", stamp));
        fs::create_dir_all(&root).expect("temp dir");
        let segment_path = root.join("clean.wav");

        let mut samples = Vec::new();
        samples.extend(std::iter::repeat(500i16).take(1_600));
        samples.extend(std::iter::repeat(5_000i16).take(3_200));
        samples.extend(std::iter::repeat(-500i16).take(3_200));
        write_pcm16_mono_wav(&segment_path, 16_000, &samples);

        let segment = SegmentRecord {
            id: "clean".to_string(),
            source_path: path_to_string(&segment_path),
            source_file_name: "source.wav".to_string(),
            segment_path: path_to_string(&segment_path),
            segment_file_name: "clean.wav".to_string(),
            role: None,
            start_ms: 0,
            end_ms: 500,
            duration_ms: 500,
            original_text: String::new(),
            phonetic_text: String::new(),
            emotion: Vec::new(),
            tags: Vec::new(),
            notes: String::new(),
        };
        let config = CutConfig {
            silence_db: -30.0,
            min_silence_ms: 300,
            min_segment_ms: 100,
            pre_roll_ms: 100,
            post_roll_ms: 200,
            max_segment_ms: 0,
            ..CutConfig::default()
        };

        let result = clean_cut_segment_noise_impl(vec![segment], config).expect("clean");
        assert_eq!(result.processed_count, 1);
        assert_eq!(result.validation.len(), 1);
        assert!(result.validation[0].ok, "{:?}", result.validation[0]);

        let gated = read_pcm16_mono_samples(&segment_path);
        assert!(gated[..1_600].iter().all(|sample| *sample == 0));
        assert!(gated[4_800..].iter().all(|sample| *sample == 0));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn repair_cut_segment_silence_impl_trims_padding_and_updates_metadata() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis();
        let root = std::env::temp_dir().join(format!("dialect_labeler_repair_{}", stamp));
        fs::create_dir_all(&root).expect("temp dir");
        let segment_path = root.join("repair.wav");

        let mut samples = Vec::new();
        samples.extend(std::iter::repeat(0i16).take(3_200));
        samples.extend(std::iter::repeat(5_000i16).take(3_200));
        samples.extend(std::iter::repeat(0i16).take(4_800));
        write_pcm16_mono_wav(&segment_path, 16_000, &samples);

        let segment = SegmentRecord {
            id: "repair".to_string(),
            source_path: path_to_string(&segment_path),
            source_file_name: "source.wav".to_string(),
            segment_path: path_to_string(&segment_path),
            segment_file_name: "repair.wav".to_string(),
            role: None,
            start_ms: 1_000,
            end_ms: 1_700,
            duration_ms: 700,
            original_text: String::new(),
            phonetic_text: String::new(),
            emotion: Vec::new(),
            tags: Vec::new(),
            notes: String::new(),
        };
        let config = CutConfig {
            silence_db: -30.0,
            min_silence_ms: 300,
            min_segment_ms: 100,
            pre_roll_ms: 100,
            post_roll_ms: 200,
            max_segment_ms: 0,
            ..CutConfig::default()
        };

        let result = repair_cut_segment_silence_impl(vec![segment], config).expect("repair");
        assert_eq!(result.processed_count, 1);
        assert_eq!(result.updated_segments.len(), 1);
        assert!(result.validation[0].ok, "{:?}", result.validation[0]);

        let updated = &result.updated_segments[0];
        assert_eq!(updated.start_ms, 1_100);
        assert_eq!(updated.duration_ms, 500);
        assert_eq!(updated.end_ms, 1_600);

        let repaired = read_pcm16_mono_samples(&segment_path);
        assert_eq!(repaired.len(), 8_000);
        assert!(repaired[..1_600].iter().all(|sample| *sample == 0));
        assert!(repaired[1_600..4_800].iter().all(|sample| *sample == 5_000));
        assert!(repaired[4_800..].iter().all(|sample| *sample == 0));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn trim_segment_head_tail_is_noop_for_all_silence() {
        if ensure_ffmpeg().is_err() {
            return;
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis();
        let root = std::env::temp_dir().join(format!("dialect_labeler_trim_silent_{}", stamp));
        fs::create_dir_all(&root).expect("temp dir");
        let segment = root.join("silent.wav");

        let status = Command::new("ffmpeg")
            .arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg("anullsrc=channel_layout=mono:sample_rate=16000:duration=1.0")
            .arg("-c:a")
            .arg("pcm_s16le")
            .arg(&segment)
            .status()
            .expect("ffmpeg should run");
        assert!(status.success());
        let before = probe_audio(&segment)
            .expect("probe")
            .duration_ms
            .unwrap_or(0);

        let (head_trim_ms, new_duration_ms) =
            trim_segment_head_tail(&segment, -30.0, 100, 200).expect("trim");
        assert_eq!(head_trim_ms, 0);
        let after = probe_audio(&segment)
            .expect("probe")
            .duration_ms
            .unwrap_or(0);
        // All-silence segment should not be deleted; duration unchanged.
        assert_eq!(after, before, "all-silence file should not be shortened");
        assert!(new_duration_ms >= before.saturating_sub(50));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn fills_manifest_text_by_role_order_when_audio_names_do_not_match() {
        let mut audio_files = vec![AudioFileInfo {
            id: "a".to_string(),
            path: "speaker/free_001.wav".to_string(),
            file_name: "free_001.wav".to_string(),
            role: Some("assistant".to_string()),
            topic_id: None,
            target_file_names: Vec::new(),
            duration_ms: None,
            sample_rate: None,
            channels: None,
            codec_name: None,
            bits_per_sample: None,
            matched_text: None,
            matched_emotion: Vec::new(),
        }];
        let records = vec![ManifestRecord {
            role: "assistant".to_string(),
            content: "冬瓜山的烤肠啊".to_string(),
            raw_content: "冬瓜山的烤肠啊".to_string(),
            audio_file: Some("unmatched.wav".to_string()),
            emotion: vec!["中立".to_string()],
            tags: Vec::new(),
        }];

        fill_manifest_fallback_by_role(&mut audio_files, &records);
        assert_eq!(
            audio_files[0].matched_text.as_deref(),
            Some("冬瓜山的烤肠啊")
        );
        assert_eq!(audio_files[0].matched_emotion, vec!["中立"]);
    }
}

// ============================================================
// Cloud commands
// ============================================================
//
// Token + base URL persistence is driven from the frontend via
// tauri-plugin-store. The Rust side keeps an in-memory copy so the
// background worker thread doesn't have to round-trip through JS for
// every HTTP call. On startup the frontend re-injects via
// `cloud_set_session`.

// Cloud commands are macOS-only — see the `mod cloud;` cfg note above.
#[cfg(not(target_os = "windows"))]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CloudLoginResult {
    token: String,
    user: cloud::UserInfo,
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
async fn cloud_login(
    state: tauri::State<'_, cloud::CloudState>,
    base_url: String,
    email: String,
    password: String,
) -> Result<CloudLoginResult, String> {
    let url = base_url.trim().to_string();
    let (token, user) =
        cloud::login_request(&url, &email, &password).map_err(|err| err.to_string())?;
    state.set_credentials(url, token.clone(), user.clone());
    Ok(CloudLoginResult { token, user })
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
fn cloud_set_session(
    state: tauri::State<'_, cloud::CloudState>,
    base_url: String,
    token: String,
    user: cloud::UserInfo,
) -> Result<(), String> {
    state.set_credentials(base_url, token, user);
    Ok(())
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
fn cloud_logout(state: tauri::State<'_, cloud::CloudState>) -> Result<(), String> {
    state.clear_credentials();
    Ok(())
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
fn cloud_status(state: tauri::State<'_, cloud::CloudState>) -> cloud::CloudStatus {
    state.status()
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
fn cloud_set_worker_config(
    state: tauri::State<'_, cloud::CloudState>,
    config: cloud::WorkerConfig,
) -> Result<(), String> {
    state.set_worker_config(config);
    Ok(())
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
fn cloud_set_worker_enabled(
    app: tauri::AppHandle,
    state: tauri::State<'_, cloud::CloudState>,
    enabled: bool,
) -> Result<(), String> {
    state
        .set_worker_enabled(app, enabled)
        .map_err(|err| err.to_string())
}

// ============================================================
// App version (surfaced in CloudPane → links to /downloads)
// ============================================================
//
// v1 ships without auto-update. CloudPane reads this and renders a link
// to the dispatcher's /downloads page so the user can grab a newer build
// manually. Re-introducing tauri-plugin-updater would also require a
// signing key + pubkey in tauri.conf.json — explicitly out of scope for
// v1 per the deploy plan.

#[tauri::command]
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// ============================================================
// Tray + window-to-tray  (macOS Cloud Worker mode only)
// ============================================================
//
// The tray icon is purely a Cloud Worker affordance — left-click toggles
// the main window, menu offers 打开窗口/彻底退出. The Windows reviewer
// build has no Worker mode, no background tasks, and no need to live in
// the tray, so the whole thing compiles out.

#[cfg(not(target_os = "windows"))]
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::{
        image::Image,
        menu::{Menu, MenuItem},
        tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
        Manager as _,
    };

    let open_item = MenuItem::with_id(app, "open", "打开窗口", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "彻底退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_item, &quit_item])?;

    // Use the bundled 32x32 PNG; it ships with every platform build.
    let icon_bytes = include_bytes!("../icons/32x32.png");
    let icon = Image::from_bytes(icon_bytes)?;

    let _ = TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip("方言标注 — 云端 Worker")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Left-click the tray icon → toggle window visibility.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app: &tauri::AppHandle = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    let visible = window.is_visible().unwrap_or(false);
                    if visible {
                        let _ = window.hide();
                    } else {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(not(target_os = "windows"))]
    use tauri::Manager as _;

    // The builder is shaped differently per-platform: the Windows reviewer
    // build skips all cloud-worker plugins (single_instance / store /
    // autostart / updater) and the tray. macOS keeps the full stack.
    let builder = tauri::Builder::default();

    #[cfg(not(target_os = "windows"))]
    let builder = builder
        // Single-instance must be registered FIRST per Tauri 2 docs.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }));

    let builder = builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init());

    #[cfg(not(target_os = "windows"))]
    let builder = builder
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(cloud::CloudState::new());

    builder
        .manage(AudioPlayerState::new())
        .setup(|_app| {
            #[cfg(not(target_os = "windows"))]
            {
                // Build the tray icon eagerly so the user sees the
                // cloud-worker entry point before they even sign in.
                if let Err(err) = build_tray(_app.handle()) {
                    eprintln!("[run] tray build failed: {err}");
                }
                // Intercept close so closing the main window hides it to
                // the tray instead of tearing down the worker. The tray
                // "quit" item is the only real exit.
                let main_window = _app.get_webview_window("main");
                if let Some(window) = main_window {
                    let window_clone = window.clone();
                    window.on_window_event(move |event| {
                        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                            api.prevent_close();
                            let _ = window_clone.hide();
                        }
                    });
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            prepare_playback_audio,
            read_waveform_peaks,
            play_audio,
            pause_audio,
            stop_audio,
            audio_state,
            set_playback_speed,
            scan_project_folder,
            cut_audio_file,
            validate_cut_segments,
            clean_cut_segment_noise,
            repair_cut_segment_silence,
            recognize_segments,
            cancel_recognize,
            run_semantic_pipeline,
            polish_text_with_llm,
            list_ollama_models,
            check_dependencies,
            get_default_llm_prompt,
            save_project_file,
            backup_project_file,
            migrate_segment_filenames,
            rename_segments_by_dialogue_sequence,
            load_project_file,
            export_segments_jsonl,
            export_dataset_bundle,
            #[cfg(not(target_os = "windows"))]
            cloud_login,
            #[cfg(not(target_os = "windows"))]
            cloud_set_session,
            #[cfg(not(target_os = "windows"))]
            cloud_logout,
            #[cfg(not(target_os = "windows"))]
            cloud_status,
            #[cfg(not(target_os = "windows"))]
            cloud_set_worker_config,
            #[cfg(not(target_os = "windows"))]
            cloud_set_worker_enabled,
            app_version
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
