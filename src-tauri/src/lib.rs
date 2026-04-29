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

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RecognitionOptions {
    #[serde(default)]
    whisper_model: Option<String>,
    #[serde(default)]
    use_llm: Option<bool>,
    #[serde(default)]
    ollama_url: Option<String>,
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

#[derive(Default)]
struct AudioPlayerInner {
    stream: Option<OutputStream>,
    handle: Option<OutputStreamHandle>,
    sink: Option<Sink>,
    path: Option<String>,
    duration_ms: u64,
    offset_ms: u64,
    started_at: Option<Instant>,
    is_playing: bool,
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

    let mut command = Command::new("ffmpeg");
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
    ensure_ffmpeg()?;
    let bucket_count = bucket_count.clamp(64, 4096);
    let input = PathBuf::from(&input_path);
    if !input.is_file() {
        return Err(format!("Audio file not found: {}", input_path));
    }

    let mut child = Command::new("ffmpeg")
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
    Ok(peaks)
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
            position = position.saturating_add(started_at.elapsed().as_millis() as u64);
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

        audio_files.push(AudioFileInfo {
            id: stable_id(path),
            path: path_to_string(path),
            file_name,
            role,
            duration_ms: probe.as_ref().and_then(|info| info.duration_ms),
            sample_rate: probe.as_ref().and_then(|info| info.sample_rate),
            channels: probe.as_ref().and_then(|info| info.channels),
            codec_name: probe.as_ref().and_then(|info| info.codec_name.clone()),
            bits_per_sample: probe.as_ref().and_then(|info| info.bits_per_sample),
            matched_text: matched.as_ref().map(|record| record.content.clone()),
            matched_emotion: matched
                .as_ref()
                .map(|record| record.emotion.clone())
                .unwrap_or_default(),
        });
    }

    audio_files.sort_by(|left, right| left.path.cmp(&right.path));
    fill_manifest_fallback_by_role(&mut audio_files, &manifest_records);

    // Auto-load project.json if it already exists so the user resumes where
    // they left off after re-scanning the same folder.
    let existing_project = fs::read_to_string(project_dir.join("project.json"))
        .ok()
        .and_then(|data| serde_json::from_str::<Value>(&data).ok());

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
) -> Result<Vec<SegmentRecord>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        cut_audio_file_impl(
            input_path,
            segments_dir,
            config,
            role,
            original_text,
            emotion,
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
    let source_stem = path_file_stem(&input);
    let safe_source = safe_name(&source_stem);
    let (base_stem, role_suffix) = split_role_suffix(&source_stem);
    let safe_base = safe_name(&base_stem);
    let source_dir = segments_root.join(&safe_source);
    fs::create_dir_all(&source_dir).map_err(|err| err.to_string())?;

    let codec = output_pcm_codec(&probe);
    let mut segments = Vec::new();
    let text = original_text.clone().unwrap_or_default();
    let text_chunks = split_text_by_ranges(&text, &ranges);

    for (index, (start_sec, end_sec)) in ranges.iter().enumerate() {
        let start_ms = seconds_to_ms(*start_sec);
        let end_ms = seconds_to_ms(*end_sec);
        // Demo convention: insert 2-digit segment number BEFORE role suffix.
        // E.g. `自由演绎_0001_02_发音人.wav` → `自由演绎_0001_02_01_发音人.wav`.
        // For files without a role suffix, append a 4-digit sequence number.
        let segment_file_name = match &role_suffix {
            Some(suffix) => format!("{}_{:02}_{}.wav", safe_base, index + 1, suffix),
            None => format!("{}_{:04}.wav", safe_source, index + 1),
        };
        let output = source_dir.join(&segment_file_name);
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
            id: format!("{}_{:04}", safe_source, index + 1),
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
    let dir = PathBuf::from(project_dir);
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let path = dir.join("project.json");
    let data = serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?;
    fs::write(&path, data).map_err(|err| err.to_string())?;
    Ok(path_to_string(&path))
}

#[tauri::command]
fn load_project_file(project_dir: String) -> Result<Value, String> {
    let path = PathBuf::from(project_dir).join("project.json");
    let data = fs::read_to_string(&path).map_err(|err| err.to_string())?;
    serde_json::from_str(&data).map_err(|err| err.to_string())
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
    let bundle_canonical = bundle
        .canonicalize()
        .map_err(|err| err.to_string())?;
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
            total_bytes = total_bytes.saturating_add(
                fs::metadata(&dst).map(|m| m.len()).unwrap_or(0),
            );
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
        let dst_dir = segments_out.join(&parent_name);
        fs::create_dir_all(&dst_dir).map_err(|err| err.to_string())?;
        let dst = dst_dir.join(&segment.segment_file_name);
        fs::copy(&src, &dst).map_err(|err| err.to_string())?;
        total_bytes = total_bytes.saturating_add(
            fs::metadata(&dst).map(|m| m.len()).unwrap_or(0),
        );

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
    let user_use_source = options.use_source_audio_for_user.unwrap_or(true);
    let prefix = options.audio_file_prefix.clone().unwrap_or_default();
    // Always rewrite paths relative to the bundle root, so when the user
    // uploads the whole bundle to OSS the JSONL references line up.
    let input_root = bundle_root_str.clone();

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
    fs::write(&jsonl_path, format!("{}\n", lines.join("\n")))
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
        if source_path_map.is_empty() { "" } else { "source/ " }
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
    let user_use_source = opts.use_source_audio_for_user.unwrap_or(true);
    let prefix = opts.audio_file_prefix.unwrap_or_default();
    let input_root = opts.input_root.unwrap_or_default();

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
        if !segment.tags.is_empty() {
            message["tags"] = json!(segment.tags);
        }
        if !segment.notes.trim().is_empty() {
            message["notes"] = json!(segment.notes);
        }
        messages.push(message);
        let line =
            serde_json::to_string(&json!({"messages": messages})).map_err(|err| err.to_string())?;
        lines.push(line);
    }
    Ok(lines)
}

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
        let group = by_pair.get(&key).cloned().unwrap_or_default();
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

        let user_text = user_segs
            .iter()
            .map(|seg| {
                if seg.phonetic_text.trim().is_empty() {
                    seg.original_text.clone()
                } else {
                    seg.phonetic_text.clone()
                }
            })
            .collect::<Vec<_>>()
            .join("");
        let user_audio = user_segs.first().map(|seg| {
            if user_use_source {
                with_prefix(prefix, &seg.source_path, input_root)
            } else {
                with_prefix(prefix, &seg.segment_path, input_root)
            }
        });

        for assistant in &assistant_segs {
            let mut messages = Vec::new();
            if !system_prompt.trim().is_empty() {
                messages.push(json!({"role": "system", "content": system_prompt}));
            }
            if let Some(audio) = &user_audio {
                messages.push(json!({
                    "role": "user",
                    "content": user_text.clone(),
                    "audio_file": audio,
                }));
            }
            let assistant_text = if assistant.phonetic_text.trim().is_empty() {
                assistant.original_text.clone()
            } else {
                assistant.phonetic_text.clone()
            };
            let mut a_msg = json!({
                "role": "assistant",
                "content": [assistant_text],
                "audio_file": with_prefix(prefix, &assistant.segment_path, input_root),
            });
            if !assistant.emotion.is_empty() {
                a_msg["emotion"] = json!(assistant.emotion);
            }
            if !assistant.tags.is_empty() {
                a_msg["tags"] = json!(assistant.tags);
            }
            if !assistant.notes.trim().is_empty() {
                a_msg["notes"] = json!(assistant.notes);
            }
            messages.push(a_msg);
            lines.push(
                serde_json::to_string(&json!({"messages": messages}))
                    .map_err(|err| err.to_string())?,
            );
        }
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
    if !segment.tags.is_empty() {
        msg["tags"] = json!(segment.tags);
    }
    if !segment.notes.trim().is_empty() {
        msg["notes"] = json!(segment.notes);
    }
    messages.push(msg);
    serde_json::to_string(&json!({"messages": messages})).map_err(|err| err.to_string())
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

