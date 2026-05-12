//! Cloud sync — the desktop side of the dispatcher.
//!
//! `CloudClient` is the typed HTTP wrapper (auth, claim, download,
//! heartbeat, upload, complete, fail). `CloudState` is the long-lived
//! per-app singleton that holds the current token + worker handle.
//! `worker_loop` is the headless background thread that polls /claim
//! and drives the existing cut / recognize / export pipeline.
//!
//! Everything is sync — Tauri 2's command runtime turns async-marked
//! commands into a spawn_blocking that doesn't buy us much given ureq
//! is already sync.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    cut_audio_file_impl, export_dataset_bundle_impl, recognize_segments_impl,
    scan_project_folder_impl, CutConfig, ExportOptions, RecognitionOptions, SegmentRecord,
};

const POLL_INTERVAL_SECONDS: u64 = 5;
const HEARTBEAT_INTERVAL_SECONDS: u64 = 60;
const HTTP_TIMEOUT_SECONDS: u64 = 60;
const UPLOAD_TIMEOUT_SECONDS: u64 = 30 * 60;
const DOWNLOAD_TIMEOUT_SECONDS: u64 = 30 * 60;

/// What the frontend tells us to use when the worker picks up a task.
/// Mirrors the relevant slice of RecognitionOptions plus a CutConfig.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerConfig {
    /// Whisper model (large-v3-turbo etc.).
    #[serde(default)]
    pub whisper_model: Option<String>,
    /// initial_prompt forwarded to whisper.
    #[serde(default)]
    pub whisper_initial_prompt: Option<String>,
    #[serde(default)]
    pub use_llm: Option<bool>,
    #[serde(default)]
    pub ollama_url: Option<String>,
    #[serde(default)]
    pub ollama_model: Option<String>,
    #[serde(default)]
    pub llm_prompt: Option<String>,
    #[serde(default)]
    pub llm_concurrency: Option<u32>,
    #[serde(default)]
    pub whisper_concurrency: Option<u32>,
    /// Per `WhisperEndpointDef` in lib.rs, but we re-shape here so cloud
    /// callers don't depend on the internal struct.
    #[serde(default)]
    pub whisper_endpoints: Vec<String>,
    #[serde(default)]
    pub ollama_extra_endpoints: Vec<String>,
    #[serde(default)]
    pub cut: Option<CutConfig>,
}

impl WorkerConfig {
    fn to_recognition_options(&self) -> RecognitionOptions {
        let whisper_endpoints = if self.whisper_endpoints.is_empty() {
            None
        } else {
            Some(
                self.whisper_endpoints
                    .iter()
                    .map(|url| crate::WhisperEndpointDef {
                        url: url.clone(),
                        enabled: true,
                    })
                    .collect(),
            )
        };
        let ollama_extra_endpoints = if self.ollama_extra_endpoints.is_empty() {
            None
        } else {
            Some(
                self.ollama_extra_endpoints
                    .iter()
                    .map(|url| crate::OllamaEndpointDef {
                        url: url.clone(),
                        model: None,
                        enabled: true,
                    })
                    .collect(),
            )
        };
        RecognitionOptions {
            whisper_model: self.whisper_model.clone(),
            use_llm: self.use_llm,
            ollama_url: self.ollama_url.clone(),
            ollama_extra_urls: None,
            ollama_extra_endpoints,
            ollama_model: self.ollama_model.clone(),
            llm_prompt: self.llm_prompt.clone(),
            initial_prompt: self.whisper_initial_prompt.clone(),
            use_cache: Some(true),
            overwrite_cache: Some(false),
            llm_concurrency: self.llm_concurrency,
            whisper_concurrency: self.whisper_concurrency,
            whisper_endpoints,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UserInfo {
    pub id: i64,
    pub email: String,
    pub role: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct LoginResponse {
    access_token: String,
    user: UserInfo,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct TaskClaim {
    pub(crate) id: String,
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) owner_id: i64,
    #[allow(dead_code)]
    pub(crate) input_size: Option<i64>,
    #[allow(dead_code)]
    pub(crate) claim_expires_at: String,
}

/// Snapshot returned to the frontend so the CloudPane can render
/// "logged in as / current task / worker on or off".
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CloudStatus {
    pub base_url: Option<String>,
    pub logged_in: bool,
    pub user: Option<UserInfo>,
    pub worker_enabled: bool,
    pub worker_running: bool,
    pub current_task: Option<CurrentTask>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CurrentTask {
    pub id: String,
    pub name: String,
    pub stage: String,
    pub started_at_unix_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("not configured — set the server URL first")]
    NotConfigured,
    #[error("not authenticated — log in first")]
    NotAuthenticated,
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("transport: {0}")]
    Transport(String),
    #[error("io: {0}")]
    Io(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("pipeline: {0}")]
    Pipeline(String),
}

impl From<ureq::Error> for CloudError {
    fn from(value: ureq::Error) -> Self {
        match value {
            ureq::Error::Status(status, response) => {
                let body = response
                    .into_string()
                    .unwrap_or_else(|_| "<unreadable>".to_string());
                CloudError::Http { status, body }
            }
            ureq::Error::Transport(t) => CloudError::Transport(t.to_string()),
        }
    }
}

impl From<std::io::Error> for CloudError {
    fn from(value: std::io::Error) -> Self {
        CloudError::Io(value.to_string())
    }
}

impl From<serde_json::Error> for CloudError {
    fn from(value: serde_json::Error) -> Self {
        CloudError::Decode(value.to_string())
    }
}

type Result<T> = std::result::Result<T, CloudError>;

/// Stateless HTTP wrapper. One per CloudState, but cheap to clone for
/// worker threads since the inner Agent is Arc'd by ureq.
#[derive(Clone)]
pub struct CloudClient {
    base_url: String,
    token: String,
    agent: ureq::Agent,
}

impl CloudClient {
    pub fn new(base_url: String, token: String) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECONDS))
            .build();
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            agent,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }

