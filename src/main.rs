use std::{
    collections::VecDeque,
    env,
    io::{Read, Write},
    net::SocketAddr,
    path::PathBuf,
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex as StdMutex},
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{Mutex, broadcast},
};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const AUTH_COOKIE: &str = "euthergate_session";
const PROXY_AUTH_HEADER: &str = "x-euthergate-proxy-token";
const HISTORY_LIMIT: usize = 256 * 1024;

#[derive(Clone)]
struct AppState {
    token_hash: [u8; 32],
    auth_session: Arc<str>,
    secure_cookie: bool,
    proxy_token_hash: Option<[u8; 32]>,
    terminal: Arc<TerminalSession>,
    desktop: Arc<DesktopManager>,
}

struct TerminalSession {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    _child: StdMutex<Box<dyn portable_pty::Child + Send + Sync>>,
    output: Arc<OutputRelay>,
}

struct DesktopManager {
    transition: Mutex<()>,
    active: AtomicBool,
    viewer_connected: AtomicBool,
    headless_output: String,
    headless_mode: String,
    selection: StdMutex<DesktopSelection>,
    helper: PathBuf,
}

#[derive(Clone)]
struct DesktopSelection {
    output: String,
    mode: String,
    workspace: u32,
    virtual_output: bool,
}

#[derive(Deserialize)]
struct LoginRequest {
    token: String,
}

#[derive(Serialize)]
struct StatusResponse {
    authenticated: bool,
    terminal_ready: bool,
    auth_mode: &'static str,
}

#[derive(Serialize)]
struct DesktopStatusResponse {
    available: bool,
    active: bool,
    viewer_connected: bool,
    output: String,
    mode: String,
    workspace: u32,
    transport: &'static str,
    input: &'static str,
    virtual_output: bool,
    outputs: Vec<DesktopOutput>,
}

#[derive(Clone, Serialize)]
struct DesktopOutput {
    name: String,
    description: String,
    mode: String,
    workspace: u32,
    virtual_output: bool,
}

#[derive(Deserialize)]
struct DesktopStartQuery {
    output: Option<String>,
}

#[derive(Deserialize)]
struct HyprMonitor {
    name: String,
    #[serde(default)]
    description: String,
    width: u32,
    height: u32,
    #[serde(rename = "refreshRate")]
    refresh_rate: f64,
    #[serde(rename = "activeWorkspace")]
    active_workspace: HyprWorkspace,
}

#[derive(Deserialize)]
struct HyprWorkspace {
    id: u32,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientControl {
    Resize { cols: u16, rows: u16 },
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let config = Config::load()?;

    if let Some(token) = &config.generated_token {
        warn!("EUTHERGATE_TOKEN was not set; generated a temporary development token");
        println!("\n  EutherGate development token: {token}\n");
    }

    let terminal = Arc::new(TerminalSession::spawn(&config.shell, &config.workdir)?);
    let state = AppState {
        token_hash: hash_token(&config.token),
        auth_session: Arc::from(random_secret(32)),
        secure_cookie: config.secure_cookie,
        proxy_token_hash: config.proxy_token.as_deref().map(hash_token),
        terminal,
        desktop: Arc::new(DesktopManager {
            transition: Mutex::new(()),
            active: AtomicBool::new(false),
            viewer_connected: AtomicBool::new(false),
            headless_output: config.desktop_output.clone(),
            headless_mode: config.desktop_mode.clone(),
            selection: StdMutex::new(DesktopSelection {
                output: config.desktop_output.clone(),
                mode: config.desktop_mode.clone(),
                workspace: 0,
                virtual_output: true,
            }),
            helper: config.desktop_helper.clone(),
        }),
    };

    let static_files = ServeDir::new(&config.web_root)
        .fallback(ServeFile::new(config.web_root.join("index.html")));
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/status", get(status))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/desktop/status", get(desktop_status))
        .route("/api/desktop/start", post(desktop_start))
        .route("/api/desktop/stop", post(desktop_stop))
        .route(
            "/api/desktop/launch-terminal",
            post(desktop_launch_terminal),
        )
        .route("/ws/terminal", get(terminal_ws))
        .route("/ws/desktop", get(desktop_ws))
        .fallback_service(static_files)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("could not bind EutherGate to {}", config.bind))?;
    info!(address = %config.bind, workdir = %config.workdir.display(), "EutherGate is ready");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("gateway server stopped unexpectedly")?;
    Ok(())
}

