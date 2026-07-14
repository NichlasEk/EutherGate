use std::{
    collections::VecDeque,
    env,
    io::{Read, Write},
    net::SocketAddr,
    path::PathBuf,
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
use tokio::sync::{Mutex, broadcast};
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
}

struct TerminalSession {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    _child: StdMutex<Box<dyn portable_pty::Child + Send + Sync>>,
    output: Arc<OutputRelay>,
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
    };

    let static_files = ServeDir::new(&config.web_root)
        .fallback(ServeFile::new(config.web_root.join("index.html")));
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/status", get(status))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/ws/terminal", get(terminal_ws))
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

        Ok(Self {
            bind,
            token,
            generated_token,
            secure_cookie,
            shell,
            workdir,
            web_root,
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
}