    #[allow(dead_code)]
    pub fn me(&self) -> Result<UserInfo> {
        let body: UserInfo = self
            .agent
            .get(&self.url("/api/auth/me"))
            .set("authorization", &self.auth_header())
            .call()?
            .into_json()?;
        Ok(body)
    }

    pub fn claim(&self) -> Result<Option<TaskClaim>> {
        let resp = self
            .agent
            .post(&self.url("/api/worker/claim"))
            .set("authorization", &self.auth_header())
            .set("content-length", "0")
            .call()?;
        if resp.status() == 204 {
            return Ok(None);
        }
        Ok(Some(resp.into_json()?))
    }

    pub fn heartbeat(&self, task_id: &str) -> Result<()> {
        self.agent
            .post(&self.url(&format!("/api/worker/tasks/{task_id}/heartbeat")))
            .set("authorization", &self.auth_header())
            .set("content-length", "0")
            .call()?;
        Ok(())
    }

    pub fn download_input(&self, task_id: &str, dest: &Path) -> Result<u64> {
        let url = self.url(&format!("/api/worker/tasks/{task_id}/input"));
        // Long timeout because audio inputs can be tens of MB to several GB.
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(DOWNLOAD_TIMEOUT_SECONDS))
            .build();
        let resp = agent
            .get(&url)
            .set("authorization", &self.auth_header())
            .call()?;
        let mut reader = resp.into_reader();
        let mut writer = fs::File::create(dest)?;
        let mut buf = [0u8; 64 * 1024];
        let mut total: u64 = 0;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n])?;
            total += n as u64;
        }
        writer.flush()?;
        Ok(total)
    }

    pub fn upload_output(&self, task_id: &str, src: &Path) -> Result<u64> {
        let url = self.url(&format!("/api/worker/tasks/{task_id}/output"));
        let bytes = fs::read(src)?;
        let total = bytes.len() as u64;
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(UPLOAD_TIMEOUT_SECONDS))
            .build();
        agent
            .put(&url)
            .set("authorization", &self.auth_header())
            .set("content-type", "application/zip")
            .send_bytes(&bytes)?;
        Ok(total)
    }

    pub fn complete(&self, task_id: &str, summary: &Value) -> Result<()> {
        self.agent
            .post(&self.url(&format!("/api/worker/tasks/{task_id}/complete")))
            .set("authorization", &self.auth_header())
            .send_json(json!({ "summary": summary }))?;
        Ok(())
    }

    pub fn fail(&self, task_id: &str, error: &str) -> Result<()> {
        // Cap error string at the dispatcher's TaskFailIn limit.
        let trimmed: String = error.chars().take(3900).collect();
        self.agent
            .post(&self.url(&format!("/api/worker/tasks/{task_id}/fail")))
            .set("authorization", &self.auth_header())
            .send_json(json!({ "error": trimmed }))?;
        Ok(())
    }
}