struct Config {
    bind: SocketAddr,
    token: String,
    generated_token: Option<String>,
    secure_cookie: bool,
    proxy_token: Option<String>,
    shell: PathBuf,
    workdir: PathBuf,
    web_root: PathBuf,
    desktop_output: String,
    desktop_mode: String,
    desktop_helper: PathBuf,
}

impl Config {
    fn load() -> Result<Self> {
        let configured_token = env::var("EUTHERGATE_TOKEN")
            .ok()
            .filter(|value| !value.is_empty());
        let generated_token = configured_token.is_none().then(|| random_secret(24));
        let token = configured_token
            .or_else(|| generated_token.clone())
            .expect("token exists");
        let bind = env::var("EUTHERGATE_BIND")
            .unwrap_or_else(|_| "127.0.0.1:8787".into())
            .parse()
            .context("EUTHERGATE_BIND must be an IP address and port")?;
        let secure_cookie = env_bool("EUTHERGATE_SECURE_COOKIE", false)?;
        let proxy_token = env::var("EUTHERGATE_PROXY_TOKEN")
            .ok()
            .filter(|value| !value.is_empty());
        let shell = env::var_os("EUTHERGATE_SHELL")
            .or_else(|| env::var_os("SHELL"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        let workdir = env::var_os("EUTHERGATE_WORKDIR")
            .map(PathBuf::from)
            .unwrap_or(env::current_dir().context("could not determine current directory")?);
        let web_root = env::var_os("EUTHERGATE_WEB_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("web/dist"));
        let desktop_output =
            env::var("EUTHERGATE_DESKTOP_OUTPUT").unwrap_or_else(|_| "EUTHERGATE-1".into());
        let desktop_mode =
            env::var("EUTHERGATE_DESKTOP_MODE").unwrap_or_else(|_| "1280x720@30".into());
        let desktop_helper = env::var_os("EUTHERGATE_DESKTOP_HELPER")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("scripts/webrtc_desktop.py"));

        Ok(Self {
            bind,
            token,
            generated_token,
            secure_cookie,
            proxy_token,
            shell,
            workdir,
            web_root,
            desktop_output,
            desktop_mode,
            desktop_helper,
        })
    }
}

impl TerminalSession {
    fn spawn(shell: &PathBuf, workdir: &PathBuf) -> Result<Self> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows: 30,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("could not create terminal PTY")?;
        let mut command = CommandBuilder::new(shell);
        command.cwd(workdir);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        let child = pair
            .slave
            .spawn_command(command)
            .context("could not start login shell")?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .context("could not clone PTY reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("could not open PTY writer")?;
        let (output_tx, _) = broadcast::channel(256);
        let output = Arc::new(OutputRelay {
            tx: output_tx,
            history: StdMutex::new(VecDeque::with_capacity(HISTORY_LIMIT)),
            gate: StdMutex::new(()),
        });
        let session = Self {
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            _child: StdMutex::new(child),
            output: output.clone(),
        };

        std::thread::Builder::new()
            .name("euthergate-pty-reader".into())
            .spawn(move || read_pty(&mut reader, &output))
            .context("could not start PTY reader thread")?;

        Ok(session)
    }

    fn replay_and_subscribe(&self) -> (Vec<u8>, broadcast::Receiver<Vec<u8>>) {
        let _gate = self.output.gate.lock().expect("output gate poisoned");
        let replay = self
            .output
            .history
            .lock()
            .expect("history poisoned")
            .iter()
            .copied()
            .collect();
        let receiver = self.output.tx.subscribe();
        (replay, receiver)
    }

    async fn write(&self, bytes: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().await;
        writer.write_all(bytes).context("could not write to PTY")?;
        writer.flush().context("could not flush PTY input")?;
        Ok(())
    }

    async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let cols = cols.clamp(20, 500);
        let rows = rows.clamp(5, 300);
        self.master
            .lock()
            .await
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("could not resize PTY")
    }
}

struct OutputRelay {
    tx: broadcast::Sender<Vec<u8>>,
    history: StdMutex<VecDeque<u8>>,
    gate: StdMutex<()>,
}

fn read_pty(reader: &mut Box<dyn Read + Send>, relay: &OutputRelay) {
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let chunk = buffer[..count].to_vec();
                let _gate = relay.gate.lock().expect("output gate poisoned");
                let mut history = relay.history.lock().expect("history poisoned");
                history.extend(chunk.iter().copied());
                while history.len() > HISTORY_LIMIT {
                    history.pop_front();
                }
                drop(history);
                let _ = relay.tx.send(chunk);
            }
            Err(error) => {
                error!(%error, "PTY reader stopped");
                break;
            }
        }
    }
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn status(State(state): State<AppState>, headers: HeaderMap) -> Json<StatusResponse> {
    let auth_mode = authentication_mode(&headers, &state);
    Json(StatusResponse {
        authenticated: auth_mode != "none",
        terminal_ready: true,
        auth_mode,
    })
}

