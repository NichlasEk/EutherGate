use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    io::{Read, Write},
    net::SocketAddr,
    os::unix::fs::MetadataExt,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex as StdMutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Path as AxumPath, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    process::Command,
    sync::{Mutex, broadcast},
    time::{Duration, Instant, MissedTickBehavior, timeout},
};
use tower_http::{
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const AUTH_COOKIE: &str = "euthergate_session";
const PROXY_AUTH_HEADER: &str = "x-euthergate-proxy-token";
const HISTORY_LIMIT: usize = 256 * 1024;
const CLIPBOARD_LIMIT: usize = 8 * 1024 * 1024;
const CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(4);
const TERMINAL_IMAGE_LIMIT: usize = 8 * 1024 * 1024;
const DISPLAY_WAKE_HOLD_SECONDS: u64 = 120;
const TURN_CREDENTIAL_TTL_SECONDS: u64 = 60 * 60;
const FALLBACK_PACKET_HEADER_BYTES: usize = 5;
const FALLBACK_MAX_PACKET_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_TERMINAL_SESSION: &str = "gate";
const BROWSER_WORKSPACE_START: u32 = 10;
const DESKTOP_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const DESKTOP_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(20);
const DESKTOP_SEND_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
struct AppState {
    token_hash: [u8; 32],
    auth_session: Arc<str>,
    secure_cookie: bool,
    proxy_token_hash: Option<[u8; 32]>,
    turn: Option<Arc<TurnConfig>>,
    terminals: Arc<TerminalManager>,
    terminal_upload_dir: Arc<PathBuf>,
    desktop: Arc<DesktopManager>,
    browser: Arc<BrowserManager>,
}

struct TerminalSession {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    _child: StdMutex<Box<dyn portable_pty::Child + Send + Sync>>,
    output: Arc<OutputRelay>,
}

struct TerminalManager {
    tmux: PathBuf,
    socket_name: String,
    shell: PathBuf,
    workdir: PathBuf,
    sessions: StdMutex<HashMap<String, Arc<TerminalSession>>>,
}

struct DesktopManager {
    transition: Mutex<()>,
    active: AtomicBool,
    viewer_connected: AtomicBool,
    headless_output: String,
    headless_mode: String,
    forge_session_file: PathBuf,
    selection: StdMutex<DesktopSelection>,
    helper: PathBuf,
    fallback_helper: PathBuf,
    wayvnc: Option<PathBuf>,
    vnc_keyboard: String,
}

struct BrowserManager {
    transition: Mutex<()>,
    firefox: Option<PathBuf>,
    profile_dir: PathBuf,
    forge_session_file: PathBuf,
    start_url: String,
}

struct DesktopViewerGuard {
    desktop: Arc<DesktopManager>,
    transport: &'static str,
}

impl Drop for DesktopViewerGuard {
    fn drop(&mut self) {
        self.desktop
            .viewer_connected
            .store(false, Ordering::Release);
        info!(transport = self.transport, "desktop viewer disconnected");
    }
}

#[derive(Clone)]
struct DesktopSelection {
    backend: DesktopBackend,
    capture_output: String,
    id: String,
    output: String,
    mode: String,
    workspace: u32,
    virtual_output: bool,
}

#[derive(Clone)]
enum DesktopBackend {
    Unavailable,
    Hyprland {
        signature: String,
        wayland_display: String,
    },
    Sway {
        wayland_display: String,
        sway_socket: String,
    },
}

#[derive(Clone)]
struct ResolvedOutput {
    public: DesktopOutput,
    backend: DesktopBackend,
    capture_output: String,
    present: bool,
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

#[derive(Clone, Serialize)]
struct TerminalSessionInfo {
    name: String,
    windows: u32,
    attached: u32,
    other_clients: u32,
    activity: u64,
    command: String,
    path: String,
}

#[derive(Deserialize)]
struct TerminalSessionQuery {
    session: Option<String>,
}

#[derive(Deserialize)]
struct CreateTerminalSessionRequest {
    name: String,
}

#[derive(Clone, Serialize)]
struct BrowserSessionInfo {
    id: u64,
    title: String,
    workspace: String,
    focused: bool,
}

#[derive(Serialize)]
struct DesktopStatusResponse {
    available: bool,
    active: bool,
    viewer_connected: bool,
    output_id: String,
    output: String,
    mode: String,
    workspace: u32,
    transport: &'static str,
    input: &'static str,
    virtual_output: bool,
    outputs: Vec<DesktopOutput>,
    ice_servers: Vec<IceServer>,
    transport_profiles: Vec<TransportProfile>,
}

#[derive(Clone)]
struct TurnConfig {
    urls: Vec<String>,
    shared_secret: Arc<str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IceServer {
    urls: Vec<String>,
    username: String,
    credential: String,
}

#[derive(Serialize)]
struct TransportProfile {
    id: &'static str,
    label: &'static str,
    description: &'static str,
    ice_transport_policy: &'static str,
    urls: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VncPerformanceProfile {
    Compatible,
    Smooth,
    Gpu,
}

#[derive(Deserialize)]
struct VncProfileQuery {
    profile: Option<String>,
}

impl VncPerformanceProfile {
    fn parse(value: Option<&str>) -> Option<Self> {
        match value.unwrap_or("compatible") {
            "compatible" => Some(Self::Compatible),
            "smooth" => Some(Self::Smooth),
            "gpu" => Some(Self::Gpu),
            _ => None,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Compatible => "compatible",
            Self::Smooth => "smooth",
            Self::Gpu => "gpu",
        }
    }

    fn max_fps(self) -> &'static str {
        match self {
            Self::Compatible => "30",
            Self::Smooth | Self::Gpu => "60",
        }
    }

    fn gpu(self) -> bool {
        self == Self::Gpu
    }
}

#[derive(Clone, Serialize)]
struct DesktopOutput {
    id: String,
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

#[derive(Serialize)]
struct DisplayWakeResponse {
    woken: Vec<String>,
    locked: bool,
    hold_seconds: u64,
}

#[derive(Serialize)]
struct ServiceRestartResponse {
    service: String,
    unit: &'static str,
    scheduled_seconds: u64,
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
struct HyprInstance {
    instance: String,
    wl_socket: String,
}

#[derive(Deserialize)]
struct SwayOutput {
    name: String,
    active: bool,
    current_workspace: Option<String>,
    current_mode: Option<SwayMode>,
}

#[derive(Deserialize)]
struct SwayMode {
    width: u32,
    height: u32,
    refresh: u32,
}

#[derive(Deserialize)]
struct SwayTreeNode {
    id: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    focused: bool,
    #[serde(default, rename = "type")]
    node_type: String,
    #[serde(default)]
    nodes: Vec<SwayTreeNode>,
    #[serde(default)]
    floating_nodes: Vec<SwayTreeNode>,
}

struct ForgeSession {
    wayland_display: String,
    sway_socket: String,
    output: String,
}

struct ClipboardPayload {
    mime: String,
    data: Vec<u8>,
}

#[derive(Serialize)]
struct TerminalImageUploadResponse {
    path: String,
    mime: &'static str,
    size: usize,
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

    let terminals = Arc::new(TerminalManager::new(
        config.tmux.clone(),
        config.tmux_socket.clone(),
        config.shell.clone(),
        config.workdir.clone(),
    )?);
    let state = AppState {
        token_hash: hash_token(&config.token),
        auth_session: Arc::from(random_secret(32)),
        secure_cookie: config.secure_cookie,
        proxy_token_hash: config.proxy_token.as_deref().map(hash_token),
        turn: config.turn.clone().map(Arc::new),
        terminals,
        terminal_upload_dir: Arc::new(config.terminal_upload_dir.clone()),
        desktop: Arc::new(DesktopManager {
            transition: Mutex::new(()),
            active: AtomicBool::new(false),
            viewer_connected: AtomicBool::new(false),
            headless_output: config.desktop_output.clone(),
            headless_mode: config.desktop_mode.clone(),
            forge_session_file: config.forge_session_file.clone(),
            selection: StdMutex::new(DesktopSelection {
                backend: DesktopBackend::Unavailable,
                capture_output: config.desktop_output.clone(),
                id: config.desktop_output.clone(),
                output: config.desktop_output.clone(),
                mode: config.desktop_mode.clone(),
                workspace: 0,
                virtual_output: true,
            }),
            helper: config.desktop_helper.clone(),
            fallback_helper: config.desktop_fallback_helper.clone(),
            wayvnc: config.wayvnc.clone(),
            vnc_keyboard: config.vnc_keyboard.clone(),
        }),
        browser: Arc::new(BrowserManager {
            transition: Mutex::new(()),
            firefox: config.firefox.clone(),
            profile_dir: config.browser_profile_dir.clone(),
            forge_session_file: config.forge_session_file.clone(),
            start_url: config.browser_start_url.clone(),
        }),
    };

    let static_files = Router::new()
        .fallback_service(
            ServeDir::new(&config.web_root)
                .fallback(ServeFile::new(config.web_root.join("index.html"))),
        )
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ));
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/status", get(status))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/displays/wake", post(display_wake))
        .route("/api/services/{service}/restart", post(service_restart))
        .route(
            "/api/terminal/image",
            post(terminal_image_upload).layer(DefaultBodyLimit::max(TERMINAL_IMAGE_LIMIT)),
        )
        .route(
            "/api/terminal/sessions",
            get(terminal_sessions).post(terminal_session_create),
        )
        .route("/api/terminal/local", post(terminal_local_create))
        .route(
            "/api/browser/sessions",
            get(browser_sessions).post(browser_session_create),
        )
        .route(
            "/api/browser/sessions/{id}/focus",
            post(browser_session_focus),
        )
        .route(
            "/api/browser/sessions/{id}",
            axum::routing::delete(browser_session_close),
        )
        .route(
            "/api/browser/sessions/{id}/close",
            post(browser_session_close),
        )
        .route("/api/desktop/status", get(desktop_status))
        .route("/api/desktop/start", post(desktop_start))
        .route("/api/desktop/stop", post(desktop_stop))
        .route(
            "/api/desktop/launch-terminal",
            post(desktop_launch_terminal),
        )
        .route(
            "/api/desktop/clipboard",
            get(desktop_clipboard_read)
                .post(desktop_clipboard_write)
                .layer(DefaultBodyLimit::max(CLIPBOARD_LIMIT)),
        )
        .route("/ws/terminal", get(terminal_ws))
        .route("/ws/desktop", get(desktop_ws))
        .route("/ws/desktop-fallback", get(desktop_fallback_ws))
        .route("/ws/desktop-vnc", get(desktop_vnc_ws))
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
    turn: Option<TurnConfig>,
    shell: PathBuf,
    workdir: PathBuf,
    tmux: PathBuf,
    tmux_socket: String,
    web_root: PathBuf,
    desktop_output: String,
    desktop_mode: String,
    desktop_helper: PathBuf,
    desktop_fallback_helper: PathBuf,
    wayvnc: Option<PathBuf>,
    vnc_keyboard: String,
    forge_session_file: PathBuf,
    terminal_upload_dir: PathBuf,
    firefox: Option<PathBuf>,
    browser_profile_dir: PathBuf,
    browser_start_url: String,
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
        let turn_urls = env::var("EUTHERGATE_TURN_URLS")
            .ok()
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .filter(|urls| !urls.is_empty());
        let turn_shared_secret = env::var("EUTHERGATE_TURN_SHARED_SECRET")
            .ok()
            .filter(|value| !value.is_empty());
        let turn = match (turn_urls, turn_shared_secret) {
            (Some(urls), Some(shared_secret)) => {
                if urls
                    .iter()
                    .any(|url| !url.starts_with("turn:") && !url.starts_with("turns:"))
                {
                    anyhow::bail!("EUTHERGATE_TURN_URLS entries must use turn: or turns:");
                }
                Some(TurnConfig {
                    urls,
                    shared_secret: Arc::from(shared_secret),
                })
            }
            (None, None) => None,
            _ => anyhow::bail!(
                "EUTHERGATE_TURN_URLS and EUTHERGATE_TURN_SHARED_SECRET must be set together"
            ),
        };
        let shell = env::var_os("EUTHERGATE_SHELL")
            .or_else(|| env::var_os("SHELL"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        let workdir = env::var_os("EUTHERGATE_WORKDIR")
            .map(PathBuf::from)
            .unwrap_or(env::current_dir().context("could not determine current directory")?);
        let tmux = env::var_os("EUTHERGATE_TMUX")
            .map(PathBuf::from)
            .map(|path| resolve_executable(&path))
            .unwrap_or_else(|| resolve_executable(Path::new("tmux")))
            .context("tmux is required for persistent terminal sessions")?;
        let tmux_socket =
            env::var("EUTHERGATE_TMUX_SOCKET").unwrap_or_else(|_| "euthergate".into());
        if !valid_tmux_socket_name(&tmux_socket) {
            anyhow::bail!("EUTHERGATE_TMUX_SOCKET must contain only letters, numbers, _ or -");
        }
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
        let desktop_fallback_helper = env::var_os("EUTHERGATE_DESKTOP_FALLBACK_HELPER")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("scripts/wss_desktop.py"));
        let wayvnc = env::var_os("EUTHERGATE_WAYVNC_BIN")
            .map(PathBuf::from)
            .map(|path| resolve_executable(&path))
            .unwrap_or_else(|| resolve_executable(Path::new("wayvnc")));
        let vnc_keyboard = env::var("EUTHERGATE_VNC_KEYBOARD")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "se".into());
        let forge_session_file = env::var_os("EUTHERGATE_FORGE_SESSION_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", nix_uid())));
                runtime_dir.join("euthergate-forge/session.env")
            });
        let terminal_upload_dir = env::var_os("EUTHERGATE_TERMINAL_UPLOAD_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                env::temp_dir()
                    .join(format!("euthergate-{}", nix_uid()))
                    .join("terminal-images")
            });
        prepare_private_directory(&terminal_upload_dir)?;
        let firefox = env::var_os("EUTHERGATE_FIREFOX_BIN")
            .map(PathBuf::from)
            .map(|path| resolve_executable(&path))
            .unwrap_or_else(|| resolve_executable(Path::new("firefox")));
        let browser_profile_dir = env::var_os("EUTHERGATE_BROWSER_PROFILE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(format!("/home/{}", nix_uid())))
                    .join(".local/share/euthergate/browser/firefox-profile")
            });
        prepare_private_directory(&browser_profile_dir)?;
        prepare_firefox_profile(&browser_profile_dir)?;
        let browser_start_url = env::var("EUTHERGATE_BROWSER_START_URL")
            .unwrap_or_else(|_| "https://chatgpt.com/".into());
        if !browser_start_url.starts_with("https://")
            || browser_start_url.chars().any(char::is_whitespace)
        {
            anyhow::bail!("EUTHERGATE_BROWSER_START_URL must be one HTTPS URL");
        }

        Ok(Self {
            bind,
            token,
            generated_token,
            secure_cookie,
            proxy_token,
            turn,
            shell,
            workdir,
            tmux,
            tmux_socket,
            web_root,
            desktop_output,
            desktop_mode,
            desktop_helper,
            desktop_fallback_helper,
            wayvnc,
            vnc_keyboard,
            forge_session_file,
            terminal_upload_dir,
            firefox,
            browser_profile_dir,
            browser_start_url,
        })
    }
}