/// Stand-alone login helper — doesn't need an existing token, just the
/// server URL. Used by the cloud_login command.
pub fn login_request(base_url: &str, email: &str, password: &str) -> Result<(String, UserInfo)> {
    let base = base_url.trim_end_matches('/');
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECONDS))
        .build();
    let resp: LoginResponse = agent
        .post(&format!("{base}/api/auth/login"))
        .send_json(json!({ "email": email, "password": password }))?
        .into_json()?;
    Ok((resp.access_token, resp.user))
}

// ============================================================
// CloudState
// ============================================================

#[derive(Default)]
struct CloudInner {
    base_url: Option<String>,
    token: Option<String>,
    user: Option<UserInfo>,
    worker_config: Option<WorkerConfig>,
    worker_enabled: bool,
    worker_handle: Option<JoinHandle<()>>,
    cancel: Option<Arc<AtomicBool>>,
    current_task: Option<CurrentTask>,
    last_error: Option<String>,
}

/// Single shared state object hung off the Tauri AppHandle.
pub struct CloudState {
    inner: Mutex<CloudInner>,
}

impl CloudState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(CloudInner::default()),
        }
    }

    pub fn set_credentials(&self, base_url: String, token: String, user: UserInfo) {
        let mut guard = self.inner.lock().unwrap();
        guard.base_url = Some(base_url);
        guard.token = Some(token);
        guard.user = Some(user);
    }

    pub fn clear_credentials(&self) {
        let mut guard = self.inner.lock().unwrap();
        guard.token = None;
        guard.user = None;
        guard.worker_enabled = false;
        if let Some(cancel) = &guard.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    #[allow(dead_code)]
    pub fn set_base_url(&self, base_url: String) {
        self.inner.lock().unwrap().base_url = Some(base_url);
    }

    pub fn set_worker_config(&self, config: WorkerConfig) {
        self.inner.lock().unwrap().worker_config = Some(config);
    }

    fn client(&self) -> Result<CloudClient> {
        let guard = self.inner.lock().unwrap();
        let base_url = guard.base_url.clone().ok_or(CloudError::NotConfigured)?;
        let token = guard.token.clone().ok_or(CloudError::NotAuthenticated)?;
        Ok(CloudClient::new(base_url, token))
    }

    pub fn status(&self) -> CloudStatus {
        let guard = self.inner.lock().unwrap();
        let worker_running = guard
            .worker_handle
            .as_ref()
            .map(|handle| !handle.is_finished())
            .unwrap_or(false);
        CloudStatus {
            base_url: guard.base_url.clone(),
            logged_in: guard.token.is_some(),
            user: guard.user.clone(),
            worker_enabled: guard.worker_enabled,
            worker_running,
            current_task: guard.current_task.clone(),
            last_error: guard.last_error.clone(),
        }
    }

    pub fn set_current_task(&self, task: Option<CurrentTask>) {
        self.inner.lock().unwrap().current_task = task;
    }

    pub fn set_last_error(&self, err: Option<String>) {
        self.inner.lock().unwrap().last_error = err;
    }

    pub fn set_worker_enabled(&self, app: AppHandle, enabled: bool) -> Result<()> {
        let need_spawn = {
            let mut guard = self.inner.lock().unwrap();
            guard.worker_enabled = enabled;
            if !enabled {
                if let Some(cancel) = &guard.cancel {
                    cancel.store(true, Ordering::Relaxed);
                }
                return Ok(());
            }
            // Already running?
            let alive = guard
                .worker_handle
                .as_ref()
                .map(|h| !h.is_finished())
                .unwrap_or(false);
            if alive {
                return Ok(());
            }
            // Must have credentials before we can spawn.
            if guard.base_url.is_none() || guard.token.is_none() {
                return Err(CloudError::NotAuthenticated);
            }
            let cancel = Arc::new(AtomicBool::new(false));
            guard.cancel = Some(cancel.clone());
            Some(cancel)
        };
        if let Some(cancel) = need_spawn {
            let app_for_thread = app.clone();
            let handle = thread::spawn(move || worker_loop(app_for_thread, cancel));
            self.inner.lock().unwrap().worker_handle = Some(handle);
        }
        // Notify the frontend so the UI refreshes its toggle state.
        let _ = app.emit("cloud://status-changed", ());
        Ok(())
    }
}