async fn login(State(state): State<AppState>, Json(request): Json<LoginRequest>) -> Response {
    if !constant_time_eq(&hash_token(&request.token), &state.token_hash) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid_token" })),
        )
            .into_response();
    }

    let secure = if state.secure_cookie { "; Secure" } else { "" };
    let cookie = format!(
        "{AUTH_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=43200{secure}",
        state.auth_session
    );
    let mut response = Json(serde_json::json!({ "authenticated": true })).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("valid cookie"),
    );
    response
}

async fn logout() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "euthergate_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
        ),
    );
    response
}

async fn terminal_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| terminal_socket(socket, state.terminal))
        .into_response()
}

async fn terminal_socket(socket: WebSocket, terminal: Arc<TerminalSession>) {
    let (mut sender, mut receiver) = socket.split();
    let (replay, mut output) = terminal.replay_and_subscribe();
    if !replay.is_empty() && sender.send(Message::Binary(replay.into())).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            outgoing = output.recv() => match outgoing {
                Ok(chunk) => {
                    if sender.send(Message::Binary(chunk.into())).await.is_err() { break; }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "terminal viewer lagged behind output");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = receiver.next() => match incoming {
                Some(Ok(Message::Binary(bytes))) => {
                    if let Err(error) = terminal.write(&bytes).await {
                        error!(%error, "terminal input failed");
                        break;
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ClientControl>(&text) {
                        Ok(ClientControl::Resize { cols, rows }) => {
                            if let Err(error) = terminal.resize(cols, rows).await {
                                warn!(%error, "terminal resize failed");
                            }
                        }
                        Err(error) => warn!(%error, "ignored malformed terminal control message"),
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    warn!(%error, "terminal WebSocket failed");
                    break;
                }
            }
        }
    }
}

async fn desktop_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let desktop = &state.desktop;
    let outputs = match desktop.outputs().await {
        Ok(outputs) => outputs,
        Err(error) => {
            warn!(%error, "could not enumerate desktop outputs");
            Vec::new()
        }
    };
    let selection = desktop.selection();
    Json(DesktopStatusResponse {
        available: desktop.helper.is_file(),
        active: desktop.active.load(Ordering::Acquire),
        viewer_connected: desktop.viewer_connected.load(Ordering::Acquire),
        output: selection.output,
        mode: selection.mode,
        workspace: selection.workspace,
        transport: "WebRTC/VP8",
        input: "WebRTC DataChannel",
        virtual_output: selection.virtual_output,
        outputs,
    })
    .into_response()
}

