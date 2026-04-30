use regex::Regex;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
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
fn silent_command<S: AsRef<std::ffi::OsStr>>(program: S) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CutConfig {
    silence_db: f32,
    min_silence_ms: u64,
    min_segment_ms: u64,
    pre_roll_ms: u64,
    post_roll_ms: u64,
    /// Hard upper bound on segment duration. Any silence-bounded range that
    /// exceeds this is force-split into ⌈len / max⌉ equal-ish chunks. Set to
    /// 0 (or omit) to disable. Default 30000 ms aligns with the labeling
    /// spec's "最长不超过 30s".
    #[serde(default)]
    max_segment_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SegmentRecord {
    id: String,
    source_path: String,
    source_file_name: String,
    segment_path: String,
    segment_file_name: String,
    role: Option<String>,
    start_ms: u64,
    end_ms: u64,
    duration_ms: u64,
    original_text: String,
    phonetic_text: String,
    emotion: Vec<String>,
    tags: Vec<String>,
    notes: String,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OllamaEndpointDef {
    url: String,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RecognitionOptions {
    #[serde(default)]
    whisper_model: Option<String>,
    #[serde(default)]
    use_llm: Option<bool>,
    /// Primary Ollama endpoint (backwards-compat).
    #[serde(default)]
    ollama_url: Option<String>,
    /// Optional pool of *additional* Ollama endpoints (URL only — uses the
    /// primary `ollama_model`). Kept for backwards-compat; prefer
    /// `ollama_extra_endpoints` when each endpoint needs its own model.
    #[serde(default)]
    ollama_extra_urls: Option<Vec<String>>,
    /// Per-endpoint URL + model. If `model` is None for an entry, the
    /// primary `ollama_model` is used. Lets you mix e.g. local qwen2.5:32b
    /// with a remote qwen3.5:122b in the same pool.
    #[serde(default)]
    ollama_extra_endpoints: Option<Vec<OllamaEndpointDef>>,
    #[serde(default)]
    ollama_model: Option<String>,
    #[serde(default)]
    llm_prompt: Option<String>,
    #[serde(default)]
    initial_prompt: Option<String>,
    #[serde(default)]
    use_cache: Option<bool>,
    #[serde(default)]
    overwrite_cache: Option<bool>,
    /// Maximum number of Ollama HTTP requests fired in parallel within a
    /// batch. Default 2. With multiple endpoints set this to N × endpoints
    /// to fully saturate the pool.
    #[serde(default)]
    llm_concurrency: Option<u32>,
    /// Number of concurrent Whisper processes. Default 1 — most setups
    /// can only afford one model copy in GPU/RAM. With a beefy box, 2-3
    /// can keep the LLM pool fed faster. Each process loads its own
    /// model (so RAM cost scales linearly).
    #[serde(default)]
    whisper_concurrency: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecognitionResult {
    segment_id: String,
    text: String,
    raw_text: String,
    polished: bool,
    cached: bool,
    emotion: Option<String>,
    tags: Vec<String>,
    /// Which Ollama endpoint actually polished this segment, for UI badges.
    /// `None` for cached results or when LLM polish was skipped/failed.
    polish_endpoint: Option<String>,
    polish_model: Option<String>,
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
struct ExportOptions {
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    pair_user_assistant: Option<bool>,
    #[serde(default)]
    use_source_audio_for_user: Option<bool>,
    #[serde(default)]
    audio_file_prefix: Option<String>,
    /// Local input root used to compute the path relative to the
    /// configured OSS prefix. Without it, only the file name is appended.
    #[serde(default)]
    input_root: Option<String>,
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

fn scan_project_folder_impl(
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

    let mut audio_files = Vec::new();
    for path in files.iter().filter(|path| is_audio(path)) {
        let probe = probe_audio(path).ok();
        let file_name = path_file_name(path);
        let matched = match_manifest(path, &manifest_records);
        let role = matched
            .as_ref()
            .map(|record| record.role.clone())
            .or_else(|| infer_role(path));
        let related_records =
            matching_manifest_records_for_source(path, role.as_deref(), &manifest_records);
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
            original_text,
            emotion,
            target_file_names.unwrap_or_default(),
        )
    })
    .await
    .map_err(|err| err.to_string())?
}

fn cut_audio_file_impl(
    input_path: String,
    segments_dir: String,
    config: CutConfig,
    role: Option<String>,
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

    let events = detect_silence(&input, &config)?;
    let ranges = build_segment_ranges(duration_sec, &events, &config);
    let codec = output_pcm_codec(&probe);
    let mut segments = Vec::new();
    let text = original_text.clone().unwrap_or_default();
    let text_chunks = split_text_by_ranges(&text, &ranges);

    for (index, (start_sec, end_sec)) in ranges.iter().enumerate() {
        let start_ms = seconds_to_ms(*start_sec);
        let end_ms = seconds_to_ms(*end_sec);
        let segment_file_name = build_segment_file_name(
            &input,
            role.as_deref(),
            &target_file_names,
            index,
            start_ms,
            end_ms,
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
            duration_ms: end_ms.saturating_sub(start_ms),
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

/// Make a timestamped copy of the project.json so a destructive operation
/// (e.g. re-cutting an audio whose segments were already annotated) can be
/// rolled back manually. Returns the backup path. Silently no-ops if
/// project.json doesn't exist yet.
/// One-shot migration: rename segment WAV files on disk so they match
/// the demo `<base>_<NN>_<role>.wav` layout, then return updated
/// `SegmentRecord`s with new `segment_path` / `segment_file_name`. The
/// frontend persists the result via `save_project_file`.
///
/// **Why a separate command?** Older builds wrote segments as
/// `<source>_<NNNN>.wav` (no role suffix) when the source filename had
/// the role token in the middle (`长沙方言-...-发音人-话题1`). The new
/// `split_role_suffix` recognizes those, but already-cut projects still
/// have the old names. Re-cutting throws away ASR + polish state, so
/// we rename in place instead.
///
/// Behavior:
///   - Groups segments by source, sorts each group by `start_ms`, and
///     assigns 1-based sub-indices in temporal order.
///   - For sources whose stem contains a role token (anywhere), the
///     new name is `<base>_<NN>_<role>.wav`.
///   - For sources without a role token, names stay as the legacy
///     `<source>_<NNNN>.wav` format — nothing to migrate.
///   - If the new name equals the existing one, the segment is skipped
///     (no rename, no record change).
///   - If a target name already exists on disk we fail with an error
///     message containing the conflicting path — never silently
///     overwrite a user's data.
#[tauri::command]
fn migrate_segment_filenames(
    project_dir: String,
    segments: Vec<SegmentRecord>,
) -> Result<Vec<SegmentRecord>, String> {
    let _ = project_dir; // not used directly — segment.segment_path is absolute
                         // Group segments by source_path; preserve original order for output.
    let mut by_source: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, seg) in segments.iter().enumerate() {
        by_source
            .entry(seg.source_path.clone())
            .or_default()
            .push(idx);
    }
    // Within each source, sort by start_ms — the sub-index then matches
    // temporal order, just like cut_audio_file_impl produced originally.
    for indices in by_source.values_mut() {
        indices.sort_by_key(|&i| segments[i].start_ms);
    }

    let mut updated = segments.clone();
    let mut renamed = 0usize;
    for indices in by_source.values() {
        if let Some(&first) = indices.first() {
            let source_stem = path_file_stem(Path::new(&segments[first].source_path));
            let (base_stem, role_suffix) = split_role_suffix(&source_stem);
            // No role token in the source — nothing to migrate. Legacy
            // `<source>_<NNNN>.wav` names stay as-is.
            let Some(role) = role_suffix else { continue };
            let (true_base_stem, turn_number) = extract_turn_from_base(&base_stem);
            let safe_base = safe_name(&true_base_stem);
            let total = indices.len();

            for (sub, &idx) in indices.iter().enumerate() {
                let seg = &segments[idx];
                // Spec rule (matches cut_audio_file_impl):
                //   1 segment per turn  → `<base>_<turn>_<role>.wav`
                //   N segments per turn → `<base>_<turn>_<sub>_<role>.wav`
                let new_name = if total == 1 {
                    format!("{}_{:02}_{}.wav", safe_base, turn_number, role)
                } else {
                    format!(
                        "{}_{:02}_{:02}_{}.wav",
                        safe_base,
                        turn_number,
                        sub + 1,
                        role
                    )
                };
                if seg.segment_file_name == new_name {
                    continue;
                }
                let old_path = PathBuf::from(&seg.segment_path);
                let parent = old_path
                    .parent()
                    .ok_or_else(|| format!("无法定位段文件父目录：{}", seg.segment_path))?;
                let new_path = parent.join(&new_name);
                if new_path.exists() {
                    return Err(format!(
                        "迁移目标已存在，拒绝覆盖：{}（来源段：{}）",
                        path_to_string(&new_path),
                        seg.segment_file_name
                    ));
                }
                if old_path.is_file() {
                    fs::rename(&old_path, &new_path).map_err(|err| {
                        format!(
                            "重命名失败 {} → {}：{}",
                            seg.segment_file_name, new_name, err
                        )
                    })?;
                    renamed += 1;
                }
                let entry = &mut updated[idx];
                entry.segment_path = path_to_string(&new_path);
                entry.segment_file_name = new_name;
            }
        }
    }
    eprintln!(
        "[migrate] renamed {} of {} segments to demo layout",
        renamed,
        segments.len()
    );
    Ok(updated)
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

fn export_dataset_bundle_impl(
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
    // annotations intact. Paths are already pointed at the bundle's own
    // segments/ + source/ subdirectories from the copy step above, so the
    // file is portable to any machine that opens the same directory.
    let saved_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let project_payload = serde_json::json!({
        "version": 2,
        "savedAt": format!("epoch_ms_{}", saved_at_ms),
        "rootPath": bundle_root_str.clone(),
        "projectDir": bundle_root_str.clone(),
        "segmentsDir": path_to_string(&segments_out),
        "config": {
            "silenceDb": -35.0,
            "minSilenceMs": 450,
            "minSegmentMs": 300,
            "preRollMs": 100,
            "postRollMs": 200,
            "maxSegmentMs": 30000
        },
        "audioFiles": Vec::<Value>::new(),
        "manifestRecords": Vec::<Value>::new(),
        "segments": new_segments,
        "systemPrompt": system_prompt
    });
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
        let mut message = json!({
            "role": role,
            "content": content_value,
            "audio_file": audio_path,
        });
        if !segment.emotion.is_empty() {
            message["emotion"] = json!(segment.emotion);
        }
        // `tags` deliberately omitted — paralinguistic markers live
        // inline in the text (e.g. `[breath]`, `<laugh>...</laugh>`),
        // and the dataset spec doesn't carry a separate tags field.
        if !segment.notes.trim().is_empty() {
            message["notes"] = json!(segment.notes);
        }
        messages.push(message);
        // Pretty-print to match the demo `自由演绎.jsonl` layout —
        // technically not strict JSONL anymore, but readable by `jq -s`
        // and friends because every record is still a self-contained
        // JSON object separated by a blank line.
        let line = serde_json::to_string_pretty(&json!({"messages": messages}))
            .map_err(|err| err.to_string())?;
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
/// Single-segment turns collapse `audio_file` and `emotion` from arrays
/// to scalars to match the demo's "single string when only one" pattern.
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
        // Text: concatenate all user sub-segments. Chinese has no word
        // boundary so we join with the empty string — adjacent segments
        // already include their own punctuation.
        if !user_segs.is_empty() {
            let user_text = user_segs
                .iter()
                .map(|s| pick_text(s))
                .collect::<Vec<_>>()
                .join("");
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
            messages.push(json!({
                "role": "user",
                "content": user_text,
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

        let single = assistant_segs.len() == 1;
        let audio_value: Value = if single {
            audios[0].clone()
        } else {
            Value::Array(audios)
        };
        let mut a_msg = json!({
            "role": "assistant",
            "content": Value::Array(texts),
            "audio_file": audio_value,
        });
        // Emit `emotion` only when the labelers actually picked one — an
        // all-empty group means the polish step never ran or was cleared.
        if assistant_segs.iter().any(|s| !s.emotion.is_empty()) {
            a_msg["emotion"] = if single {
                emotions[0].clone()
            } else {
                Value::Array(emotions)
            };
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

        lines.push(
            serde_json::to_string_pretty(&json!({"messages": messages}))
                .map_err(|err| err.to_string())?,
        );

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
    let content = if role == "assistant" {
        json!([text])
    } else {
        json!(text)
    };
    let mut msg = json!({
        "role": role,
        "content": content,
        "audio_file": with_prefix(prefix, &segment.segment_path, input_root),
    });
    if !segment.emotion.is_empty() {
        msg["emotion"] = json!(segment.emotion);
    }
    // `tags` deliberately omitted — see build_paired_jsonl.
    if !segment.notes.trim().is_empty() {
        msg["notes"] = json!(segment.notes);
    }
    messages.push(msg);
    serde_json::to_string_pretty(&json!({"messages": messages})).map_err(|err| err.to_string())
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

fn parse_dataset_source_audio_name(
    path: &Path,
    role_hint: Option<&str>,
) -> Option<DatasetSourceName> {
    let stem = path_file_stem(path);
    let mode = dataset_mode(&stem)?;
    let topic_id = last_number(&stem)?;
    let role = role_hint
        .map(|value| value.to_string())
        .or_else(|| infer_role(path));

    Some(DatasetSourceName {
        mode,
        topic_id,
        role,
    })
}

fn parse_segment_output_name(value: &str) -> Option<SegmentOutputName> {
    let file_name = target_audio_file_name(value);
    let stem = Path::new(&file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(file_name.as_str());
    let re = Regex::new(r"^(文案演绎|自由演绎)_(\d+)_(\d+)(?:_(\d+))?_(陪聊|发音人)$")
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

    if let Some(source) = parse_dataset_source_audio_name(input, role_hint) {
        let round_index = index + 1;
        match source.role.as_deref() {
            Some("user") => {
                return format!(
                    "{}_{:04}_{:02}_陪聊.wav",
                    source.mode, source.topic_id, round_index
                );
            }
            Some("assistant") => {
                return format!(
                    "{}_{:04}_{:02}_{:02}_发音人.wav",
                    source.mode, source.topic_id, round_index, 1
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

fn dataset_mode(value: &str) -> Option<String> {
    ["文案演绎", "自由演绎"]
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
    if lower_extension(Path::new(&file_name)).as_deref() == Some("wav") {
        file_name
    } else {
        format!("{}.wav", file_name.trim_end_matches('.'))
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
    if let Some(parts) = parse_segment_output_name(&segment.segment_file_name) {
        return format!(
            "{}_{:04}_{:02}",
            parts.key.mode, parts.key.topic_id, parts.key.round_index
        );
    }

    let stem = path_file_stem(Path::new(&segment.source_path));
    // (left_sep) (role) (right_sep | end)
    let re =
        Regex::new(r"([_\-])(?:发音人|assistant|陪聊|user)(?:([_\-])|$)").expect("valid regex");
    let cleaned = re
        .replace(&stem, |caps: &regex::Captures| {
            // If a right separator was captured we're in the middle of
            // the stem — keep one separator so adjacent tokens don't
            // fuse. If not, the role was at the end — drop everything.
            match caps.get(2) {
                Some(right) => right.as_str().to_string(),
                None => String::new(),
            }
        })
        .to_string();
    let trimmed = cleaned.trim_end_matches(['_', '-']).to_string();
    if trimmed.is_empty() {
        stem
    } else {
        trimmed
    }
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
fn split_role_suffix(stem: &str) -> (String, Option<String>) {
    // (left_sep) (role) (right_sep | end_of_string)
    let re = Regex::new(r"([_\-])(发音人|assistant|陪聊|user)(?:([_\-])|$)").expect("valid regex");
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
fn recognize_segments_impl(
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
    let whisper = find_whisper_command()?;
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

    if !to_run.is_empty() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_millis())
            .unwrap_or_default();
        let output_dir = project_dir.join(".asr").join(format!("run_{}", stamp));
        fs::create_dir_all(&output_dir).map_err(|err| err.to_string())?;

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

            // URL normaliser — tolerates user typos.
            fn normalise_url(raw: &str) -> Option<String> {
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

            let mut endpoints: Vec<(String, String)> = Vec::new();
            if let Some(url) = normalise_url(&ollama_url) {
                endpoints.push((url, model_name.clone()));
            }
            if let Some(defs) = options.ollama_extra_endpoints.as_ref() {
                for def in defs {
                    let Some(url) = normalise_url(&def.url) else {
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
                    let Some(url) = normalise_url(url) else {
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
    records: &'a [ManifestRecord],
) -> Vec<&'a ManifestRecord> {
    let Some(source) = parse_dataset_source_audio_name(path, role_hint) else {
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
    let pre_roll = config.pre_roll_ms as f64 / 1000.0;
    let post_roll = config.post_roll_ms as f64 / 1000.0;

    for event in events {
        match event {
            SilenceEvent::Start(start) => {
                push_padded_range(
                    &mut ranges,
                    cursor,
                    *start,
                    duration_sec,
                    min_segment,
                    pre_roll,
                    post_roll,
                );
            }
            SilenceEvent::End(end) => {
                cursor = (*end).min(duration_sec).max(0.0);
            }
        }
    }

    push_padded_range(
        &mut ranges,
        cursor,
        duration_sec,
        duration_sec,
        min_segment,
        pre_roll,
        post_roll,
    );

    if ranges.is_empty() && duration_sec > 0.0 {
        ranges.push((0.0, duration_sec));
    }

    // Enforce maxSegmentMs as a hard upper bound — any silence-free stretch
    // longer than this gets force-split into N equal-ish chunks. The split
    // doesn't try to land on natural boundaries (no silence info available),
    // it just slices time uniformly. Users can still tweak silence params
    // first; this is a safety net for the spec's 30s limit.
    if config.max_segment_ms > 0 {
        let max_sec = config.max_segment_ms as f64 / 1000.0;
        let mut adjusted = Vec::with_capacity(ranges.len());
        for (start, end) in ranges {
            let len = end - start;
            if len > max_sec {
                let chunks = (len / max_sec).ceil() as usize;
                let chunk_len = len / chunks as f64;
                for i in 0..chunks {
                    let cs = start + chunk_len * i as f64;
                    let ce = (cs + chunk_len).min(end);
                    adjusted.push((cs, ce));
                }
            } else {
                adjusted.push((start, end));
            }
        }
        return adjusted;
    }

    ranges
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
            build_segment_file_name(assistant, Some("assistant"), &[], 0, 0, 1000),
            "自由演绎_0001_01_01_发音人.wav"
        );

        let user = Path::new("/tmp/长沙方言-0413-文案演绎-陪聊-话题12.wav");
        assert_eq!(
            build_segment_file_name(user, Some("user"), &[], 2, 0, 1000),
            "文案演绎_0012_03_陪聊.wav"
        );

        let targets = vec!["oss://bucket/WAV/自由演绎_0001_01_02_发音人.wav".to_string()];
        assert_eq!(
            build_segment_file_name(assistant, Some("assistant"), &targets, 0, 0, 1000),
            "自由演绎_0001_01_02_发音人.wav"
        );
    }

    #[test]
    fn paired_export_groups_by_dataset_segment_file_name() {
        let user = SegmentRecord {
            id: "u".to_string(),
            source_path: "/input/长沙方言-0413-自由演绎-陪聊-惊讶1.wav".to_string(),
            source_file_name: "长沙方言-0413-自由演绎-陪聊-惊讶1.wav".to_string(),
            segment_path: "/out/segments/自由演绎_0001_01_陪聊.wav".to_string(),
            segment_file_name: "自由演绎_0001_01_陪聊.wav".to_string(),
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
        assert_eq!(
            messages[1]["audio_file"],
            "/out/segments/自由演绎_0001_01_陪聊.wav"
        );
        assert_eq!(
            messages[2]["audio_file"],
            "/out/segments/自由演绎_0001_01_01_发音人.wav"
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
        assert!(messages[2]["content"].is_array());
        // Single assistant sub-segment → audio_file collapses to a string
        // and `emotion` is a single string (matching demo's pattern for
        // length-1 turns).
        assert!(messages[2]["audio_file"].is_string());
        assert_eq!(messages[2]["emotion"], "中立");
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
        let content = assistant_msg["content"].as_array().unwrap();
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

        let emotions = assistant_msg["emotion"].as_array().unwrap();
        assert_eq!(emotions, &vec![json!("中立"), json!("开心"), json!("中立")]);
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
        };

        let segments = cut_audio_file_impl(
            path_to_string(&input),
            path_to_string(&segments_dir),
            config,
            Some("assistant".to_string()),
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
        // 50s of audio, no silence detected → one big range; with max=20s
        // we expect ⌈50/20⌉ = 3 chunks of ~16.67s each.
        let config = CutConfig {
            silence_db: -35.0,
            min_silence_ms: 450,
            min_segment_ms: 300,
            pre_roll_ms: 100,
            post_roll_ms: 200,
            max_segment_ms: 20_000,
        };
        let ranges = build_segment_ranges(50.0, &[], &config);
        assert_eq!(ranges.len(), 3);
        assert!((ranges[0].1 - ranges[0].0 - 50.0 / 3.0).abs() < 0.001);
        // No max → keeps the long single range.
        let unbounded = CutConfig {
            max_segment_ms: 0,
            ..config
        };
        let ranges = build_segment_ranges(50.0, &[], &unbounded);
        assert_eq!(ranges.len(), 1);
    }

    #[test]
    fn splits_text_across_segments_by_duration() {
        let chunks = split_text_by_ranges("冬瓜山的烤肠啊", &[(0.0, 1.0), (1.0, 3.0), (3.0, 4.0)]);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.concat(), "冬瓜山的烤肠啊");
        assert!(chunks[1].chars().count() >= chunks[0].chars().count());
    }

    #[test]
    fn fills_manifest_text_by_role_order_when_audio_names_do_not_match() {
        let mut audio_files = vec![AudioFileInfo {
            id: "a".to_string(),
            path: "speaker/free_001.wav".to_string(),
            file_name: "free_001.wav".to_string(),
            role: Some("assistant".to_string()),
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AudioPlayerState::new())
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
            recognize_segments,
            cancel_recognize,
            polish_text_with_llm,
            list_ollama_models,
            check_dependencies,
            get_default_llm_prompt,
            save_project_file,
            backup_project_file,
            migrate_segment_filenames,
            load_project_file,
            export_segments_jsonl,
            export_dataset_bundle
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