// ============================================================
// Worker loop
// ============================================================

fn worker_loop(app: AppHandle, cancel: Arc<AtomicBool>) {
    log::info("worker_loop started");
    while !cancel.load(Ordering::Relaxed) {
        let state = match app.try_state::<CloudState>() {
            Some(state) => state,
            None => {
                log::warn("CloudState missing; bailing worker_loop");
                return;
            }
        };
        let client = match state.client() {
            Ok(c) => c,
            Err(err) => {
                state.set_last_error(Some(err.to_string()));
                sleep_with_cancel(&cancel, POLL_INTERVAL_SECONDS);
                continue;
            }
        };

        match client.claim() {
            Ok(None) => {
                // Empty queue is the steady state. Clear any error
                // banner from a previous transient failure so the UI
                // doesn't keep showing stale text indefinitely.
                state.set_last_error(None);
                let _ = app.emit("cloud://status-changed", ());
                sleep_with_cancel(&cancel, POLL_INTERVAL_SECONDS);
            }
            Ok(Some(claim)) => {
                // Reaching this point means the dispatcher is healthy
                // and the token is valid — clear any prior error.
                state.set_last_error(None);
                let task_id = claim.id.clone();
                let task_name = claim.name.clone();
                state.set_current_task(Some(CurrentTask {
                    id: task_id.clone(),
                    name: task_name.clone(),
                    stage: "downloading".to_string(),
                    started_at_unix_ms: unix_ms(),
                }));
                let _ = app.emit("cloud://status-changed", ());
                let outcome = run_task(&app, &client, &claim);
                state.set_current_task(None);
                let _ = app.emit("cloud://status-changed", ());
                match outcome {
                    Ok(()) => {
                        state.set_last_error(None);
                        log::info(&format!("task {task_id} ({task_name}) completed"));
                    }
                    Err(err) => {
                        let msg = err.to_string();
                        log::warn(&format!("task {task_id} failed: {msg}"));
                        state.set_last_error(Some(msg.clone()));
                        let _ = client.fail(&task_id, &msg);
                    }
                }
            }
            Err(err) => {
                state.set_last_error(Some(err.to_string()));
                let _ = app.emit("cloud://status-changed", ());
                sleep_with_cancel(&cancel, POLL_INTERVAL_SECONDS * 2);
            }
        }
    }
    log::info("worker_loop exited");
}