fn pair_key(segment: &SegmentRecord) -> String {
    let stem = path_file_stem(Path::new(&segment.source_path));
    let role = segment.role.as_deref().unwrap_or("");
    let pattern = match role {
        "user" => r"_(?:陪聊|user)$",
        "assistant" => r"_(?:\d+_)?(?:发音人|assistant)$",
        _ => r"_(?:\d+_)?(?:发音人|陪聊|user|assistant)$",
    };
    let key_re = Regex::new(pattern).expect("valid regex");
    let trimmed = key_re.replace(&stem, "").to_string();
    if trimmed.is_empty() {
        stem
    } else {
        trimmed
    }
}

/// Split a file stem into (base, optional role suffix). When the stem ends
/// with a known role marker (`_发音人`, `_陪聊`, `_assistant`, `_user`),
/// returns the base without that marker plus the marker as a string.
/// Otherwise returns (stem, None).
fn split_role_suffix(stem: &str) -> (String, Option<String>) {
    for suffix in ["发音人", "陪聊", "assistant", "user"] {
        let marker = format!("_{}", suffix);
        if let Some(base) = stem.strip_suffix(&marker) {
            return (base.to_string(), Some(suffix.to_string()));
        }
    }
    (stem.to_string(), None)
}

#[tauri::command]
async fn recognize_segments(
    project_dir: String,
    segments: Vec<SegmentRecord>,
    options: Option<RecognitionOptions>,
) -> Result<Vec<RecognitionResult>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        recognize_segments_impl(project_dir, segments, options.unwrap_or_default())
    })
    .await
    .map_err(|err| err.to_string())?
}