async fn desktop_start(
    State(state): State<AppState>,
    Query(query): Query<DesktopStartQuery>,
    headers: HeaderMap,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.desktop.start(query.output.as_deref()).await {
        Ok(selection) => Json(serde_json::json!({
            "active": true,
            "output": selection.output,
            "workspace": selection.workspace,
        }))
        .into_response(),
        Err(error) => {
            error!(%error, "could not start virtual desktop");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

async fn desktop_launch_terminal(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.desktop.launch_terminal().await {
        Ok(workspace) => Json(serde_json::json!({ "workspace": workspace })).into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn desktop_stop(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.desktop.stop().await {
        Ok(()) => Json(serde_json::json!({ "active": false })).into_response(),
        Err(error) => {
            error!(%error, "could not stop virtual desktop");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

async fn desktop_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !state.desktop.active.load(Ordering::Acquire) {
        return (StatusCode::CONFLICT, "desktop capture is not active").into_response();
    }
    if state
        .desktop
        .viewer_connected
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return (
            StatusCode::CONFLICT,
            "a desktop viewer is already connected",
        )
            .into_response();
    }
    let desktop = state.desktop.clone();
    ws.on_upgrade(move |socket| async move {
        if let Err(error) = desktop_socket(socket, desktop.clone()).await {
            error!(%error, "desktop WebRTC bridge stopped");
        }
        desktop.viewer_connected.store(false, Ordering::Release);
    })
    .into_response()
}

impl DesktopManager {
    fn selection(&self) -> DesktopSelection {
        self.selection
            .lock()
            .expect("desktop selection poisoned")
            .clone()
    }

    async fn outputs(&self) -> Result<Vec<DesktopOutput>> {
        let monitors = hyprctl(&["monitors", "all", "-j"]).await?;
        let mut outputs = parse_outputs(&monitors, &self.headless_output)?;
        if !outputs
            .iter()
            .any(|output| output.name == self.headless_output)
        {
            outputs.insert(
                0,
                DesktopOutput {
                    name: self.headless_output.clone(),
                    description: "Virtual Forge output".into(),
                    mode: self.headless_mode.clone(),
                    workspace: 0,
                    virtual_output: true,
                },
            );
        }
        Ok(outputs)
    }

    async fn start(&self, requested_output: Option<&str>) -> Result<DesktopSelection> {
        let _transition = self.transition.lock().await;
        if !self.helper.is_file() {
            anyhow::bail!("WebRTC helper not found at {}", self.helper.display());
        }

        let requested = requested_output.unwrap_or(&self.headless_output);
        let previous = self.selection();
        if self.viewer_connected.load(Ordering::Acquire) && requested != previous.output {
            anyhow::bail!("disconnect the current viewer before switching output");
        }

        let monitors = hyprctl(&["monitors", "all", "-j"]).await?;
        let existing = parse_outputs(&monitors, &self.headless_output)?;
        if requested == self.headless_output {
            if !existing.iter().any(|output| output.name == requested) {
                hyprctl(&["output", "create", "headless", &self.headless_output]).await?;
            }
            if let Err(error) = hyprctl(&[
                "keyword",
                "monitor",
                &format!("{},{},auto,1", self.headless_output, self.headless_mode),
            ])
            .await
            {
                let _ = hyprctl(&["output", "remove", &self.headless_output]).await;
                return Err(error);
            }
        } else if !existing.iter().any(|output| output.name == requested) {
            anyhow::bail!("Wayland output {requested} does not exist");
        }

        let monitors = hyprctl(&["monitors", "all", "-j"]).await?;
        let selected = parse_outputs(&monitors, &self.headless_output)?
            .into_iter()
            .find(|output| output.name == requested)
            .with_context(|| format!("Wayland output {requested} disappeared"))?;
        let selection = DesktopSelection {
            output: selected.name,
            mode: selected.mode,
            workspace: selected.workspace,
            virtual_output: selected.virtual_output,
        };
        *self.selection.lock().expect("desktop selection poisoned") = selection.clone();
        self.active.store(true, Ordering::Release);
        info!(output = %selection.output, mode = %selection.mode, workspace = selection.workspace, "desktop capture started");
        Ok(selection)
    }

    async fn stop(&self) -> Result<()> {
        let _transition = self.transition.lock().await;
        if !self.active.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let monitors = hyprctl(&["monitors", "all", "-j"]).await?;
        if parse_outputs(&monitors, &self.headless_output)?
            .iter()
            .any(|output| output.name == self.headless_output)
        {
            hyprctl(&["output", "remove", &self.headless_output]).await?;
        }
        info!("desktop capture stopped");
        Ok(())
    }

    async fn launch_terminal(&self) -> Result<u32> {
        if !self.active.load(Ordering::Acquire) {
            anyhow::bail!("start a desktop capture first");
        }
        let selection = self.selection();
        let monitors = hyprctl(&["monitors", "all", "-j"]).await?;
        let workspace = monitor_workspace(&monitors, &selection.output)?;
        let rule = format!("[workspace {workspace} silent] kitty --title EutherGate-Remote-Forge");
        hyprctl(&["dispatch", "exec", &rule]).await?;
        Ok(workspace)
    }
}

fn parse_outputs(monitors: &str, headless_output: &str) -> Result<Vec<DesktopOutput>> {
    let monitors: Vec<HyprMonitor> =
        serde_json::from_str(monitors).context("hyprctl returned invalid monitor JSON")?;
    Ok(monitors
        .into_iter()
        .map(|monitor| DesktopOutput {
            virtual_output: monitor.name == headless_output,
            mode: format!(
                "{}x{}@{}",
                monitor.width,
                monitor.height,
                monitor.refresh_rate.round() as u32
            ),
            workspace: monitor.active_workspace.id,
            description: if monitor.description.is_empty() {
                "Virtual output".into()
            } else {
                monitor.description
            },
            name: monitor.name,
        })
        .collect())
}

fn monitor_workspace(monitors: &str, output: &str) -> Result<u32> {
    let monitors: serde_json::Value =
        serde_json::from_str(monitors).context("hyprctl returned invalid monitor JSON")?;
    monitors
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("name").and_then(|name| name.as_str()) == Some(output))
        })
        .and_then(|item| item.pointer("/activeWorkspace/id"))
        .and_then(|id| id.as_u64())
        .and_then(|id| u32::try_from(id).ok())
        .with_context(|| format!("Hyprland output {output} has no active workspace"))
}

async fn hyprctl(args: &[&str]) -> Result<String> {
    let output = Command::new("hyprctl")
        .args(args)
        .output()
        .await
        .with_context(|| format!("could not run hyprctl {}", args.join(" ")))?;
    if !output.status.success() {
        anyhow::bail!(
            "hyprctl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

async fn desktop_socket(socket: WebSocket, desktop: Arc<DesktopManager>) -> Result<()> {
    let selection = desktop.selection();
    let mut child = Command::new("python")
        .arg(&desktop.helper)
        .args(["--output", &selection.output, "--mode", &selection.mode])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("could not start {}", desktop.helper.display()))?;
    let mut child_input = child
        .stdin
        .take()
        .context("WebRTC helper stdin unavailable")?;
    let child_output = child
        .stdout
        .take()
        .context("WebRTC helper stdout unavailable")?;
    let mut child_lines = BufReader::new(child_output).lines();
    let (mut sender, mut receiver) = socket.split();

    loop {
        tokio::select! {
            line = child_lines.next_line() => match line {
                Ok(Some(line)) => {
                    if sender.send(Message::Text(line.into())).await.is_err() { break; }
                }
                Ok(None) => break,
                Err(error) => return Err(error).context("could not read WebRTC helper output"),
            },
            message = receiver.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    child_input.write_all(text.as_bytes()).await?;
                    child_input.write_all(b"\n").await?;
                    child_input.flush().await?;
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    warn!(%error, "desktop signaling WebSocket disconnected");
                    break;
                }
            },
            status = child.wait() => {
                let status = status.context("could not wait for WebRTC helper")?;
                anyhow::bail!("WebRTC helper exited with {status}");
            }
        }
    }

    let _ = child_input.write_all(b"{\"type\":\"stop\"}\n").await;
    drop(child_input);
    if tokio::time::timeout(std::time::Duration::from_secs(2), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
    }
    Ok(())
}

fn is_authenticated(headers: &HeaderMap, state: &AppState) -> bool {
    authentication_mode(headers, state) != "none"
}

fn authentication_mode(headers: &HeaderMap, state: &AppState) -> &'static str {
    if proxy_token_authenticated(headers, state.proxy_token_hash.as_ref()) {
        return "eutheroxide_proxy";
    }

    if cookie_authenticated(headers, state) {
        "gate_cookie"
    } else {
        "none"
    }
}

fn proxy_token_authenticated(headers: &HeaderMap, expected: Option<&[u8; 32]>) -> bool {
    expected.is_some_and(|expected| {
        headers
            .get(PROXY_AUTH_HEADER)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| constant_time_eq(&hash_token(value), expected))
    })
}

fn cookie_authenticated(headers: &HeaderMap, state: &AppState) -> bool {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|part| {
                let (name, value) = part.trim().split_once('=')?;
                (name == AUTH_COOKIE).then_some(value)
            })
        })
        .is_some_and(|value| constant_time_eq(value.as_bytes(), state.auth_session.as_bytes()))
}

fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

fn random_secret(bytes: usize) -> String {
    let mut random = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut random);
    URL_SAFE_NO_PAD.encode(random)
}

fn env_bool(name: &str, default: bool) -> Result<bool> {
    match env::var(name) {
        Ok(value) if matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes") => {
            Ok(true)
        }
        Ok(value) if matches!(value.to_ascii_lowercase().as_str(), "0" | "false" | "no") => {
            Ok(false)
        }
        Ok(_) => anyhow::bail!("{name} must be true or false"),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("euthergate=info,tower_http=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler")
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install terminate handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_comparison_handles_equal_and_different_values() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"diff"));
        assert!(!constant_time_eq(b"short", b"longer"));
    }

    #[test]
    fn token_hash_is_stable_and_not_plaintext() {
        assert_eq!(hash_token("gate"), hash_token("gate"));
        assert_ne!(hash_token("gate"), hash_token("other"));
    }

    #[test]
    fn trusted_proxy_token_must_be_configured_and_match() {
        let expected = hash_token("oxide-secret");
        let mut headers = HeaderMap::new();
        headers.insert(PROXY_AUTH_HEADER, HeaderValue::from_static("wrong"));
        assert!(!proxy_token_authenticated(&headers, Some(&expected)));
        headers.insert(PROXY_AUTH_HEADER, HeaderValue::from_static("oxide-secret"));
        assert!(proxy_token_authenticated(&headers, Some(&expected)));
        assert!(!proxy_token_authenticated(&headers, None));
    }

    #[test]
    fn monitor_workspace_finds_the_headless_output() {
        let monitors = r#"[
            {"name":"DP-1","activeWorkspace":{"id":2}},
            {"name":"EUTHERGATE-1","activeWorkspace":{"id":50}}
        ]"#;
        assert_eq!(monitor_workspace(monitors, "EUTHERGATE-1").unwrap(), 50);
        assert!(monitor_workspace(monitors, "MISSING").is_err());
    }

    #[test]
    fn output_parser_distinguishes_physical_and_virtual_outputs() {
        let monitors = r#"[
            {
                "name":"DP-1",
                "description":"DisplayPort monitor",
                "width":1600,
                "height":900,
                "refreshRate":60.0,
                "activeWorkspace":{"id":2}
            },
            {
                "name":"EUTHERGATE-1",
                "description":"",
                "width":1280,
                "height":720,
                "refreshRate":30.0,
                "activeWorkspace":{"id":3}
            }
        ]"#;
        let outputs = parse_outputs(monitors, "EUTHERGATE-1").unwrap();

        assert_eq!(outputs[0].name, "DP-1");
        assert_eq!(outputs[0].mode, "1600x900@60");
        assert_eq!(outputs[0].workspace, 2);
        assert!(!outputs[0].virtual_output);
        assert_eq!(outputs[1].description, "Virtual output");
        assert!(outputs[1].virtual_output);
    }
}