fn sleep_with_cancel(cancel: &AtomicBool, seconds: u64) {
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn run_task(app: &AppHandle, client: &CloudClient, claim: &TaskClaim) -> Result<()> {
    let task_id = claim.id.clone();
    let state = app
        .try_state::<CloudState>()
        .ok_or_else(|| CloudError::Pipeline("CloudState missing".into()))?;

    let workdir = tempfile::tempdir()
        .map_err(|err| CloudError::Pipeline(format!("tempdir: {err}")))?;
    let input_zip = workdir.path().join("input.zip");
    let extract_dir = workdir.path().join("extract");
    fs::create_dir_all(&extract_dir)?;
    let bundle_dir = workdir.path().join("bundle");
    let output_zip = workdir.path().join("output.zip");

    // ---- 1. download ----
    state.set_current_task(Some(CurrentTask {
        id: task_id.clone(),
        name: claim.name.clone(),
        stage: "downloading".to_string(),
        started_at_unix_ms: unix_ms(),
    }));
    let _ = app.emit("cloud://status-changed", ());
    client.download_input(&task_id, &input_zip)?;

    // Spawn a heartbeat thread for the duration of processing. The
    // dispatcher's default lease is 5 minutes; we extend every 60 s.
    let hb_cancel = Arc::new(AtomicBool::new(false));
    let hb_handle = spawn_heartbeat(client.clone(), task_id.clone(), hb_cancel.clone());

    let pipeline_result =
        run_pipeline(app, claim, &input_zip, &extract_dir, &bundle_dir, &output_zip);

    hb_cancel.store(true, Ordering::Relaxed);
    let _ = hb_handle.join();

    let (summary, output_bytes) = match pipeline_result {
        Ok(value) => value,
        Err(err) => return Err(err),
    };

    // ---- 6. upload ----
    state.set_current_task(Some(CurrentTask {
        id: task_id.clone(),
        name: claim.name.clone(),
        stage: "uploading".to_string(),
        started_at_unix_ms: unix_ms(),
    }));
    let _ = app.emit("cloud://status-changed", ());
    client.upload_output(&task_id, &output_zip)?;

    // ---- 7. complete ----
    let mut full_summary = summary;
    if let Some(obj) = full_summary.as_object_mut() {
        obj.insert("output_size".to_string(), json!(output_bytes));
    }
    client.complete(&task_id, &full_summary)?;
    Ok(())
}

fn run_pipeline(
    app: &AppHandle,
    claim: &TaskClaim,
    input_zip: &Path,
    extract_dir: &Path,
    bundle_dir: &Path,
    output_zip: &Path,
) -> Result<(Value, u64)> {
    let started = Instant::now();
    let state = app
        .try_state::<CloudState>()
        .ok_or_else(|| CloudError::Pipeline("CloudState missing".into()))?;
    let worker_config = state
        .inner
        .lock()
        .unwrap()
        .worker_config
        .clone()
        .unwrap_or_default();

    // ---- 2. unzip ----
    state.set_current_task(Some(CurrentTask {
        id: claim.id.clone(),
        name: claim.name.clone(),
        stage: "extracting".to_string(),
        started_at_unix_ms: unix_ms(),
    }));
    let _ = app.emit("cloud://status-changed", ());
    unzip_into(input_zip, extract_dir).map_err(CloudError::Pipeline)?;

    // ---- 3. scan + cut ----
    state.set_current_task(Some(CurrentTask {
        id: claim.id.clone(),
        name: claim.name.clone(),
        stage: "cutting".to_string(),
        started_at_unix_ms: unix_ms(),
    }));
    let _ = app.emit("cloud://status-changed", ());
    let scan = scan_project_folder_impl(
        extract_dir.to_string_lossy().to_string(),
        None,
        None,
    )
    .map_err(CloudError::Pipeline)?;
    if scan.audio_files.is_empty() {
        return Err(CloudError::Pipeline(
            "input zip contained no audio files".into(),
        ));
    }

    let cut_config = worker_config.cut.clone().unwrap_or(CutConfig {
        silence_db: -30.0,
        min_silence_ms: 400,
        min_segment_ms: 300,
        pre_roll_ms: 100,
        post_roll_ms: 200,
        max_segment_ms: 0,
    });

    let mut all_segments: Vec<SegmentRecord> = Vec::new();
    for audio in &scan.audio_files {
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
    }

    if all_segments.is_empty() {
        return Err(CloudError::Pipeline(
            "切分后无可识别的片段（可能整段静音）".into(),
        ));
    }

    // ---- 4. recognize ----
    state.set_current_task(Some(CurrentTask {
        id: claim.id.clone(),
        name: claim.name.clone(),
        stage: "recognizing".to_string(),
        started_at_unix_ms: unix_ms(),
    }));
    let _ = app.emit("cloud://status-changed", ());
    let recognition = recognize_segments_impl(
        app.clone(),
        scan.project_dir.clone(),
        all_segments.clone(),
        worker_config.to_recognition_options(),
    )
    .map_err(CloudError::Pipeline)?;
    apply_recognition(&mut all_segments, &recognition);

    // ---- 5. bundle + zip ----
    state.set_current_task(Some(CurrentTask {
        id: claim.id.clone(),
        name: claim.name.clone(),
        stage: "bundling".to_string(),
        started_at_unix_ms: unix_ms(),
    }));
    let _ = app.emit("cloud://status-changed", ());
    fs::create_dir_all(bundle_dir)?;
    export_dataset_bundle_impl(
        bundle_dir.to_string_lossy().to_string(),
        all_segments.clone(),
        ExportOptions {
            system_prompt: None,
            pair_user_assistant: Some(true),
            use_source_audio_for_user: Some(false),
            audio_file_prefix: None,
            input_root: Some(extract_dir.to_string_lossy().to_string()),
        },
        true,
    )
    .map_err(CloudError::Pipeline)?;

    zip_dir(bundle_dir, output_zip).map_err(CloudError::Pipeline)?;
    let output_bytes = fs::metadata(output_zip).map(|m| m.len()).unwrap_or(0);

    // Build the summary the dispatcher keeps after files are cleaned.
    let mut duration_by_role: serde_json::Map<String, Value> = Default::default();
    for seg in &all_segments {
        let key = seg.role.clone().unwrap_or_else(|| "unknown".to_string());
        let entry = duration_by_role
            .entry(key)
            .or_insert_with(|| json!(0u64));
        if let Some(current) = entry.as_u64() {
            *entry = json!(current + seg.duration_ms);
        }
    }
    let summary = json!({
        "segment_count": all_segments.len(),
        "duration_by_role": duration_by_role,
        "source_files": scan.audio_files.len(),
        "asr_engine": worker_config
            .whisper_model
            .clone()
            .unwrap_or_else(|| "large-v3-turbo".to_string()),
        "llm_endpoints": worker_config.ollama_extra_endpoints,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    });

    Ok((summary, output_bytes))
}

fn spawn_heartbeat(
    client: CloudClient,
    task_id: String,
    cancel: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while !cancel.load(Ordering::Relaxed) {
            sleep_with_cancel(&cancel, HEARTBEAT_INTERVAL_SECONDS);
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if let Err(err) = client.heartbeat(&task_id) {
                log::warn(&format!("heartbeat {task_id} failed: {err}"));
            }
        }
    })
}

