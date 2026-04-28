use regex::Regex;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PROJECT_FOLDER: &str = "_dialect_labeler";

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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CutConfig {
    silence_db: f32,
    min_silence_ms: u64,
    min_segment_ms: u64,
    pre_roll_ms: u64,
    post_roll_ms: u64,
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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecognitionResult {
    segment_id: String,
    text: String,
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
    if let Some(path) = manifest_path.filter(|value| !value.trim().is_empty()) {
        let manifest = parse_manifest_file(Path::new(&path))?;
        manifest_records.extend(manifest);
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

    Ok(ProjectScan {
        root_path: path_to_string(&root),
        project_dir: path_to_string(&project_dir),
        segments_dir: path_to_string(&segments_dir),
        audio_files,
        manifest_records,
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
    let source_dir = segments_root.join(&safe_source);
    fs::create_dir_all(&source_dir).map_err(|err| err.to_string())?;

    let codec = output_pcm_codec(&probe);
    let mut segments = Vec::new();
    let text = original_text.clone().unwrap_or_default();
    let text_chunks = split_text_by_ranges(&text, &ranges);

    for (index, (start_sec, end_sec)) in ranges.iter().enumerate() {
        let start_ms = seconds_to_ms(*start_sec);
        let end_ms = seconds_to_ms(*end_sec);
        let segment_file_name = format!(
            "{}_{:04}_{}-{}.wav",
            safe_source,
            index + 1,
            start_ms,
            end_ms
        );
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

#[tauri::command]
fn export_segments_jsonl(
    project_dir: String,
    output_path: Option<String>,
    segments: Vec<SegmentRecord>,
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
    let mut lines = Vec::with_capacity(segments.len());

    for segment in segments {
        let item = serde_json::json!({
            "role": segment.role.unwrap_or_else(|| "unknown".to_string()),
            "content": [segment.phonetic_text],
            "audio_file": segment.segment_path,
            "emotion": segment.emotion,
            "tags": segment.tags,
            "notes": segment.notes,
            "source_audio_file": segment.source_path,
            "start_ms": segment.start_ms,
            "end_ms": segment.end_ms,
            "duration_ms": segment.duration_ms
        });
        lines.push(serde_json::to_string(&item).map_err(|err| err.to_string())?);
    }

    fs::write(&path, format!("{}\n", lines.join("\n"))).map_err(|err| err.to_string())?;
    Ok(path_to_string(&path))
}

#[tauri::command]
async fn recognize_segments(
    project_dir: String,
    segments: Vec<SegmentRecord>,
    model: Option<String>,
) -> Result<Vec<RecognitionResult>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        recognize_segments_impl(project_dir, segments, model)
    })
    .await
    .map_err(|err| err.to_string())?
}

fn recognize_segments_impl(
    project_dir: String,
    segments: Vec<SegmentRecord>,
    model: Option<String>,
) -> Result<Vec<RecognitionResult>, String> {
    if segments.is_empty() {
        return Ok(Vec::new());
    }

    ensure_ffmpeg()?;
    let whisper = find_whisper_command()?;
    let project_dir = PathBuf::from(project_dir);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default();
    let output_dir = project_dir.join(".asr").join(format!("run_{}", stamp));
    fs::create_dir_all(&output_dir).map_err(|err| err.to_string())?;

    let model = model
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "base".to_string());

    let mut command = Command::new(whisper);
    command
        .arg("--model")
        .arg(model)
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
        .arg("以下是中文方言口语转写，请用中文汉字记录听到的字音，不要翻译。");

    for segment in &segments {
        let path = PathBuf::from(&segment.segment_path);
        if !path.is_file() {
            return Err(format!("切割片段不存在：{}", segment.segment_path));
        }
        command.arg(path);
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

    let mut results = Vec::with_capacity(segments.len());
    for segment in segments {
        let path = PathBuf::from(&segment.segment_path);
        let text = read_whisper_json_text(&output_dir, &path)?;
        results.push(RecognitionResult {
            segment_id: segment.id,
            text,
        });
    }
    Ok(results)
}

fn find_whisper_command() -> Result<&'static str, String> {
    for command in ["whisper", "whisper.exe"] {
        if Command::new(command).arg("--help").output().is_ok() {
            return Ok(command);
        }
    }
    Err("未找到本地 whisper 命令；请先安装 openai-whisper 或配置本地识别工具".to_string())
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
    fn prepares_playback_data_url_for_24_bit_wav() {
        if ensure_ffmpeg().is_err() {
            return;
        }

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be valid")
            .as_millis();
        let root = std::env::temp_dir().join(format!("dialect_labeler_playback_{}", stamp));
        fs::create_dir_all(&root).expect("test temp dir should be created");
        let input = root.join("sample24.wav");

        let status = Command::new("ffmpeg")
            .arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg("sine=frequency=440:duration=0.2:sample_rate=48000")
            .arg("-c:a")
            .arg("pcm_s24le")
            .arg(&input)
            .status()
            .expect("ffmpeg should run");
        assert!(status.success());

        let playback = prepare_playback_audio_impl(path_to_string(&input), path_to_string(&root))
            .expect("playback preview should be generated");
        assert!(playback.is_preview);
        assert!(Path::new(&playback.path).is_file());

        let probe = probe_audio(Path::new(&playback.path)).expect("preview probes");
        assert_eq!(probe.codec_name.as_deref(), Some("pcm_s16le"));

        fs::remove_dir_all(root).ok();
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
            play_audio,
            pause_audio,
            stop_audio,
            audio_state,
            scan_project_folder,
            cut_audio_file,
            recognize_segments,
            save_project_file,
            load_project_file,
            export_segments_jsonl
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
