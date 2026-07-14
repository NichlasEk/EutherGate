use std::{
    collections::VecDeque,
    env,
    io::{Read, Write},
    net::SocketAddr,
    path::PathBuf,
    process::Stdio,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    sync::{Arc, Mutex as StdMutex},
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{
        State, WebSocketUpgrade,
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
const HISTORY_LIMIT: usize = 256 * 1024;

#[derive(Clone)]
struct AppState {
    token_hash: [u8; 32],
    auth_session: Arc<str>,
    secure_cookie: bool,
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
    output: String,
    mode: String,
    workspace: AtomicU32,
    helper: PathBuf,
}

#[derive(Deserialize)]
struct LoginRequest {
    token: String,
}

#[derive(Serialize)]
struct StatusResponse {
    authenticated: bool,
    terminal_ready: bool,
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
        terminal,
        desktop: Arc::new(DesktopManager {
            transition: Mutex::new(()),
            active: AtomicBool::new(false),
            viewer_connected: AtomicBool::new(false),
            output: config.desktop_output.clone(),
            mode: config.desktop_mode.clone(),
            workspace: AtomicU32::new(0),
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
    Json(StatusResponse {
        authenticated: is_authenticated(&headers, &state),
        terminal_ready: true,
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
    Json(DesktopStatusResponse {
        available: desktop.helper.is_file(),
        active: desktop.active.load(Ordering::Acquire),
        viewer_connected: desktop.viewer_connected.load(Ordering::Acquire),
        output: desktop.output.clone(),
        mode: desktop.mode.clone(),
        workspace: desktop.workspace.load(Ordering::Acquire),
        transport: "WebRTC/VP8",
        input: "WebRTC DataChannel",
    })
    .into_response()
}

async fn desktop_start(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.desktop.start().await {
        Ok(()) => Json(serde_json::json!({ "active": true })).into_response(),
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
        return (StatusCode::CONFLICT, "virtual desktop is not active").into_response();
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
    async fn start(&self) -> Result<()> {
        let _transition = self.transition.lock().await;
        if self.active.load(Ordering::Acquire) {
            return Ok(());
        }
        if !self.helper.is_file() {
            anyhow::bail!("WebRTC helper not found at {}", self.helper.display());
        }

        let monitors = hyprctl(&["monitors", "all", "-j"]).await?;
        if !monitors.contains(&format!("\"{}\"", self.output)) {
            hyprctl(&["output", "create", "headless", &self.output]).await?;
        }
        if let Err(error) = hyprctl(&[
            "keyword",
            "monitor",
            &format!("{}, {},auto,1", self.output, self.mode).replace(", ", ","),
        ])
        .await
        {
            let _ = hyprctl(&["output", "remove", &self.output]).await;
            return Err(error);
        }
        let monitors = hyprctl(&["monitors", "all", "-j"]).await?;
        let workspace = monitor_workspace(&monitors, &self.output)?;
        self.workspace.store(workspace, Ordering::Release);
        self.active.store(true, Ordering::Release);
        info!(output = %self.output, mode = %self.mode, workspace, "virtual desktop started");
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let _transition = self.transition.lock().await;
        if !self.active.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        hyprctl(&["output", "remove", &self.output]).await?;
        info!(output = %self.output, "virtual desktop stopped");
        Ok(())
    }
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
    let mut child = Command::new("python")
        .arg(&desktop.helper)
        .args(["--output", &desktop.output, "--mode", &desktop.mode])
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
    fn monitor_workspace_finds_the_headless_output() {
        let monitors = r#"[
            {"name":"DP-1","activeWorkspace":{"id":2}},
            {"name":"EUTHERGATE-1","activeWorkspace":{"id":50}}
        ]"#;
        assert_eq!(monitor_workspace(monitors, "EUTHERGATE-1").unwrap(), 50);
        assert!(monitor_workspace(monitors, "MISSING").is_err());
    }
}