fn apply_recognition(segments: &mut [SegmentRecord], results: &[crate::RecognitionResult]) {
    use std::collections::HashMap;
    let by_id: HashMap<&str, &crate::RecognitionResult> =
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
// Zip helpers
// ============================================================

fn unzip_into(zip_path: &Path, dest: &Path) -> std::result::Result<(), String> {
    let file = fs::File::open(zip_path).map_err(|e| format!("open zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("read zip: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("zip entry {i}: {e}"))?;
        let rel = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => continue, // skip suspicious entries (path traversal)
        };
        let out_path = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out_path).map_err(|e| format!("mkdir: {e}"))?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("mkdir parent: {e}"))?;
            }
            let mut out_file =
                fs::File::create(&out_path).map_err(|e| format!("create: {e}"))?;
            std::io::copy(&mut entry, &mut out_file)
                .map_err(|e| format!("extract: {e}"))?;
        }
    }
    Ok(())
}

fn zip_dir(src: &Path, dest_zip: &Path) -> std::result::Result<(), String> {
    let file = fs::File::create(dest_zip).map_err(|e| format!("create zip: {e}"))?;
    let mut writer = zip::ZipWriter::new(file);
    let options: zip::write::FileOptions<()> = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);

    for entry in walkdir::WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        let rel = match path.strip_prefix(src) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if entry.file_type().is_dir() {
            writer
                .add_directory(&rel_str, options)
                .map_err(|e| format!("zip mkdir: {e}"))?;
        } else if entry.file_type().is_file() {
            writer
                .start_file(&rel_str, options)
                .map_err(|e| format!("zip start: {e}"))?;
            let mut f = fs::File::open(path).map_err(|e| format!("open: {e}"))?;
            std::io::copy(&mut f, &mut writer)
                .map_err(|e| format!("copy: {e}"))?;
        }
    }
    writer.finish().map_err(|e| format!("zip finish: {e}"))?;
    Ok(())
}

fn unix_ms() -> u64 {
    UNIX_EPOCH
        .elapsed()
        .ok()
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ============================================================
// Tiny eprintln-based logging — keeps cloud.rs free of crate-level
// log/env_logger boilerplate while still leaving a forensic trail in
// the Tauri console.
// ============================================================
mod log {
    pub fn info(msg: &str) {
        eprintln!("[cloud INFO ] {msg}");
    }
    pub fn warn(msg: &str) {
        eprintln!("[cloud WARN ] {msg}");
    }
}