fn recognize_segments_impl(
    project_dir: String,
    segments: Vec<SegmentRecord>,
    options: RecognitionOptions,
) -> Result<Vec<RecognitionResult>, String> {
    if segments.is_empty() {
        return Ok(Vec::new());
    }

    ensure_ffmpeg()?;
    let whisper = find_whisper_command()?;
    let project_dir = PathBuf::from(project_dir);
    let cache_dir = project_dir.join(".asr").join("cache");
    fs::create_dir_all(&cache_dir).map_err(|err| err.to_string())?;

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

    let mut results: Vec<RecognitionResult> = Vec::with_capacity(segments.len());
    let mut to_run: Vec<SegmentRecord> = Vec::new();
    let mut cache_keys: HashMap<String, String> = HashMap::new();

    for segment in &segments {
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
                results.push(RecognitionResult {
                    segment_id: segment.id.clone(),
                    text: raw.clone(),
                    raw_text: raw,
                    polished: false,
                    cached: true,
                    emotion: None,
                    tags: Vec::new(),
                });
                continue;
            }
        }
        to_run.push(segment.clone());
    }

    if !to_run.is_empty() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_millis())
            .unwrap_or_default();
        let output_dir = project_dir.join(".asr").join(format!("run_{}", stamp));
        fs::create_dir_all(&output_dir).map_err(|err| err.to_string())?;

        let mut command = Command::new(whisper);
        command
            .arg("--model")
            .arg(&model)
            .arg("--language")
            .arg("Chinese")
            .arg("--task")
            .arg("transcribe")
            .arg("--output_format")
            .arg("json")
            .arg("--output_dir")
            .arg(&output_dir)
            .arg("--fp16")
            .arg("False")
            .arg("--verbose")
            .arg("False")
            .arg("--condition_on_previous_text")
            .arg("False")
            .arg("--initial_prompt")
            .arg(&initial_prompt);

        for segment in &to_run {
            command.arg(&segment.segment_path);
        }

        let output = command
            .output()
            .map_err(|err| format!("无法运行本地 whisper：{}", err))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(format!(
                "whisper 识别失败：{}{}",
                stderr.trim(),
                stdout.trim()
            ));
        }

        for segment in to_run {
            let segment_path = PathBuf::from(&segment.segment_path);
            let raw = read_whisper_json_text(&output_dir, &segment_path)?;
            if let Some(key) = cache_keys.get(&segment.id) {
                let cache_file = cache_dir.join(format!("{}.json", key));
                let _ = fs::write(&cache_file, json!({"text": raw}).to_string());
            }
            results.push(RecognitionResult {
                segment_id: segment.id.clone(),
                text: raw.clone(),
                raw_text: raw,
                polished: false,
                cached: false,
                emotion: None,
                tags: Vec::new(),
            });
        }
    }

    let id_to_segment: HashMap<String, &SegmentRecord> =
        segments.iter().map(|s| (s.id.clone(), s)).collect();
    let id_to_index: HashMap<String, usize> = segments
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone(), i))
        .collect();
    results.sort_by_key(|r| {
        id_to_index
            .get(&r.segment_id)
            .copied()
            .unwrap_or(usize::MAX)
    });

    if use_llm {
        let model_name = ollama_model
            .clone()
            .ok_or_else(|| "未指定 Ollama 模型".to_string())?;
        let mut llm_failures: Vec<String> = Vec::new();
        for result in results.iter_mut() {
            if result.text.trim().is_empty() {
                continue;
            }
            let segment = match id_to_segment.get(&result.segment_id) {
                Some(s) => s,
                None => continue,
            };
            let role = segment.role.clone().unwrap_or_else(|| "assistant".into());
            match polish_text(
                &ollama_url,
                &model_name,
                &llm_prompt,
                &result.text,
                &role,
                segment.original_text.as_str(),
            ) {
                Ok(polished) => {
                    if !polished.text.trim().is_empty() {
                        result.text = polished.text;
                        result.polished = true;
                        result.emotion = polished.emotion;
                        result.tags = polished.tags;
                    }
                }
                Err(err) => {
                    // Per-segment graceful fallback: keep the Whisper text and
                    // record the failure. The frontend surfaces this as a toast
                    // so users know LLM polish didn't run, but the recognition
                    // step still landed.
                    eprintln!(
                        "[recognize] Ollama polish failed for segment {}: {}",
                        result.segment_id, err
                    );
                    llm_failures.push(format!("{}: {}", result.segment_id, err));
                }
            }
        }
        if !llm_failures.is_empty() && llm_failures.len() == results.len() {
            // Every single LLM call failed — that's a config error worth
            // surfacing as a hard error so the user fixes it.
            return Err(format!(
                "Ollama 全部失败（{} 段）。最后一段错误：{}",
                llm_failures.len(),
                llm_failures.last().cloned().unwrap_or_default()
            ));
        }
    }

    Ok(results)
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
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(180))
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
        if role != "user" && role != "assistant" {
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

fn normalize_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn infer_role(path: &Path) -> Option<String> {
    let value = path_to_string(path);
    if value.contains("陪聊") || value.to_ascii_lowercase().contains("user") {
        return Some("user".to_string());
    }
    if value.contains("发音人") || value.to_ascii_lowercase().contains("assistant") {
        return Some("assistant".to_string());
    }
    None
}

fn probe_audio(path: &Path) -> Result<AudioProbe, String> {
    let output = Command::new("ffprobe")
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
    let output = Command::new("ffmpeg")
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

    let output = Command::new("ffmpeg")
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
    let mut command = Command::new("ffmpeg");
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
    fn split_role_suffix_recognizes_known_markers() {
        assert_eq!(
            split_role_suffix("自由演绎_0001_02_发音人"),
            ("自由演绎_0001_02".to_string(), Some("发音人".to_string())),
        );
        assert_eq!(
            split_role_suffix("自由演绎_0001_02_陪聊"),
            ("自由演绎_0001_02".to_string(), Some("陪聊".to_string())),
        );
        assert_eq!(
            split_role_suffix("random_audio"),
            ("random_audio".to_string(), None),
        );
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
        let assistant = SegmentRecord {
            id: "x".into(),
            source_path: "/audio/自由演绎_0001_01_01_发音人.wav".into(),
            source_file_name: "自由演绎_0001_01_01_发音人.wav".into(),
            segment_path: "/segments/x.wav".into(),
            segment_file_name: "x.wav".into(),
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
            source_path: "/audio/free_001_01_01_发音人.wav".into(),
            source_file_name: "free_001_01_01_发音人.wav".into(),
            segment_path: "/segments/a_01.wav".into(),
            segment_file_name: "a_01.wav".into(),
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
            scan_project_folder,
            cut_audio_file,
            recognize_segments,
            polish_text_with_llm,
            list_ollama_models,
            check_dependencies,
            get_default_llm_prompt,
            save_project_file,
            load_project_file,
            export_segments_jsonl,
            export_dataset_bundle
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