impl TerminalManager {
    fn new(tmux: PathBuf, socket_name: String, shell: PathBuf, workdir: PathBuf) -> Result<Self> {
        let manager = Self {
            tmux,
            socket_name,
            shell,
            workdir,
            sessions: StdMutex::new(HashMap::new()),
        };
        manager.ensure_tmux_session(DEFAULT_TERMINAL_SESSION)?;
        Ok(manager)
    }

    fn ensure_tmux_session(&self, name: &str) -> Result<()> {
        validate_terminal_session_name(name)?;
        let target = format!("={name}");
        let exists = StdCommand::new(&self.tmux)
            .args(["-L", &self.socket_name, "has-session", "-t", &target])
            .output()
            .context("could not query tmux sessions")?
            .status
            .success();
        if exists {
            return Ok(());
        }

        let output = StdCommand::new(&self.tmux)
            .args([
                "-L",
                &self.socket_name,
                "new-session",
                "-d",
                "-s",
                name,
                "-c",
            ])
            .arg(&self.workdir)
            .arg(&self.shell)
            .env("SHELL", &self.shell)
            .output()
            .context("could not create tmux session")?;
        if output.status.success() {
            return Ok(());
        }

        let raced = StdCommand::new(&self.tmux)
            .args(["-L", &self.socket_name, "has-session", "-t", &target])
            .output()
            .context("could not recheck tmux session")?
            .status
            .success();
        if raced {
            return Ok(());
        }
        anyhow::bail!(
            "tmux could not create session {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }

    fn session(&self, name: &str) -> Result<Arc<TerminalSession>> {
        self.ensure_tmux_session(name)?;
        let mut sessions = self.sessions.lock().expect("terminal sessions poisoned");
        if let Some(session) = sessions.get(name)
            && session.is_alive()
        {
            return Ok(session.clone());
        }
        sessions.remove(name);
        let session = Arc::new(TerminalSession::spawn_tmux(
            &self.tmux,
            &self.socket_name,
            name,
            &self.workdir,
        )?);
        sessions.insert(name.to_owned(), session.clone());
        Ok(session)
    }

    async fn list_sessions(&self) -> Result<Vec<TerminalSessionInfo>> {
        let gateway_sessions: Vec<String> = self
            .sessions
            .lock()
            .expect("terminal sessions poisoned")
            .iter()
            .filter(|(_, session)| session.is_alive())
            .map(|(name, _)| name.clone())
            .collect();
        let output = Command::new(&self.tmux)
            .args([
                "-L",
                &self.socket_name,
                "list-sessions",
                "-F",
                "#{session_name}\t#{session_windows}\t#{session_attached}\t#{session_activity}\t#{pane_current_command}\t#{pane_current_path}",
            ])
            .output()
            .await
            .context("could not list tmux sessions")?;
        if !output.status.success() {
            anyhow::bail!(
                "tmux could not list sessions: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let mut sessions = parse_terminal_sessions(&String::from_utf8_lossy(&output.stdout))?;
        for session in &mut sessions {
            if gateway_sessions.contains(&session.name) {
                session.other_clients = session.attached.saturating_sub(1);
            } else {
                session.other_clients = session.attached;
            }
        }
        Ok(sessions)
    }
}

impl TerminalSession {
    fn spawn_tmux(tmux: &Path, socket_name: &str, name: &str, workdir: &Path) -> Result<Self> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows: 30,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("could not create terminal PTY")?;
        let mut command = CommandBuilder::new(tmux);
        command.args(["-L", socket_name, "attach-session", "-t"]);
        command.arg(format!("={name}"));
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

    fn is_alive(&self) -> bool {
        self._child
            .lock()
            .expect("terminal child poisoned")
            .try_wait()
            .is_ok_and(|status| status.is_none())
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

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let Ok(child) = self._child.get_mut() else {
            return;
        };
        if child.try_wait().is_ok_and(|status| status.is_none()) {
            // Stop the tmux client while its PTY is still open. Dropping the PTY
            // first delivers a terminal hangup that can also tear down the tmux
            // pane instead of merely detaching this gateway viewer.
            if child.kill().is_ok() {
                let _ = child.wait();
            }
        }
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

async fn terminal_sessions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.terminals.list_sessions().await {
        Ok(sessions) => Json(serde_json::json!({ "sessions": sessions })).into_response(),
        Err(error) => {
            error!(%error, "could not list terminal sessions");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

async fn terminal_session_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateTerminalSessionRequest>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let Err(error) = validate_terminal_session_name(&request.name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response();
    }
    match state.terminals.ensure_tmux_session(&request.name) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "name": request.name })),
        )
            .into_response(),
        Err(error) => {
            error!(%error, "could not create terminal session");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

async fn terminal_local_create(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let name = new_local_terminal_session_name();
    if let Err(error) = state.terminals.ensure_tmux_session(&name) {
        error!(%error, session = name, "could not create local terminal session");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response();
    }
    match state
        .desktop
        .launch_hyprland_terminal(&state.terminals.tmux, &state.terminals.socket_name, &name)
        .await
    {
        Ok(()) => Json(serde_json::json!({ "name": name })).into_response(),
        Err(error) => {
            error!(%error, session = name, "could not launch local terminal window");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": error.to_string(), "name": name })),
            )
                .into_response()
        }
    }
}

async fn browser_sessions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.browser.sessions().await {
        Ok(sessions) => Json(serde_json::json!({
            "available": state.browser.firefox.is_some(),
            "sessions": sessions,
        }))
        .into_response(),
        Err(error) => {
            warn!(%error, "could not list EutherBrowse sessions");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

async fn browser_session_create(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.browser.launch().await {
        Ok(session) => (StatusCode::CREATED, Json(session)).into_response(),
        Err(error) => {
            error!(%error, "could not launch EutherBrowse Firefox window");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

async fn browser_session_focus(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<u64>,
    headers: HeaderMap,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.browser.focus(id).await {
        Ok(session) => Json(session).into_response(),
        Err(error) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn browser_session_close(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<u64>,
    headers: HeaderMap,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.browser.close(id).await {
        Ok(()) => {
            info!(browser_session_id = id, "Firefox window closed");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            warn!(browser_session_id = id, %error, "Firefox window close failed");
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

async fn terminal_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<TerminalSessionQuery>,
    headers: HeaderMap,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let name = query.session.as_deref().unwrap_or(DEFAULT_TERMINAL_SESSION);
    if let Err(error) = validate_terminal_session_name(name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response();
    }
    match state.terminals.session(name) {
        Ok(terminal) => ws
            .on_upgrade(move |socket| terminal_socket(socket, terminal))
            .into_response(),
        Err(error) => {
            error!(%error, session = name, "could not attach terminal session");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
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

async fn terminal_image_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some((mime, extension)) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(terminal_image_format)
    else {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(
                serde_json::json!({ "error": "terminal paste supports PNG, JPEG and WebP images" }),
            ),
        )
            .into_response();
    };
    if body.is_empty() || !valid_image_signature(mime, body.as_ref()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "clipboard data is not a valid image" })),
        )
            .into_response();
    }

    let upload_dir = state.terminal_upload_dir.clone();
    let size = body.len();
    let result = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
        prepare_private_directory(upload_dir.as_ref())?;
        for _ in 0..4 {
            let path = upload_dir.join(format!("paste-{}.{}", random_secret(9), extension));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(mut file) => {
                    file.write_all(body.as_ref())?;
                    file.sync_all()?;
                    return Ok(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("could not allocate a unique terminal image name")
    })
    .await;

    match result {
        Ok(Ok(path)) => {
            info!(path = %path.display(), size, "stored terminal clipboard image");
            Json(TerminalImageUploadResponse {
                path: path.to_string_lossy().into_owned(),
                mime,
                size,
            })
            .into_response()
        }
        Ok(Err(error)) => {
            warn!(%error, "could not store terminal clipboard image");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "could not store clipboard image" })),
            )
                .into_response()
        }
        Err(error) => {
            error!(%error, "terminal image upload task failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn display_wake(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match wake_physical_displays(&state.desktop.headless_output).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => {
            warn!(%error, "could not wake physical displays");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

async fn service_restart(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(service): AxumPath<String>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let Some(unit) = restart_unit_for_service(&service) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "unknown EutherGate service" })),
        )
            .into_response();
    };
    let timer_unit = format!(
        "euthergate-remote-restart-{}-{}",
        service,
        random_secret(6).replace('_', "-")
    );
    let output = Command::new("systemd-run")
        .args([
            "--user",
            "--collect",
            "--on-active=2s",
            "--unit",
            &timer_unit,
            "/usr/bin/systemctl",
            "--user",
            "restart",
            unit,
        ])
        .output()
        .await;
    match output {
        Ok(output) if output.status.success() => {
            info!(service, unit, "scheduled remote service restart");
            (
                StatusCode::ACCEPTED,
                Json(ServiceRestartResponse {
                    service,
                    unit,
                    scheduled_seconds: 2,
                }),
            )
                .into_response()
        }
        Ok(output) => {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            warn!(service, unit, %detail, "could not schedule remote service restart");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": detail })),
            )
                .into_response()
        }
        Err(error) => {
            warn!(service, unit, %error, "could not launch systemd-run");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

fn restart_unit_for_service(service: &str) -> Option<&'static str> {
    match service {
        "gateway" => Some("euthergate.service"),
        "tunnel" => Some("euthergate-tunnel.service"),
        "forge" => Some("euthergate-forge.service"),
        _ => None,
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
    let ice_servers = state
        .turn
        .as_deref()
        .and_then(TurnConfig::ice_server)
        .into_iter()
        .collect();
    let transport_profiles = transport_profiles(state.turn.as_deref(), desktop.wayvnc.is_some());
    Json(DesktopStatusResponse {
        available: desktop.helper.is_file(),
        active: desktop.active.load(Ordering::Acquire),
        viewer_connected: desktop.viewer_connected.load(Ordering::Acquire),
        output_id: selection.id,
        output: selection.output,
        mode: selection.mode,
        workspace: selection.workspace,
        transport: "WebRTC/VP8",
        input: "WebRTC DataChannel",
        virtual_output: selection.virtual_output,
        outputs,
        ice_servers,
        transport_profiles,
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

async fn desktop_launch_terminal(
    State(state): State<AppState>,
    Query(query): Query<TerminalSessionQuery>,
    headers: HeaderMap,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let name = query.session.as_deref().unwrap_or(DEFAULT_TERMINAL_SESSION);
    if let Err(error) = validate_terminal_session_name(name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response();
    }
    if let Err(error) = state.terminals.ensure_tmux_session(name) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response();
    }
    match state
        .desktop
        .launch_terminal(&state.terminals.tmux, &state.terminals.socket_name, name)
        .await
    {
        Ok(workspace) => Json(serde_json::json!({ "workspace": workspace })).into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn desktop_clipboard_read(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.desktop.read_clipboard().await {
        Ok(Some(payload)) => {
            let mut response_headers = HeaderMap::new();
            response_headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&payload.mime).expect("supported clipboard MIME type"),
            );
            response_headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-store, max-age=0"),
            );
            (response_headers, payload.data).into_response()
        }
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            warn!(%error, "could not read Wayland clipboard");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

async fn desktop_clipboard_write(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(content_type) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(supported_upload_mime)
    else {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(
                serde_json::json!({ "error": "clipboard supports plain text, PNG, JPEG and WebP" }),
            ),
        )
            .into_response();
    };
    if body.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "clipboard payload is empty" })),
        )
            .into_response();
    }
    match state
        .desktop
        .write_clipboard(content_type, body.as_ref())
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            warn!(%error, "could not write Wayland clipboard");
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

fn acquire_desktop_viewer(
    desktop: &Arc<DesktopManager>,
    transport: &'static str,
) -> Option<DesktopViewerGuard> {
    if desktop
        .viewer_connected
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        warn!(
            transport,
            "desktop viewer rejected because another viewer is active"
        );
        return None;
    }
    info!(transport, "desktop viewer connected");
    Some(DesktopViewerGuard {
        desktop: desktop.clone(),
        transport,
    })
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
    let Some(viewer) = acquire_desktop_viewer(&state.desktop, "webrtc") else {
        return (
            StatusCode::CONFLICT,
            "a desktop viewer is already connected",
        )
            .into_response();
    };
    let desktop = state.desktop.clone();
    ws.on_upgrade(move |socket| async move {
        let _viewer = viewer;
        if let Err(error) = desktop_socket(socket, desktop.clone()).await {
            error!(%error, "desktop WebRTC bridge stopped");
        }
    })
    .into_response()
}

async fn desktop_fallback_ws(
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
    if !state.desktop.fallback_helper.is_file() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "HTTPS/WSS desktop helper is unavailable",
        )
            .into_response();
    }
    let Some(viewer) = acquire_desktop_viewer(&state.desktop, "https-wss") else {
        return (
            StatusCode::CONFLICT,
            "a desktop viewer is already connected",
        )
            .into_response();
    };
    let desktop = state.desktop.clone();
    ws.max_message_size(256 * 1024)
        .on_upgrade(move |socket| async move {
            let _viewer = viewer;
            if let Err(error) = desktop_fallback_socket(socket, desktop.clone()).await {
                error!(%error, "desktop HTTPS/WSS bridge stopped");
            }
        })
        .into_response()
}

async fn desktop_vnc_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<VncProfileQuery>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !state.desktop.active.load(Ordering::Acquire) {
        return (StatusCode::CONFLICT, "desktop capture is not active").into_response();
    }
    if state.desktop.wayvnc.is_none() {
        return (StatusCode::SERVICE_UNAVAILABLE, "WayVNC is unavailable").into_response();
    }
    let Some(profile) = VncPerformanceProfile::parse(query.profile.as_deref()) else {
        return (
            StatusCode::BAD_REQUEST,
            "unknown VNC profile; expected compatible, smooth or gpu",
        )
            .into_response();
    };
    let Some(viewer) = acquire_desktop_viewer(&state.desktop, "vnc-wss") else {
        return (
            StatusCode::CONFLICT,
            "a desktop viewer is already connected",
        )
            .into_response();
    };
    let desktop = state.desktop.clone();
    ws.max_message_size(16 * 1024 * 1024)
        .max_frame_size(16 * 1024 * 1024)
        .on_upgrade(move |socket| async move {
            let _viewer = viewer;
            if let Err(error) = desktop_vnc_socket(socket, desktop.clone(), profile).await {
                error!(%error, "desktop VNC/WSS bridge stopped");
            }
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
        Ok(self
            .resolved_outputs()
            .await?
            .into_iter()
            .map(|output| output.public)
            .collect())
    }

    async fn start(&self, requested_output: Option<&str>) -> Result<DesktopSelection> {
        let _transition = self.transition.lock().await;
        if !self.helper.is_file() {
            anyhow::bail!("WebRTC helper not found at {}", self.helper.display());
        }

        let mut outputs = self.resolved_outputs().await?;
        let fallback_id = outputs
            .iter()
            .find(|output| matches!(output.backend, DesktopBackend::Sway { .. }))
            .or_else(|| outputs.first())
            .map(|output| output.public.id.clone())
            .unwrap_or_else(|| self.headless_output.clone());
        let requested = requested_output.map(str::to_owned).unwrap_or(fallback_id);
        let previous = self.selection();
        if self.viewer_connected.load(Ordering::Acquire) && requested != previous.id {
            anyhow::bail!("disconnect the current viewer before switching output");
        }

        let mut selected = outputs
            .iter()
            .find(|output| output.public.id == requested || output.public.name == requested)
            .cloned()
            .with_context(|| format!("Wayland output {requested} does not exist"))?;

        if !selected.present {
            let DesktopBackend::Hyprland { signature, .. } = &selected.backend else {
                anyhow::bail!("Wayland output {requested} is unavailable");
            };
            hyprctl_instance(
                signature,
                &["output", "create", "headless", &self.headless_output],
            )
            .await?;
            if let Err(error) = hyprctl_instance(
                signature,
                &[
                    "keyword",
                    "monitor",
                    &format!("{},{},auto,1", self.headless_output, self.headless_mode),
                ],
            )
            .await
            {
                let _ =
                    hyprctl_instance(signature, &["output", "remove", &self.headless_output]).await;
                return Err(error);
            }
            outputs = self.resolved_outputs().await?;
            selected = outputs
                .into_iter()
                .find(|output| output.public.id == requested)
                .with_context(|| format!("Wayland output {requested} disappeared"))?;
        }

        let selection = DesktopSelection {
            backend: selected.backend,
            capture_output: selected.capture_output,
            id: selected.public.id,
            output: selected.public.name,
            mode: selected.public.mode,
            workspace: selected.public.workspace,
            virtual_output: selected.public.virtual_output,
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
        let selection = self.selection();
        if selection.capture_output == self.headless_output
            && let DesktopBackend::Hyprland { signature, .. } = &selection.backend
        {
            let monitors = hyprctl_instance(signature, &["monitors", "all", "-j"]).await?;
            if parse_outputs(&monitors, &self.headless_output)?
                .iter()
                .any(|output| output.name == self.headless_output)
            {
                hyprctl_instance(signature, &["output", "remove", &self.headless_output]).await?;
            }
        }
        info!("desktop capture stopped");
        Ok(())
    }

    async fn launch_terminal(&self, tmux: &Path, tmux_socket: &str, session: &str) -> Result<u32> {
        if !self.active.load(Ordering::Acquire) {
            anyhow::bail!("start a desktop capture first");
        }
        validate_terminal_session_name(session)?;
        let terminal_command = format!(
            "kitty --title {} {} -L {} attach-session -t ={}",
            shell_quote(&format!("EutherGate-{session}")),
            shell_quote(&tmux.display().to_string()),
            tmux_socket,
            session
        );
        let selection = self.selection();
        match &selection.backend {
            DesktopBackend::Hyprland { signature, .. } => {
                let monitors = hyprctl_instance(signature, &["monitors", "all", "-j"]).await?;
                let workspace = monitor_workspace(&monitors, &selection.capture_output)?;
                let rule = format!("[workspace {workspace} silent] {terminal_command}");
                hyprctl_instance(signature, &["dispatch", "exec", &rule]).await?;
                Ok(workspace)
            }
            DesktopBackend::Sway { sway_socket, .. } => {
                swayctl(sway_socket, &["exec", &terminal_command]).await?;
                Ok(selection.workspace)
            }
            DesktopBackend::Unavailable => anyhow::bail!("no Wayland session is available"),
        }
    }

    async fn launch_hyprland_terminal(
        &self,
        tmux: &Path,
        tmux_socket: &str,
        session: &str,
    ) -> Result<()> {
        validate_terminal_session_name(session)?;
        let instance = hypr_instances()
            .await?
            .into_iter()
            .next()
            .context("no logged-in Hyprland session is available")?;
        let terminal_command = format!(
            "kitty --title {} {} -L {} attach-session -t ={}",
            shell_quote(&format!("EutherGate-{session}")),
            shell_quote(&tmux.display().to_string()),
            tmux_socket,
            session
        );
        hyprctl_instance(&instance.instance, &["dispatch", "exec", &terminal_command]).await?;
        Ok(())
    }

    async fn read_clipboard(&self) -> Result<Option<ClipboardPayload>> {
        let selection = self.clipboard_selection()?;
        let mut list_command = clipboard_command(&selection, "wl-paste")?;
        let listed = timeout(
            CLIPBOARD_TIMEOUT,
            list_command.args(["--list-types"]).output(),
        )
        .await
        .context("Wayland clipboard type query timed out")?
        .context("could not run wl-paste --list-types")?;
        if !listed.status.success() {
            return Ok(None);
        }
        let types = String::from_utf8_lossy(&listed.stdout);
        let Some((source_mime, response_mime)) = choose_clipboard_mime(&types) else {
            return Ok(None);
        };

        let mut paste_command = clipboard_command(&selection, "wl-paste")?;
        let mut child = paste_command
            .args(["--no-newline", "--type", &source_mime])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("could not start wl-paste")?;
        let stdout = child.stdout.take().context("wl-paste stdout unavailable")?;
        let mut limited = stdout.take((CLIPBOARD_LIMIT + 1) as u64);
        let mut data = Vec::new();
        timeout(CLIPBOARD_TIMEOUT, limited.read_to_end(&mut data))
            .await
            .context("Wayland clipboard read timed out")?
            .context("could not read wl-paste output")?;
        if data.len() > CLIPBOARD_LIMIT {
            let _ = child.kill().await;
            anyhow::bail!("Wayland clipboard exceeds the 8 MiB limit");
        }
        let status = timeout(CLIPBOARD_TIMEOUT, child.wait())
            .await
            .context("wl-paste did not exit")?
            .context("could not wait for wl-paste")?;
        if !status.success() {
            anyhow::bail!("wl-paste could not retrieve {source_mime}");
        }
        if data.is_empty() {
            return Ok(None);
        }
        Ok(Some(ClipboardPayload {
            mime: response_mime,
            data,
        }))
    }

    async fn write_clipboard(&self, mime: &str, data: &[u8]) -> Result<()> {
        let selection = self.clipboard_selection()?;
        let mut copy_command = clipboard_command(&selection, "wl-copy")?;
        let mut child = copy_command
            .args(["--type", mime])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("could not start wl-copy")?;
        let mut stdin = child.stdin.take().context("wl-copy stdin unavailable")?;
        timeout(CLIPBOARD_TIMEOUT, stdin.write_all(data))
            .await
            .context("Wayland clipboard write timed out")?
            .context("could not write wl-copy input")?;
        stdin
            .shutdown()
            .await
            .context("could not close wl-copy input")?;
        drop(stdin);
        let status = timeout(CLIPBOARD_TIMEOUT, child.wait())
            .await
            .context("wl-copy did not accept clipboard data")?
            .context("could not wait for wl-copy")?;
        if !status.success() {
            anyhow::bail!("wl-copy rejected {mime}");
        }
        Ok(())
    }

    fn clipboard_selection(&self) -> Result<DesktopSelection> {
        if !self.active.load(Ordering::Acquire) {
            anyhow::bail!("start a desktop before using its clipboard");
        }
        let selection = self.selection();
        if matches!(selection.backend, DesktopBackend::Unavailable) {
            anyhow::bail!("selected Wayland session is unavailable");
        }
        Ok(selection)
    }

    async fn resolved_outputs(&self) -> Result<Vec<ResolvedOutput>> {
        let mut outputs = Vec::new();
        if let Ok(session) = read_forge_session(&self.forge_session_file) {
            let raw = swayctl(&session.sway_socket, &["-t", "get_outputs", "-r"]).await;
            if let Ok(raw) = raw {
                let sway_outputs: Vec<SwayOutput> =
                    serde_json::from_str(&raw).context("swaymsg returned invalid output JSON")?;
                for output in sway_outputs.into_iter().filter(|output| output.active) {
                    if output.name != session.output {
                        continue;
                    }
                    let Some(mode) = output.current_mode else {
                        continue;
                    };
                    let workspace = output
                        .current_workspace
                        .as_deref()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(1);
                    let name = output.name;
                    outputs.push(ResolvedOutput {
                        public: DesktopOutput {
                            id: format!("forge:{name}"),
                            name: "Forge Session".into(),
                            description: "Always-on headless desktop".into(),
                            mode: format!("{}x{}@{}", mode.width, mode.height, mode.refresh / 1000),
                            workspace,
                            virtual_output: true,
                        },
                        backend: DesktopBackend::Sway {
                            wayland_display: session.wayland_display.clone(),
                            sway_socket: session.sway_socket.clone(),
                        },
                        capture_output: name,
                        present: true,
                    });
                }
            }
        }

        let instances = hypr_instances().await.unwrap_or_default();
        for instance in instances {
            let raw = match hyprctl_instance(&instance.instance, &["monitors", "all", "-j"]).await {
                Ok(raw) => raw,
                Err(error) => {
                    warn!(signature = %instance.instance, %error, "could not inspect Hyprland session");
                    continue;
                }
            };
            let parsed = parse_outputs(&raw, &self.headless_output)?;
            let has_headless = parsed
                .iter()
                .any(|output| output.name == self.headless_output);
            for mut output in parsed {
                let capture_output = output.name.clone();
                output.id = format!("hypr:{}:{}", instance.instance, output.name);
                output.description = if output.virtual_output {
                    "Logged-in Hyprland virtual output".into()
                } else {
                    format!("Logged-in Hyprland · {}", output.description)
                };
                outputs.push(ResolvedOutput {
                    public: output,
                    backend: DesktopBackend::Hyprland {
                        signature: instance.instance.clone(),
                        wayland_display: instance.wl_socket.clone(),
                    },
                    capture_output,
                    present: true,
                });
            }
            if !has_headless {
                outputs.push(ResolvedOutput {
                    public: DesktopOutput {
                        id: format!("hypr:{}:{}", instance.instance, self.headless_output),
                        name: "Logged-in Virtual Output".into(),
                        description: "Create a private output in the logged-in session".into(),
                        mode: self.headless_mode.clone(),
                        workspace: 0,
                        virtual_output: true,
                    },
                    backend: DesktopBackend::Hyprland {
                        signature: instance.instance,
                        wayland_display: instance.wl_socket,
                    },
                    capture_output: self.headless_output.clone(),
                    present: false,
                });
            }
        }
        Ok(outputs)
    }
}

fn parse_outputs(monitors: &str, headless_output: &str) -> Result<Vec<DesktopOutput>> {
    let monitors: Vec<HyprMonitor> =
        serde_json::from_str(monitors).context("hyprctl returned invalid monitor JSON")?;
    Ok(monitors
        .into_iter()
        .map(|monitor| DesktopOutput {
            id: monitor.name.clone(),
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

async fn hypr_instances() -> Result<Vec<HyprInstance>> {
    let output = Command::new("hyprctl")
        .args(["instances", "-j"])
        .output()
        .await
        .context("could not enumerate Hyprland instances")?;
    if !output.status.success() {
        anyhow::bail!(
            "hyprctl instances failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("hyprctl returned invalid instance JSON")
}

async fn hyprctl_instance(signature: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("hyprctl")
        .args(["-i", signature])
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

async fn wake_physical_displays(headless_output: &str) -> Result<DisplayWakeResponse> {
    let instances = hypr_instances().await?;
    if instances.is_empty() {
        anyhow::bail!("no logged-in Hyprland session is available");
    }

    let hold_seconds = start_idle_hold().await;
    let mut woken = Vec::new();
    let mut errors = Vec::new();
    for instance in instances {
        let monitors = match hyprctl_instance(&instance.instance, &["monitors", "all", "-j"]).await
        {
            Ok(monitors) => monitors,
            Err(error) => {
                errors.push(error.to_string());
                continue;
            }
        };
        let outputs = match parse_outputs(&monitors, headless_output) {
            Ok(outputs) => outputs,
            Err(error) => {
                errors.push(error.to_string());
                continue;
            }
        };
        for output in outputs.into_iter().filter(|output| !output.virtual_output) {
            match hyprctl_instance(
                &instance.instance,
                &["dispatch", "dpms", "on", &output.name],
            )
            .await
            {
                Ok(_) => woken.push(output.name),
                Err(error) => errors.push(error.to_string()),
            }
        }
    }

    if woken.is_empty() {
        let detail = errors
            .first()
            .map(String::as_str)
            .unwrap_or("no physical outputs found");
        anyhow::bail!("no physical Hyprland display could be woken: {detail}");
    }

    woken.sort();
    woken.dedup();
    let locked = Command::new("pidof")
        .arg("hyprlock")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success());
    info!(outputs = ?woken, locked, hold_seconds, "physical displays woken remotely");
    Ok(DisplayWakeResponse {
        woken,
        locked,
        hold_seconds,
    })
}

async fn start_idle_hold() -> u64 {
    let child = Command::new("systemd-inhibit")
        .args([
            "--what=idle",
            "--mode=block",
            "--why=EutherGate remote screen wake",
            "sleep",
            &DISPLAY_WAKE_HOLD_SECONDS.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match child {
        Ok(_) => DISPLAY_WAKE_HOLD_SECONDS,
        Err(error) => {
            warn!(%error, "could not start temporary idle inhibitor; displays were still woken");
            0
        }
    }
}

impl BrowserManager {
    async fn sessions(&self) -> Result<Vec<BrowserSessionInfo>> {
        let forge = read_forge_session(&self.forge_session_file)?;
        let raw = swayctl(&forge.sway_socket, &["-t", "get_tree", "-r"]).await?;
        parse_browser_sessions(&raw)
    }

    async fn launch(&self) -> Result<BrowserSessionInfo> {
        let _transition = self.transition.lock().await;
        let firefox = self
            .firefox
            .as_ref()
            .context("Firefox is unavailable on the EutherGate host")?;
        let existing = self.sessions().await?;
        if existing.len() >= 4 {
            anyhow::bail!("EutherBrowse is limited to four open Firefox windows");
        }
        let existing_ids: HashSet<u64> = existing.iter().map(|session| session.id).collect();
        let workspace = existing
            .iter()
            .filter_map(|session| session.workspace.parse::<u32>().ok())
            .max()
            .map(|value| value.saturating_add(1))
            .unwrap_or(BROWSER_WORKSPACE_START)
            .max(BROWSER_WORKSPACE_START);
        let forge = read_forge_session(&self.forge_session_file)?;
        let workspace = workspace.to_string();
        swayctl(&forge.sway_socket, &["workspace", "number", &workspace]).await?;
        let mut arguments = Vec::new();
        if existing.is_empty() {
            arguments.push("--new-instance");
        }
        arguments.extend([
            "--profile",
            self.profile_dir
                .to_str()
                .context("Firefox profile path is not UTF-8")?,
            "--new-window",
            &self.start_url,
        ]);
        let command = format!(
            "env MOZ_ENABLE_WAYLAND=1 {} {}",
            shell_quote(&firefox.display().to_string()),
            arguments
                .iter()
                .map(|argument| shell_quote(argument))
                .collect::<Vec<_>>()
                .join(" ")
        );
        swayctl(&forge.sway_socket, &["exec", &command]).await?;

        for _ in 0..80 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let sessions = self.sessions().await?;
            if let Some(session) = sessions
                .into_iter()
                .find(|session| !existing_ids.contains(&session.id))
            {
                let session = self.focus(session.id).await?;
                tokio::time::sleep(Duration::from_millis(350)).await;
                self.navigate_to_start().await?;
                return Ok(session);
            }
        }
        anyhow::bail!("Firefox did not publish a new EutherBrowse window in time")
    }

    async fn focus(&self, id: u64) -> Result<BrowserSessionInfo> {
        let forge = read_forge_session(&self.forge_session_file)?;
        let sessions = self.sessions().await?;
        let session = sessions
            .into_iter()
            .find(|session| session.id == id)
            .with_context(|| format!("EutherBrowse session {id} does not exist"))?;
        let criteria = format!("[con_id=\"{id}\"]");
        swayctl(&forge.sway_socket, &[&criteria, "focus"]).await?;
        swayctl(&forge.sway_socket, &[&criteria, "fullscreen", "enable"]).await?;
        Ok(BrowserSessionInfo {
            focused: true,
            ..session
        })
    }

    async fn close(&self, id: u64) -> Result<()> {
        let _transition = self.transition.lock().await;
        let forge = read_forge_session(&self.forge_session_file)?;
        if !self
            .sessions()
            .await?
            .iter()
            .any(|session| session.id == id)
        {
            anyhow::bail!("EutherBrowse session {id} does not exist");
        }
        let criteria = format!("[con_id=\"{id}\"]");
        swayctl(&forge.sway_socket, &[&criteria, "kill"]).await?;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if !self
                .sessions()
                .await?
                .iter()
                .any(|session| session.id == id)
            {
                return Ok(());
            }
        }
        anyhow::bail!("Firefox window {id} did not close in time")
    }

    async fn navigate_to_start(&self) -> Result<()> {
        let forge = read_forge_session(&self.forge_session_file)?;
        let typed = Command::new("wtype")
            .env("WAYLAND_DISPLAY", &forge.wayland_display)
            .env("SWAYSOCK", &forge.sway_socket)
            .args(["-M", "ctrl", "-k", "l", "-m", "ctrl", "-s", "100", "--"])
            .arg(&self.start_url)
            .output()
            .await
            .context("could not type the fixed EutherBrowse start URL")?;
        if !typed.status.success() {
            anyhow::bail!(
                "wtype could not enter the EutherBrowse start URL: {}",
                String::from_utf8_lossy(&typed.stderr).trim()
            );
        }
        let entered = Command::new("wtype")
            .env("WAYLAND_DISPLAY", &forge.wayland_display)
            .env("SWAYSOCK", &forge.sway_socket)
            .args(["-k", "Return"])
            .output()
            .await
            .context("could not submit the fixed EutherBrowse start URL")?;
        if !entered.status.success() {
            anyhow::bail!(
                "wtype could not submit the EutherBrowse start URL: {}",
                String::from_utf8_lossy(&entered.stderr).trim()
            );
        }
        Ok(())
    }
}

fn parse_browser_sessions(raw: &str) -> Result<Vec<BrowserSessionInfo>> {
    let root: SwayTreeNode =
        serde_json::from_str(raw).context("swaymsg returned invalid tree JSON")?;
    let mut sessions = Vec::new();
    collect_browser_sessions(&root, "", &mut sessions);
    sessions.sort_by(|left, right| {
        browser_workspace_number(&left.workspace)
            .cmp(&browser_workspace_number(&right.workspace))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(sessions)
}

fn collect_browser_sessions(
    node: &SwayTreeNode,
    parent_workspace: &str,
    sessions: &mut Vec<BrowserSessionInfo>,
) {
    let workspace = if node.node_type == "workspace" {
        node.name.as_deref().unwrap_or(parent_workspace)
    } else {
        parent_workspace
    };
    if node.app_id.as_deref().is_some_and(is_firefox_app_id) {
        sessions.push(BrowserSessionInfo {
            id: node.id,
            title: node
                .name
                .as_deref()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or("Firefox")
                .to_owned(),
            workspace: workspace.to_owned(),
            focused: node.focused,
        });
    }
    for child in node.nodes.iter().chain(&node.floating_nodes) {
        collect_browser_sessions(child, workspace, sessions);
    }
}

fn is_firefox_app_id(app_id: &str) -> bool {
    matches!(app_id, "firefox" | "org.mozilla.firefox")
}

fn browser_workspace_number(workspace: &str) -> u32 {
    workspace.parse().unwrap_or(u32::MAX)
}

async fn swayctl(socket: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("swaymsg")
        .env("SWAYSOCK", socket)
        .args(args)
        .output()
        .await
        .with_context(|| format!("could not run swaymsg {}", args.join(" ")))?;
    if !output.status.success() {
        anyhow::bail!(
            "swaymsg {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn clipboard_command(selection: &DesktopSelection, program: &str) -> Result<Command> {
    let mut command = Command::new(program);
    match &selection.backend {
        DesktopBackend::Hyprland {
            signature,
            wayland_display,
        } => {
            command
                .env("WAYLAND_DISPLAY", wayland_display)
                .env("HYPRLAND_INSTANCE_SIGNATURE", signature);
        }
        DesktopBackend::Sway {
            wayland_display,
            sway_socket,
        } => {
            command
                .env("WAYLAND_DISPLAY", wayland_display)
                .env("SWAYSOCK", sway_socket);
        }
        DesktopBackend::Unavailable => anyhow::bail!("selected Wayland session is unavailable"),
    }
    Ok(command)
}

fn choose_clipboard_mime(types: &str) -> Option<(String, String)> {
    let offered: Vec<&str> = types
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    for mime in ["image/png", "image/jpeg", "image/webp"] {
        if let Some(found) = offered
            .iter()
            .find(|offered| offered.eq_ignore_ascii_case(mime))
        {
            return Some(((*found).to_owned(), mime.to_owned()));
        }
    }
    for mime in [
        "text/plain;charset=utf-8",
        "text/plain",
        "UTF8_STRING",
        "STRING",
    ] {
        if let Some(found) = offered
            .iter()
            .find(|offered| offered.eq_ignore_ascii_case(mime))
        {
            return Some(((*found).to_owned(), "text/plain;charset=utf-8".into()));
        }
    }
    None
}

fn supported_upload_mime(content_type: &str) -> Option<&'static str> {
    match content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "text/plain" => Some("text/plain;charset=utf-8"),
        "image/png" => Some("image/png"),
        "image/jpeg" => Some("image/jpeg"),
        "image/webp" => Some("image/webp"),
        _ => None,
    }
}

fn terminal_image_format(content_type: &str) -> Option<(&'static str, &'static str)> {
    match content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/png" => Some(("image/png", "png")),
        "image/jpeg" => Some(("image/jpeg", "jpg")),
        "image/webp" => Some(("image/webp", "webp")),
        _ => None,
    }
}

fn valid_image_signature(mime: &str, data: &[u8]) -> bool {
    match mime {
        "image/png" => data.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => data.starts_with(b"\xff\xd8\xff"),
        "image/webp" => data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP",
        _ => false,
    }
}

fn prepare_private_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .with_context(|| format!("could not create {}", path.display()))?;
    }
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("could not inspect {}", path.display()))?;
    if !metadata.file_type().is_dir() {
        anyhow::bail!(
            "{} must be a directory, not a symlink or file",
            path.display()
        );
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("could not secure {}", path.display()))?;
    Ok(())
}

fn prepare_firefox_profile(path: &Path) -> Result<()> {
    let preferences = path.join("user.js");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&preferences)
        .with_context(|| format!("could not prepare {}", preferences.display()))?;
    std::fs::set_permissions(&preferences, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("could not secure {}", preferences.display()))?;
    file.write_all(
        br#"user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.startup.page", 0);
user_pref("browser.tabs.warnOnClose", false);
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("extensions.enabledScopes", 1);
user_pref("toolkit.telemetry.reportingpolicy.firstRun", false);
"#,
    )
    .with_context(|| format!("could not initialize {}", preferences.display()))?;
    Ok(())
}

fn read_forge_session(path: &Path) -> Result<ForgeSession> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let value = |name: &str| -> Result<String> {
        contents
            .lines()
            .find_map(|line| line.split_once('=').filter(|(key, _)| *key == name))
            .map(|(_, value)| value.to_owned())
            .filter(|value| !value.is_empty() && !value.contains(char::is_whitespace))
            .with_context(|| format!("{name} missing from {}", path.display()))
    };
    if value("BACKEND")? != "sway" {
        anyhow::bail!("unsupported Forge compositor backend");
    }
    Ok(ForgeSession {
        wayland_display: value("WAYLAND_DISPLAY")?,
        sway_socket: value("SWAYSOCK")?,
        output: value("OUTPUT")?,
    })
}

async fn desktop_socket(socket: WebSocket, desktop: Arc<DesktopManager>) -> Result<()> {
    let selection = desktop.selection();
    let (backend_name, wayland_display) = match &selection.backend {
        DesktopBackend::Hyprland {
            signature,
            wayland_display,
        } => (
            "hyprland",
            (
                wayland_display,
                Some(("HYPRLAND_INSTANCE_SIGNATURE", signature)),
            ),
        ),
        DesktopBackend::Sway {
            wayland_display,
            sway_socket,
        } => ("sway", (wayland_display, Some(("SWAYSOCK", sway_socket)))),
        DesktopBackend::Unavailable => anyhow::bail!("selected Wayland session is unavailable"),
    };
    let mut command = Command::new("python");
    command
        .arg(&desktop.helper)
        .args([
            "--backend",
            backend_name,
            "--output",
            &selection.capture_output,
            "--mode",
            &selection.mode,
        ])
        .env("WAYLAND_DISPLAY", wayland_display.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    if let Some((name, value)) = wayland_display.1 {
        command.env(name, value);
    }
    let mut child = command
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

async fn desktop_fallback_socket(socket: WebSocket, desktop: Arc<DesktopManager>) -> Result<()> {
    let selection = desktop.selection();
    let (backend_name, wayland_display, backend_environment) = match &selection.backend {
        DesktopBackend::Hyprland {
            signature,
            wayland_display,
        } => (
            "hyprland",
            wayland_display,
            Some(("HYPRLAND_INSTANCE_SIGNATURE", signature)),
        ),
        DesktopBackend::Sway {
            wayland_display,
            sway_socket,
        } => ("sway", wayland_display, Some(("SWAYSOCK", sway_socket))),
        DesktopBackend::Unavailable => anyhow::bail!("selected Wayland session is unavailable"),
    };
    let mut command = Command::new("python");
    command
        .arg(&desktop.fallback_helper)
        .args([
            "--backend",
            backend_name,
            "--output",
            &selection.capture_output,
            "--mode",
            &selection.mode,
        ])
        .env("WAYLAND_DISPLAY", wayland_display)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    if let Some((name, value)) = backend_environment {
        command.env(name, value);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("could not start {}", desktop.fallback_helper.display()))?;
    let mut child_input = child
        .stdin
        .take()
        .context("HTTPS/WSS helper stdin unavailable")?;
    let child_output = child
        .stdout
        .take()
        .context("HTTPS/WSS helper stdout unavailable")?;
    let mut child_output = BufReader::new(child_output);
    let (mut sender, mut receiver) = socket.split();
    let mut heartbeat = tokio::time::interval(DESKTOP_HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_client_message = Instant::now();

    loop {
        tokio::select! {
            biased;
            _ = heartbeat.tick() => {
                if last_client_message.elapsed() >= DESKTOP_HEARTBEAT_TIMEOUT {
                    anyhow::bail!("HTTPS/WSS desktop viewer heartbeat timed out");
                }
            },
            packet = read_fallback_packet(&mut child_output) => match packet {
                Ok(Some((1, payload))) => {
                    match timeout(DESKTOP_SEND_TIMEOUT, sender.send(Message::Binary(payload.into()))).await {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) | Err(_) => break,
                    }
                }
                Ok(Some((2, payload))) => {
                    let text = String::from_utf8(payload).context("fallback helper returned invalid JSON text")?;
                    match timeout(DESKTOP_SEND_TIMEOUT, sender.send(Message::Text(text.into()))).await {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) | Err(_) => break,
                    }
                }
                Ok(Some((kind, _))) => anyhow::bail!("fallback helper returned unknown packet type {kind}"),
                Ok(None) => break,
                Err(error) => return Err(error).context("could not read HTTPS/WSS helper output"),
            },
            message = receiver.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    last_client_message = Instant::now();
                    if text.as_str() == "{\"type\":\"heartbeat\"}" {
                        continue;
                    }
                    child_input.write_all(text.as_bytes()).await?;
                    child_input.write_all(b"\n").await?;
                    child_input.flush().await?;
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    warn!(%error, "desktop HTTPS/WSS socket disconnected");
                    break;
                }
            },
            status = child.wait() => {
                let status = status.context("could not wait for HTTPS/WSS helper")?;
                anyhow::bail!("HTTPS/WSS helper exited with {status}");
            }
        }
    }

    let _ = child_input.write_all(b"{\"type\":\"stop\"}\n").await;
    drop(child_input);
    if tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
    }
    Ok(())
}

async fn desktop_vnc_socket(
    socket: WebSocket,
    desktop: Arc<DesktopManager>,
    profile: VncPerformanceProfile,
) -> Result<()> {
    let selection = desktop.selection();
    let (wayland_display, backend_environment) = match &selection.backend {
        DesktopBackend::Hyprland {
            signature,
            wayland_display,
        } => (
            wayland_display,
            Some(("HYPRLAND_INSTANCE_SIGNATURE", signature)),
        ),
        DesktopBackend::Sway {
            wayland_display,
            sway_socket,
        } => (wayland_display, Some(("SWAYSOCK", sway_socket))),
        DesktopBackend::Unavailable => anyhow::bail!("selected Wayland session is unavailable"),
    };
    let wayvnc = desktop
        .wayvnc
        .as_ref()
        .context("WayVNC executable is unavailable")?;
    let _idle_hold = start_vnc_idle_hold();
    wake_vnc_output(&selection).await?;
    let socket_id = random_secret(12);
    let rfb_socket = env::temp_dir().join(format!("euthergate-vnc-{socket_id}.sock"));
    let control_socket = env::temp_dir().join(format!("euthergate-vncctl-{socket_id}.sock"));
    let _socket_cleanup = VncSocketCleanup {
        rfb_socket: rfb_socket.clone(),
        control_socket: control_socket.clone(),
    };
    let mut command = Command::new(wayvnc);
    command
        .args(["--exit-on-disconnect", "--unix-socket"])
        .arg("--max-fps")
        .arg(profile.max_fps())
        .arg("--output")
        .arg(&selection.capture_output)
        .arg("--socket")
        .arg(&control_socket)
        .arg("--name")
        .arg("EutherGate Forge")
        .arg("--keyboard")
        .arg(&desktop.vnc_keyboard);
    if profile.gpu() {
        command.arg("--gpu");
    }
    command
        .arg(&rfb_socket)
        .env("WAYLAND_DISPLAY", wayland_display)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    if let Some((name, value)) = backend_environment {
        command.env(name, value);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("could not start {}", wayvnc.display()))?;
    info!(
        profile = profile.id(),
        max_fps = profile.max_fps(),
        gpu = profile.gpu(),
        output = %selection.capture_output,
        "VNC performance profile started"
    );

    let connect_result = timeout(Duration::from_secs(4), async {
        loop {
            match UnixStream::connect(&rfb_socket).await {
                Ok(stream) => return Ok(stream),
                Err(error) => {
                    if let Some(status) = child.try_wait().context("could not inspect WayVNC")? {
                        anyhow::bail!("WayVNC exited with {status}: {error}");
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        }
    })
    .await;
    let stream = match connect_result {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            let _ = child.kill().await;
            remove_vnc_sockets(&rfb_socket, &control_socket);
            return Err(error);
        }
        Err(_) => {
            let _ = child.kill().await;
            remove_vnc_sockets(&rfb_socket, &control_socket);
            anyhow::bail!("WayVNC did not create its private socket within four seconds");
        }
    };

    let (mut vnc_reader, mut vnc_writer) = stream.into_split();
    let (mut sender, mut receiver) = socket.split();
    let mut buffer = vec![0_u8; 256 * 1024];
    loop {
        tokio::select! {
            read = vnc_reader.read(&mut buffer) => match read {
                Ok(0) => break,
                Ok(length) => {
                    if sender.send(Message::Binary(buffer[..length].to_vec().into())).await.is_err() {
                        break;
                    }
                }
                Err(error) => return Err(error).context("could not read private WayVNC socket"),
            },
            message = receiver.next() => match message {
                Some(Ok(Message::Binary(data))) => {
                    vnc_writer.write_all(&data).await?;
                    vnc_writer.flush().await?;
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    warn!(%error, "desktop VNC WebSocket disconnected");
                    break;
                }
            },
            status = child.wait() => {
                let status = status.context("could not wait for WayVNC")?;
                anyhow::bail!("WayVNC exited with {status}");
            }
        }
    }

    drop(vnc_writer);
    if timeout(Duration::from_secs(2), child.wait()).await.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    remove_vnc_sockets(&rfb_socket, &control_socket);
    Ok(())
}

fn start_vnc_idle_hold() -> Option<tokio::process::Child> {
    match Command::new("systemd-inhibit")
        .args([
            "--what=idle",
            "--mode=block",
            "--why=EutherGate active VNC desktop session",
            "sleep",
            "infinity",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => Some(child),
        Err(error) => {
            warn!(%error, "could not inhibit display idle during VNC session");
            None
        }
    }
}

async fn wake_vnc_output(selection: &DesktopSelection) -> Result<()> {
    match &selection.backend {
        DesktopBackend::Hyprland { signature, .. } => {
            hyprctl_instance(
                signature,
                &["dispatch", "dpms", "on", &selection.capture_output],
            )
            .await
            .with_context(|| {
                format!(
                    "could not wake Hyprland output {} for VNC capture",
                    selection.capture_output
                )
            })?;
        }
        DesktopBackend::Sway { sway_socket, .. } => {
            swayctl(
                sway_socket,
                &["output", &selection.capture_output, "power", "on"],
            )
            .await
            .with_context(|| {
                format!(
                    "could not wake Sway output {} for VNC capture",
                    selection.capture_output
                )
            })?;
        }
        DesktopBackend::Unavailable => {
            anyhow::bail!("selected Wayland session is unavailable");
        }
    }
    info!(output = %selection.capture_output, "VNC output awake with idle inhibited");
    Ok(())
}

fn remove_vnc_sockets(rfb_socket: &Path, control_socket: &Path) {
    for path in [rfb_socket, control_socket] {
        if let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(path = %path.display(), %error, "could not remove private WayVNC socket");
        }
    }
}

struct VncSocketCleanup {
    rfb_socket: PathBuf,
    control_socket: PathBuf,
}

impl Drop for VncSocketCleanup {
    fn drop(&mut self) {
        remove_vnc_sockets(&self.rfb_socket, &self.control_socket);
    }
}

async fn read_fallback_packet<R>(reader: &mut R) -> Result<Option<(u8, Vec<u8>)>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut header = [0_u8; FALLBACK_PACKET_HEADER_BYTES];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let length = u32::from_be_bytes(header[1..5].try_into().expect("four-byte length")) as usize;
    if length > FALLBACK_MAX_PACKET_BYTES {
        anyhow::bail!("fallback packet exceeds {FALLBACK_MAX_PACKET_BYTES} bytes");
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(Some((header[0], payload)))
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

fn new_local_terminal_session_name() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("local-{timestamp}-{}", random_secret(4))
}

impl TurnConfig {
    fn ice_server(&self) -> Option<IceServer> {
        let expires = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs()
            .checked_add(TURN_CREDENTIAL_TTL_SECONDS)?;
        let username = format!("{expires}:euthergate");
        let mut mac = Hmac::<Sha1>::new_from_slice(self.shared_secret.as_bytes()).ok()?;
        mac.update(username.as_bytes());
        let credential = STANDARD.encode(mac.finalize().into_bytes());
        Some(IceServer {
            urls: self.urls.clone(),
            username,
            credential,
        })
    }
}

fn transport_profiles(turn: Option<&TurnConfig>, wayvnc_available: bool) -> Vec<TransportProfile> {
    let all_urls = turn.map(|config| config.urls.clone()).unwrap_or_default();
    let mut profiles = vec![
        TransportProfile {
            id: "auto",
            label: "AUTO",
            description: "Direct WebRTC first, with every configured relay available as fallback.",
            ice_transport_policy: "all",
            urls: all_urls,
        },
        TransportProfile {
            id: "direct",
            label: "DIRECT / LAN",
            description: "Direct WebRTC only. Intended for the same LAN or a trusted VPN.",
            ice_transport_policy: "all",
            urls: Vec::new(),
        },
        TransportProfile {
            id: "https-wss",
            label: "WORK · HTTPS/WSS",
            description: "JPEG desktop frames and input over the authenticated HTTPS WebSocket path.",
            ice_transport_policy: "all",
            urls: Vec::new(),
        },
    ];

    if wayvnc_available {
        profiles.push(TransportProfile {
            id: "vnc-wss",
            label: "WORK · VNC/WSS",
            description: "WayVNC changed regions and input over the authenticated HTTPS WebSocket path.",
            ice_transport_policy: "all",
            urls: Vec::new(),
        });
    }

    let Some(turn) = turn else {
        return profiles;
    };

    type UrlMatcher = fn(&str) -> bool;
    let definitions: [(&str, &str, &str, UrlMatcher); 4] = [
        (
            "turn-tls-443",
            "WORK · TURN/TLS 443",
            "Relay-only TURN secured with TLS over TCP port 443.",
            |url: &str| {
                let url = url.to_ascii_lowercase();
                url.starts_with("turns:") && url.contains(":443") && url.contains("transport=tcp")
            },
        ),
        (
            "turn-udp-443",
            "TURN/UDP 443",
            "Relay-only TURN over UDP port 443; usually the fastest relay path.",
            |url: &str| {
                let url = url.to_ascii_lowercase();
                url.starts_with("turn:") && url.contains(":443") && url.contains("transport=udp")
            },
        ),
        (
            "turn-tcp-3478",
            "TURN/TCP 3478",
            "Relay-only TURN over TCP on the standard TURN port.",
            |url: &str| {
                let url = url.to_ascii_lowercase();
                url.starts_with("turn:") && url.contains(":3478") && url.contains("transport=tcp")
            },
        ),
        (
            "turn-udp-3478",
            "TURN/UDP 3478",
            "Relay-only TURN over UDP on the standard TURN port.",
            |url: &str| {
                let url = url.to_ascii_lowercase();
                url.starts_with("turn:") && url.contains(":3478") && url.contains("transport=udp")
            },
        ),
    ];

    for (id, label, description, matches) in definitions {
        let urls = turn
            .urls
            .iter()
            .filter(|url| matches(url))
            .cloned()
            .collect::<Vec<_>>();
        if !urls.is_empty() {
            profiles.push(TransportProfile {
                id,
                label,
                description,
                ice_transport_policy: "relay",
                urls,
            });
        }
    }

    profiles
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

fn valid_tmux_socket_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_terminal_session_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 32
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        anyhow::bail!(
            "terminal session names must be 1-32 letters, numbers, underscores or dashes"
        );
    }
    Ok(())
}

fn parse_terminal_sessions(value: &str) -> Result<Vec<TerminalSessionInfo>> {
    let mut sessions = Vec::new();
    for line in value.lines() {
        let mut fields = line.splitn(6, '\t');
        let Some(name) = fields.next() else { continue };
        let Some(windows) = fields.next() else {
            anyhow::bail!("tmux returned a malformed session row");
        };
        let Some(attached) = fields.next() else {
            anyhow::bail!("tmux returned a malformed session row");
        };
        let Some(activity) = fields.next() else {
            anyhow::bail!("tmux returned a malformed session row");
        };
        let Some(command) = fields.next() else {
            anyhow::bail!("tmux returned a malformed session row");
        };
        let Some(path) = fields.next() else {
            anyhow::bail!("tmux returned a malformed session row");
        };
        if validate_terminal_session_name(name).is_err() {
            warn!(session = name, "ignored tmux session with unsupported name");
            continue;
        }
        sessions.push(TerminalSessionInfo {
            name: name.to_owned(),
            windows: windows
                .parse()
                .context("tmux returned an invalid window count")?,
            attached: attached
                .parse()
                .context("tmux returned an invalid attached-client count")?,
            other_clients: attached
                .parse()
                .context("tmux returned an invalid attached-client count")?,
            activity: activity
                .parse()
                .context("tmux returned an invalid session activity time")?,
            command: command.to_owned(),
            path: path.to_owned(),
        });
    }
    sessions.sort_by(|left, right| {
        (left.name != DEFAULT_TERMINAL_SESSION, &left.name)
            .cmp(&(right.name != DEFAULT_TERMINAL_SESSION, &right.name))
    });
    Ok(sessions)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn resolve_executable(path: &Path) -> Option<PathBuf> {
    if path.components().count() > 1 {
        return is_executable(path).then(|| path.to_path_buf());
    }
    env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join(path))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn nix_uid() -> u32 {
    std::fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .unwrap_or(1000)
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
    fn turn_credentials_are_ephemeral_and_hmac_authenticated() {
        let turn = TurnConfig {
            urls: vec!["turns:turn.example.test:443?transport=tcp".into()],
            shared_secret: Arc::from("shared-secret"),
        };
        let server = turn.ice_server().unwrap();
        assert_eq!(server.urls, turn.urls);
        assert!(server.username.ends_with(":euthergate"));
        assert!(!server.credential.is_empty());
        assert!(!server.credential.contains("shared-secret"));
    }

    #[test]
    fn transport_profiles_separate_each_configured_turn_route() {
        let turn = TurnConfig {
            urls: vec![
                "turns:turn.example.test:443?transport=tcp".into(),
                "turn:turn.example.test:443?transport=udp".into(),
                "turn:turn.example.test:3478?transport=tcp".into(),
                "turn:turn.example.test:3478?transport=udp".into(),
            ],
            shared_secret: Arc::from("shared-secret"),
        };
        let profiles = transport_profiles(Some(&turn), true);
        assert_eq!(profiles[0].id, "auto");
        assert_eq!(profiles[0].urls, turn.urls);
        assert_eq!(profiles[1].id, "direct");
        assert!(profiles[1].urls.is_empty());
        assert_eq!(profiles[2].id, "https-wss");
        assert!(profiles[2].urls.is_empty());
        assert_eq!(profiles[3].id, "vnc-wss");
        assert!(profiles[3].urls.is_empty());
        for profile in &profiles[4..] {
            assert_eq!(profile.ice_transport_policy, "relay");
            assert_eq!(profile.urls.len(), 1);
        }
        assert_eq!(profiles[4].id, "turn-tls-443");
        assert!(profiles[4].urls[0].starts_with("turns:"));
        assert_eq!(profiles[5].id, "turn-udp-443");
        assert_eq!(profiles[6].id, "turn-tcp-3478");
        assert_eq!(profiles[7].id, "turn-udp-3478");
    }

    #[test]
    fn vnc_profile_only_appears_when_wayvnc_is_available() {
        assert!(
            transport_profiles(None, false)
                .iter()
                .all(|profile| profile.id != "vnc-wss")
        );
        assert!(
            transport_profiles(None, true)
                .iter()
                .any(|profile| profile.id == "vnc-wss")
        );
    }

    #[test]
    fn vnc_performance_profiles_are_strict_and_bounded() {
        let compatible = VncPerformanceProfile::parse(None).unwrap();
        assert_eq!(compatible, VncPerformanceProfile::Compatible);
        assert_eq!(compatible.max_fps(), "30");
        assert!(!compatible.gpu());

        let smooth = VncPerformanceProfile::parse(Some("smooth")).unwrap();
        assert_eq!(smooth.max_fps(), "60");
        assert!(!smooth.gpu());

        let gpu = VncPerformanceProfile::parse(Some("gpu")).unwrap();
        assert_eq!(gpu.max_fps(), "60");
        assert!(gpu.gpu());
        assert!(VncPerformanceProfile::parse(Some("120")).is_none());
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
    fn remote_service_restart_is_strictly_allowlisted() {
        assert_eq!(
            restart_unit_for_service("gateway"),
            Some("euthergate.service")
        );
        assert_eq!(
            restart_unit_for_service("tunnel"),
            Some("euthergate-tunnel.service")
        );
        assert_eq!(
            restart_unit_for_service("forge"),
            Some("euthergate-forge.service")
        );
        assert_eq!(restart_unit_for_service("eutherhost"), None);
        assert_eq!(restart_unit_for_service("../../anything"), None);
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

    #[test]
    fn forge_session_file_is_parsed_without_shell_evaluation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.env");
        std::fs::write(
            &path,
            "BACKEND=sway\nWAYLAND_DISPLAY=wayland-2\nSWAYSOCK=/run/user/1000/sway.sock\nOUTPUT=HEADLESS-1\n",
        )
        .unwrap();
        let session = read_forge_session(&path).unwrap();
        assert_eq!(session.wayland_display, "wayland-2");
        assert_eq!(session.sway_socket, "/run/user/1000/sway.sock");
        assert_eq!(session.output, "HEADLESS-1");

        std::fs::write(
            &path,
            "BACKEND=sway\nWAYLAND_DISPLAY=$(touch /tmp/nope)\nSWAYSOCK=x\nOUTPUT=y\n",
        )
        .unwrap();
        assert!(read_forge_session(&path).is_err());
    }

    #[test]
    fn clipboard_prefers_images_then_plain_text() {
        assert_eq!(
            choose_clipboard_mime("text/plain\nimage/png\ntext/html\n"),
            Some(("image/png".into(), "image/png".into()))
        );
        assert_eq!(
            choose_clipboard_mime("text/html\nUTF8_STRING\n"),
            Some(("UTF8_STRING".into(), "text/plain;charset=utf-8".into()))
        );
        assert_eq!(choose_clipboard_mime("text/html\nimage/gif\n"), None);
    }

    #[test]
    fn clipboard_uploads_accept_only_bounded_formats() {
        assert_eq!(
            supported_upload_mime("text/plain; charset=UTF-8"),
            Some("text/plain;charset=utf-8")
        );
        assert_eq!(supported_upload_mime("IMAGE/PNG"), Some("image/png"));
        assert_eq!(supported_upload_mime("image/svg+xml"), None);
        assert_eq!(supported_upload_mime("application/octet-stream"), None);
    }

    #[test]
    fn terminal_image_uploads_require_supported_magic_bytes() {
        assert_eq!(
            terminal_image_format("image/png"),
            Some(("image/png", "png"))
        );
        assert_eq!(
            terminal_image_format("IMAGE/JPEG; charset=binary"),
            Some(("image/jpeg", "jpg"))
        );
        assert_eq!(terminal_image_format("image/gif"), None);
        assert!(valid_image_signature("image/png", b"\x89PNG\r\n\x1a\nrest"));
        assert!(valid_image_signature(
            "image/webp",
            b"RIFF\x04\x00\x00\x00WEBP"
        ));
        assert!(!valid_image_signature("image/jpeg", b"not a jpeg"));
    }

    #[test]
    fn terminal_image_directory_is_private() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("nested").join("terminal-images");
        prepare_private_directory(&path).unwrap();
        assert!(path.is_dir());
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn terminal_session_names_are_strictly_bounded() {
        for valid in ["gate", "WaylandForge", "euther_drive-2"] {
            assert!(validate_terminal_session_name(valid).is_ok());
        }
        for invalid in ["", "two words", "../escape", "name:window", "åäö"] {
            assert!(validate_terminal_session_name(invalid).is_err());
        }
        assert!(validate_terminal_session_name(&"a".repeat(33)).is_err());
        let local = new_local_terminal_session_name();
        assert!(local.starts_with("local-"));
        assert!(validate_terminal_session_name(&local).is_ok());
    }

    #[test]
    fn tmux_session_rows_are_parsed_and_gate_is_first() {
        let sessions = parse_terminal_sessions(
            "work\t2\t1\t1720000000\tcargo\t/home/nichlas/work\ngate\t1\t0\t1720000001\tcodex\t/home/nichlas/EutherGate\n",
        )
        .unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, "gate");
        assert_eq!(sessions[0].windows, 1);
        assert_eq!(sessions[0].command, "codex");
        assert_eq!(sessions[1].name, "work");
        assert_eq!(sessions[1].attached, 1);
        assert_eq!(sessions[1].other_clients, 1);
        assert_eq!(sessions[1].path, "/home/nichlas/work");
        assert!(parse_terminal_sessions("broken\trow\n").is_err());
    }

    #[test]
    fn browser_sessions_include_only_firefox_windows_and_keep_workspaces() {
        let sessions = parse_browser_sessions(
            r#"{
                "id": 1,
                "type": "root",
                "nodes": [
                    {
                        "id": 2,
                        "type": "workspace",
                        "name": "10",
                        "nodes": [
                            {"id": 20, "type": "con", "name": "ChatGPT — Mozilla Firefox", "app_id": "firefox", "focused": true},
                            {"id": 21, "type": "con", "name": "Forge", "app_id": "kitty", "focused": false}
                        ]
                    },
                    {
                        "id": 3,
                        "type": "workspace",
                        "name": "11",
                        "nodes": [
                            {"id": 22, "type": "con", "name": "OpenAI", "app_id": "org.mozilla.firefox", "focused": false}
                        ]
                    }
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, 20);
        assert_eq!(sessions[0].workspace, "10");
        assert!(sessions[0].focused);
        assert_eq!(sessions[1].id, 22);
        assert_eq!(sessions[1].workspace, "11");
        assert!(is_firefox_app_id("firefox"));
        assert!(!is_firefox_app_id("firefox-evil"));
    }

    #[test]
    fn desktop_terminal_arguments_are_shell_quoted() {
        assert_eq!(shell_quote("/usr/bin/tmux"), "'/usr/bin/tmux'");
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }
}
