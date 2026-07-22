import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import RFB from "@novnc/novnc";
import "@xterm/xterm/css/xterm.css";
import "./style.css";

type Status = {
  authenticated: boolean;
  terminal_ready: boolean;
  auth_mode: "none" | "gate_cookie" | "eutheroxide_proxy";
};

type TerminalSessionInfo = {
  name: string;
  windows: number;
  attached: number;
  other_clients: number;
  activity: number;
  command: string;
  path: string;
};

type DesktopStatus = {
  available: boolean;
  active: boolean;
  viewer_connected: boolean;
  output_id: string;
  output: string;
  mode: string;
  workspace: number;
  transport: string;
  input: string;
  virtual_output: boolean;
  outputs: DesktopOutput[];
  ice_servers: RTCIceServer[];
  transport_profiles: DesktopTransportProfile[];
};

type DesktopTransportProfile = {
  id: string;
  label: string;
  description: string;
  ice_transport_policy: RTCIceTransportPolicy;
  urls: string[];
};

type VncPerformanceProfile = {
  id: "compatible" | "smooth" | "gpu";
  label: string;
  description: string;
  fps: number;
  gpu: boolean;
};

type DesktopOutput = {
  id: string;
  name: string;
  description: string;
  mode: string;
  workspace: number;
  virtual_output: boolean;
};

type DisplayWakeResult = {
  woken: string[];
  locked: boolean;
  hold_seconds: number;
};

type ExtendedIceCandidate = RTCIceCandidate & {
  relayProtocol?: string;
  url?: string;
};

type IceCandidatePairStats = RTCStats & {
  localCandidateId: string;
  remoteCandidateId: string;
};

type DesktopTouchPoint = {
  startX: number;
  startY: number;
  clientX: number;
  clientY: number;
  startPanX: number;
  startPanY: number;
  moved: boolean;
  consumed: boolean;
};

type DesktopPinch = {
  distance: number;
  midpointX: number;
  midpointY: number;
  scale: number;
  panX: number;
  panY: number;
};

type VncDisplayBridge = {
  scale: number;
};

type VncSessionBridge = RFB & {
  clipViewport: boolean;
  dragViewport: boolean;
  _display?: VncDisplayBridge;
};

type VncGestureDetail = {
  type: string;
  magnitudeX?: number;
  magnitudeY?: number;
};

const appNode = document.querySelector<HTMLDivElement>("#app");
if (!appNode) throw new Error("Missing #app");
const app: HTMLDivElement = appNode;

const encoder = new TextEncoder();
const decoder = new TextDecoder();
let socket: WebSocket | null = null;
let terminal: Terminal | null = null;
let fitAddon: FitAddon | null = null;
const terminalSessionPreferenceKey = "euthergate.terminal-session";
let activeTerminalSession = loadTerminalSessionPreference();
let terminalSessionRefreshTimer: number | null = null;
let peer: RTCPeerConnection | null = null;
let inputChannel: RTCDataChannel | null = null;
let remoteCandidates: RTCIceCandidateInit[] = [];
let desktopIceServers: RTCIceServer[] = [];
let activeDesktopIceServers: RTCIceServer[] = [];
let desktopTransportProfiles: DesktopTransportProfile[] = [];
let desktopVideoReady = false;
let desktopFallbackActive = false;
let desktopVncActive = false;
let vnc: RFB | null = null;
let desktopFallbackFrameUrl: string | null = null;
let desktopNegotiationTimer: number | null = null;
let desktopVncRetryTimer: number | null = null;
let vncSuperHeld = false;
let desktopSuperHeld = false;
let desktopControlActive = false;
let desktopIceCandidates: string[] = [];
let desktopIceErrors: string[] = [];
let desktopSelectedRoute = "none";
let proxiedSession = false;
let clipboardPreviewUrl: string | null = null;
let remoteClipboardBlob: Blob | null = null;
const desktopTouches = new Map<number, DesktopTouchPoint>();
let desktopPinch: DesktopPinch | null = null;
let desktopViewScale = 1;
let desktopViewPanX = 0;
let desktopViewPanY = 0;
let desktopVncFitScale = 1;
let desktopVncPinchDistance = 1;
let desktopVncPinchStartScale = 1;
const desktopTouchReleaseTimers = new Set<number>();

const clipboardLimit = 8 * 1024 * 1024;
const transportPreferenceKey = "euthergate.transport-profile";
const vncProfilePreferenceKey = "euthergate.vnc-performance-profile";
const remoteSuperKeysym = 0xffeb;
const vncPerformanceProfiles: VncPerformanceProfile[] = [
  {
    id: "compatible",
    label: "VNC · 30 FPS",
    description: "Compatible RFB mode capped at 30 FPS.",
    fps: 30,
    gpu: false,
  },
  {
    id: "smooth",
    label: "VNC · 60 FPS",
    description: "RFB mode capped at 60 FPS; higher CPU and network use.",
    fps: 60,
    gpu: false,
  },
  {
    id: "gpu",
    label: "VNC · 60 FPS + GPU",
    description: "60 FPS with WayVNC GPU features and H.264 when the browser supports it.",
    fps: 60,
    gpu: true,
  },
];

const gateRoot = new URL("./", document.baseURI);

function gateUrl(path: string): URL {
  return new URL(path.replace(/^\//, ""), gateRoot);
}

function gateWebSocket(path: string): URL {
  const url = gateUrl(path);
  url.protocol = location.protocol === "https:" ? "wss:" : "ws:";
  return url;
}

type GateView = "login" | "terminal" | "desktop";

function gateShell(content: string, view: GateView = "login"): string {
  const cockpit = view !== "login";
  return `
    <main class="gate-shell${cockpit ? " gate-cockpit" : ""}">
      <header class="topbar">
        <a class="brand" href="${escapeHtml(gateRoot.pathname)}" aria-label="EutherGate home">
          <span class="brand-mark" aria-hidden="true"><i></i></span>
          <span><strong>Euther</strong>Gate</span>
        </a>
        <div class="topbar-context">
          ${cockpit ? `<span class="topbar-path">GATE CONTROL / ${view.toUpperCase()}</span>` : ""}
          <div class="gate-state"><span class="pulse"></span><span id="connection-label">LOCAL GATE</span></div>
        </div>
      </header>
      ${cockpit ? `
        <div class="cockpit-layout">
          <aside class="gate-sidebar" aria-label="Gate navigation">
            <div class="sidebar-section">
              <span class="sidebar-label">WORKSPACE</span>
              <button class="sidebar-link${view === "terminal" ? " is-active" : ""}" data-gate-view="terminal" type="button">
                <span>TERMINAL</span><small>Persistent shell</small>
              </button>
              <button class="sidebar-link${view === "desktop" ? " is-active" : ""}" data-gate-view="desktop" type="button">
                <span>DESKTOP</span><small>Remote Wayland</small>
              </button>
            </div>
            <div class="sidebar-section">
              <span class="sidebar-label">QUICK ACTIONS</span>
              <button class="sidebar-link sidebar-wake" type="button">
                <span>WAKE SCREENS</span><small>Preserves lock</small>
              </button>
            </div>
            <div class="sidebar-foot"><span class="pulse"></span> GATE ONLINE</div>
          </aside>
          <div class="gate-stage">${content}</div>
          <aside class="gate-rail" aria-label="Gate status">
            <section class="rail-card rail-status">
              <span class="rail-label">STATUS</span>
              <strong>GATE ONLINE</strong>
              <small>Encrypted local session</small>
            </section>
            <section class="rail-card">
              <span class="rail-label">ACTIVE VIEW</span>
              <strong>${view === "terminal" ? "FORGE SHELL" : "WAYLAND DESKTOP"}</strong>
              <small>${view === "terminal" ? "Persistent PTY channel" : "Live transport control"}</small>
            </section>
            <section class="rail-card rail-trace">
              <span class="rail-label">OXIDATIVE TRACE</span>
              <p>${view === "terminal" ? "Shell output remains attached across reloads." : "Transport diagnostics appear below the stream."}</p>
            </section>
          </aside>
        </div>` : content}
    </main>`;
}

function bindCockpitNavigation(): void {
  document.querySelector<HTMLButtonElement>('[data-gate-view="terminal"]')?.addEventListener("click", renderTerminal);
  document.querySelector<HTMLButtonElement>('[data-gate-view="desktop"]')?.addEventListener("click", renderDesktop);
  document.querySelector<HTMLButtonElement>(".sidebar-wake")?.addEventListener("click", wakeScreens);
}

function renderLogin(message = ""): void {
  disposeViews();
  app.innerHTML = gateShell(`
    <section class="login-wrap">
      <div class="eyebrow">REMOTE FORGE ENVIRONMENT</div>
      <h1>Open the gate.</h1>
      <p class="lede">One secure surface for your terminal, forge and remote Wayland session.</p>
      <form id="login-form" class="login-card">
        <label for="token">ACCESS TOKEN</label>
        <div class="token-row">
          <input id="token" name="token" type="password" autocomplete="current-password" autofocus required placeholder="Enter gate token" />
          <button type="submit">ENTER <span aria-hidden="true">&#8594;</span></button>
        </div>
        <p id="login-message" class="form-message" role="alert">${escapeHtml(message)}</p>
      </form>
      <div class="horizon" aria-hidden="true"><span></span></div>
    </section>`);

  document.querySelector<HTMLFormElement>("#login-form")?.addEventListener("submit", login);
}

function renderTerminal(): void {
  disposeViews();
  app.innerHTML = gateShell(`
    <section class="workspace">
      <div class="workspace-bar">
        <div>
          <span class="eyebrow">GATE SHELL / SESSION 01</span>
          <h1>Forge terminal</h1>
        </div>
        <div class="actions">
          <select id="terminal-session-picker" class="output-picker terminal-session-picker" aria-label="Terminal session">
            <option value="${escapeHtml(activeTerminalSession)}">${escapeHtml(activeTerminalSession)}</option>
          </select>
          <button id="terminal-session-new" class="ghost-button" type="button">+ SESSION</button>
          <button id="terminal-local-new" class="ghost-button" type="button">OPEN TERMINAL</button>
          <span id="socket-state" class="socket-state">CONNECTING</span>
          <button class="ghost-button wake-screens" type="button">WAKE SCREENS</button>
          <button id="terminal-image-button" class="ghost-button" type="button">PASTE IMAGE</button>
          <button id="show-desktop" class="ghost-button primary-action" type="button">DESKTOP</button>
          ${proxiedSession ? "" : '<button id="logout" class="ghost-button" type="button">CLOSE GATE</button>'}
        </div>
      </div>
      <div id="terminal-session-strip" class="terminal-session-strip" aria-label="Open terminal sessions"></div>
      <div class="terminal-frame">
        <div class="terminal-chrome"><span></span><span></span><span></span><b>euthergate://tmux/${escapeHtml(activeTerminalSession)}</b></div>
        <div id="terminal" aria-label="EutherGate terminal"></div>
      </div>
      <input id="terminal-image-input" type="file" accept="image/png,image/jpeg,image/webp" hidden />
      <p id="terminal-image-status" class="hint">Ctrl+V pastes clipboard images as a private file path. The shell remains alive when this page is reloaded.</p>
    </section>`, "terminal");

  terminal = new Terminal({
    cursorBlink: true,
    cursorStyle: "bar",
    fontFamily: '"JetBrains Mono", "SFMono-Regular", Consolas, monospace',
    fontSize: 14,
    lineHeight: 1.28,
    scrollback: 5000,
    theme: {
      background: "#090b0f",
      foreground: "#d8ded9",
      cursor: "#c7ff4a",
      selectionBackground: "#3d4f2b",
      black: "#11151a",
      brightBlack: "#606a69",
      green: "#9ed64a",
      brightGreen: "#c7ff4a",
      cyan: "#66c9c2",
      brightCyan: "#8cf0e7",
    },
  });
  fitAddon = new FitAddon();
  terminal.loadAddon(fitAddon);
  terminal.open(document.querySelector<HTMLDivElement>("#terminal")!);
  fitAddon.fit();

  const terminalNode = document.querySelector<HTMLDivElement>("#terminal")!;
  terminalNode.addEventListener("paste", pasteImageIntoTerminal, { capture: true });
  terminalNode.addEventListener("dragover", (event) => event.preventDefault());
  terminalNode.addEventListener("drop", dropImageIntoTerminal);

  terminal.onData((data) => {
    if (socket?.readyState === WebSocket.OPEN) socket.send(encoder.encode(data));
  });
  terminal.onResize(sendResize);
  window.addEventListener("resize", fitTerminal);
  document.querySelector<HTMLButtonElement>("#logout")?.addEventListener("click", logout);
  document.querySelector<HTMLButtonElement>(".wake-screens")?.addEventListener("click", wakeScreens);
  document.querySelector<HTMLButtonElement>("#terminal-image-button")?.addEventListener("click", chooseTerminalImage);
  document.querySelector<HTMLInputElement>("#terminal-image-input")?.addEventListener("change", selectTerminalImage);
  document.querySelector<HTMLButtonElement>("#show-desktop")?.addEventListener("click", renderDesktop);
  document.querySelector<HTMLSelectElement>("#terminal-session-picker")?.addEventListener("change", switchTerminalSession);
  document.querySelector<HTMLButtonElement>("#terminal-session-new")?.addEventListener("click", createTerminalSession);
  document.querySelector<HTMLButtonElement>("#terminal-local-new")?.addEventListener("click", openLocalTerminal);
  bindCockpitNavigation();
  void refreshTerminalSessions();
  terminalSessionRefreshTimer = window.setInterval(refreshTerminalSessions, 2500);
  connectSocket();
}

function loadTerminalSessionPreference(): string {
  try {
    const stored = window.localStorage.getItem(terminalSessionPreferenceKey) || "gate";
    return /^[A-Za-z0-9_-]{1,32}$/.test(stored) ? stored : "gate";
  } catch {
    return "gate";
  }
}

function saveTerminalSessionPreference(name: string): void {
  activeTerminalSession = name;
  try {
    window.localStorage.setItem(terminalSessionPreferenceKey, name);
  } catch {
    // Private browsing may disable storage; the in-memory selection still works.
  }
}

async function refreshTerminalSessions(): Promise<void> {
  const picker = document.querySelector<HTMLSelectElement>("#terminal-session-picker");
  if (!picker) return;
  try {
    const response = await fetch(gateUrl("api/terminal/sessions"));
    const body = (await response.json()) as { sessions?: TerminalSessionInfo[]; error?: string };
    if (!response.ok) throw new Error(body.error || "Could not list terminal sessions.");
    const sessions = (body.sessions || []).sort((left, right) => {
      if (left.name === "gate") return -1;
      if (right.name === "gate") return 1;
      return right.activity - left.activity || left.name.localeCompare(right.name);
    });
    if (!sessions.some((session) => session.name === activeTerminalSession)) {
      sessions.unshift({
        name: activeTerminalSession,
        windows: 1,
        attached: 0,
        other_clients: 0,
        activity: 0,
        command: "shell",
        path: "",
      });
    }
    picker.innerHTML = sessions.map((session) => {
      const detail = `${session.windows}W${session.attached > 0 ? ` · ${session.attached}A` : ""}`;
      return `<option value="${escapeHtml(session.name)}"${session.name === activeTerminalSession ? " selected" : ""}>${escapeHtml(session.name)} · ${detail}</option>`;
    }).join("");
    renderTerminalSessionStrip(sessions);
  } catch (error) {
    setSocketState("SESSION LIST FAILED");
    setTerminalImageStatus(error instanceof Error ? error.message : "Could not list terminal sessions.", true);
  }
}

function renderTerminalSessionStrip(sessions: TerminalSessionInfo[]): void {
  const strip = document.querySelector<HTMLDivElement>("#terminal-session-strip");
  if (!strip) return;
  const now = Math.floor(Date.now() / 1000);
  strip.innerHTML = sessions.map((session) => {
    const active = session.name === activeTerminalSession;
    const recent = session.activity > 0 && now - session.activity < 15;
    const context = terminalSessionPath(session.path);
    const clients = session.other_clients > 0
      ? `${session.other_clients} LOCAL`
      : `${session.windows}W`;
    const title = [session.name, session.command, session.path, `${session.windows} windows`, `${session.other_clients} other clients`]
      .filter(Boolean)
      .join(" · ");
    return `
      <button class="terminal-session-chip${active ? " is-active" : ""}${recent ? " is-recent" : ""}"
        type="button" data-terminal-session="${escapeHtml(session.name)}" aria-pressed="${active}" title="${escapeHtml(title)}">
        <span class="terminal-session-chip-top">
          <i aria-hidden="true"></i>
          <strong>${escapeHtml(terminalSessionLabel(session.name))}</strong>
          <small>${escapeHtml(clients)}</small>
        </span>
        <span class="terminal-session-chip-detail">
          <b>${escapeHtml(session.command || "shell")}</b>
          ${context ? `<em>${escapeHtml(context)}</em>` : ""}
        </span>
      </button>`;
  }).join("");
  strip.querySelectorAll<HTMLButtonElement>("[data-terminal-session]").forEach((button) => {
    button.addEventListener("click", () => switchToTerminalSession(button.dataset.terminalSession || ""));
  });
}

function terminalSessionLabel(name: string): string {
  const local = /^local-\d{8}-(\d{2})(\d{2})\d{2}-\d+$/.exec(name);
  if (local) return `LOCAL ${local[1]}:${local[2]}`;
  const generated = /^local-(\d{10})-[A-Za-z0-9_-]+$/.exec(name);
  if (!generated) return name;
  const created = new Date(Number(generated[1]) * 1000);
  return `LOCAL ${created.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
}

function terminalSessionPath(path: string): string {
  const homePath = path.replace(/^\/home\/[^/]+(?=\/|$)/, "~");
  if (homePath === "~") return "~";
  if (homePath.startsWith("~/")) {
    const homeParts = homePath.slice(2).split("/").filter(Boolean);
    return `~/${homeParts.slice(-2).join("/")}`;
  }
  const parts = homePath.split("/").filter(Boolean);
  return parts.length > 2 ? `…/${parts.slice(-2).join("/")}` : homePath;
}

function switchTerminalSession(event: Event): void {
  const picker = event.currentTarget as HTMLSelectElement;
  switchToTerminalSession(picker.value);
}

function switchToTerminalSession(name: string): void {
  if (!/^[A-Za-z0-9_-]{1,32}$/.test(name) || name === activeTerminalSession) return;
  saveTerminalSessionPreference(name);
  renderTerminal();
}

async function createTerminalSession(): Promise<void> {
  const requested = window.prompt("Session name (letters, numbers, _ or -):", "work");
  if (requested === null) return;
  const name = requested.trim();
  if (!/^[A-Za-z0-9_-]{1,32}$/.test(name)) {
    setTerminalImageStatus("Use 1-32 letters, numbers, underscores or dashes.", true);
    return;
  }
  const button = document.querySelector<HTMLButtonElement>("#terminal-session-new");
  if (button) button.disabled = true;
  try {
    const response = await fetch(gateUrl("api/terminal/sessions"), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name }),
    });
    const body = (await response.json()) as { name?: string; error?: string };
    if (!response.ok || !body.name) throw new Error(body.error || "Could not create terminal session.");
    saveTerminalSessionPreference(body.name);
    renderTerminal();
  } catch (error) {
    setTerminalImageStatus(error instanceof Error ? error.message : "Could not create terminal session.", true);
    if (button) button.disabled = false;
  }
}

async function openLocalTerminal(): Promise<void> {
  const button = document.querySelector<HTMLButtonElement>("#terminal-local-new");
  if (button) button.disabled = true;
  setTerminalImageStatus("Opening another tmux terminal on the Hyprland desktop…");
  try {
    const response = await fetch(gateUrl("api/terminal/local"), { method: "POST" });
    const body = (await response.json()) as { name?: string; error?: string };
    if (!response.ok || !body.name) throw new Error(body.error || "Could not open local terminal.");
    await refreshTerminalSessions();
    setTerminalImageStatus(`${terminalSessionLabel(body.name)} opened on the Hyprland desktop.`);
  } catch (error) {
    setTerminalImageStatus(error instanceof Error ? error.message : "Could not open local terminal.", true);
  } finally {
    if (button) button.disabled = false;
  }
}

function chooseTerminalImage(): void {
  document.querySelector<HTMLInputElement>("#terminal-image-input")?.click();
}

function selectTerminalImage(event: Event): void {
  const input = event.currentTarget as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  if (file) void uploadTerminalImage(file);
}

function pasteImageIntoTerminal(event: ClipboardEvent): void {
  const image = Array.from(event.clipboardData?.items || [])
    .find((item) => item.kind === "file" && item.type.startsWith("image/"))
    ?.getAsFile();
  if (!image) return;
  event.preventDefault();
  event.stopPropagation();
  void uploadTerminalImage(image);
}

function dropImageIntoTerminal(event: DragEvent): void {
  const image = Array.from(event.dataTransfer?.files || [])
    .find((file) => file.type.startsWith("image/"));
  if (!image) return;
  event.preventDefault();
  void uploadTerminalImage(image);
}

async function uploadTerminalImage(image: File): Promise<void> {
  setTerminalImageStatus(`Uploading ${image.name || "clipboard image"}…`);
  try {
    if (image.size > clipboardLimit) throw new Error("Image exceeds the 8 MiB limit.");
    const response = await fetch(gateUrl("api/terminal/image"), {
      method: "POST",
      headers: { "content-type": image.type },
      body: image,
    });
    if (response.status === 401) return renderLogin("Your gate session expired.");
    if (!response.ok) throw new Error(await responseError(response, "Could not upload clipboard image."));
    const result = (await response.json()) as { path: string; size: number };
    terminal?.focus();
    terminal?.paste(result.path);
    setTerminalImageStatus(`Image ready and path pasted: ${result.path}`);
  } catch (error) {
    setTerminalImageStatus(error instanceof Error ? error.message : "Could not upload clipboard image.", true);
  }
}

function setTerminalImageStatus(message: string, failed = false): void {
  const status = document.querySelector<HTMLParagraphElement>("#terminal-image-status");
  if (!status) return;
  status.textContent = message;
  status.classList.toggle("error", failed);
}

async function renderDesktop(): Promise<void> {
  disposeViews();
  app.innerHTML = gateShell(`
    <section class="workspace desktop-workspace">
      <div class="workspace-bar">
        <div>
          <span class="eyebrow">GLASS STREAM / WAYLAND OUTPUT</span>
          <h1>Remote forge</h1>
        </div>
        <div class="actions">
          <span id="desktop-state" class="socket-state">PROBING</span>
          <button class="ghost-button wake-screens" type="button">WAKE SCREENS</button>
          <select id="desktop-output-picker" class="output-picker" aria-label="Wayland output" disabled></select>
          <select id="desktop-transport-picker" class="output-picker transport-picker" aria-label="Connection protocol" disabled></select>
          <select id="desktop-vnc-profile-picker" class="output-picker vnc-profile-picker" aria-label="VNC performance" hidden></select>
          <button id="desktop-terminal" class="ghost-button" type="button" disabled>OPEN TERMINAL</button>
          <button id="desktop-clipboard" class="ghost-button" type="button" disabled>CLIPBOARD</button>
          <button id="desktop-keyboard" class="ghost-button primary-action" type="button" disabled>KEYBOARD</button>
          <button id="show-terminal" class="ghost-button" type="button">TERMINAL</button>
          <button id="desktop-power" class="ghost-button primary-action" type="button" disabled>START DESKTOP</button>
        </div>
      </div>
      <aside id="clipboard-panel" class="clipboard-panel" aria-label="Clipboard bridge" hidden>
        <div class="clipboard-heading">
          <div><span class="eyebrow">WAYLAND / LOCAL BRIDGE</span><h2>Clipboard</h2></div>
          <button id="clipboard-close" class="ghost-button" type="button">CLOSE</button>
        </div>
        <p id="clipboard-status" class="clipboard-status">Choose a direction. Clipboard contents never leave this authenticated session.</p>
        <div id="clipboard-preview" class="clipboard-preview">
          <span>Remote clipboard preview appears here.</span>
          <textarea id="clipboard-text" readonly hidden aria-label="Remote clipboard text"></textarea>
          <img id="clipboard-image" alt="Remote clipboard image" hidden />
        </div>
        <div id="clipboard-paste-zone" class="clipboard-paste-zone" tabindex="0">
          If browser access is blocked, click here and press Ctrl+V.
        </div>
        <div class="clipboard-actions">
          <button id="clipboard-from-remote" class="ghost-button primary-action" type="button">REMOTE → HERE</button>
          <button id="clipboard-to-remote" class="ghost-button primary-action" type="button">HERE → REMOTE</button>
        </div>
      </aside>
      <div class="desktop-frame" id="desktop-frame" tabindex="0">
        <video id="desktop-video" autoplay playsinline muted></video>
        <img id="desktop-fallback-image" alt="Remote desktop over HTTPS WebSocket" hidden />
        <div id="desktop-vnc" aria-label="Remote desktop over VNC WebSocket" hidden></div>
        <textarea id="desktop-mobile-input" class="desktop-mobile-input" rows="1" inputmode="text"
          autocomplete="off" autocapitalize="off" autocorrect="off" spellcheck="false"
          aria-label="Mobile keyboard input for the remote desktop"></textarea>
        <div class="desktop-touch-hint">TAP = CLICK · DRAG = POINTER · PINCH = ZOOM</div>
        <button id="desktop-zoom-reset" class="desktop-zoom-reset" type="button" hidden>VIEW 100%</button>
        <div class="desktop-empty" id="desktop-empty">
          <span class="brand-mark large" aria-hidden="true"><i></i></span>
          <strong>Virtual Wayland output offline</strong>
          <p>Start the headless forge, then choose WebRTC or the HTTPS/WSS fallback.</p>
        </div>
        <div class="stream-hud">
          <span id="desktop-output">EUTHERGATE-1</span>
          <span id="desktop-transport">VP8 / DATACHANNEL</span>
        </div>
      </div>
      <div class="desktop-footer">
        <span>Click to control · Keyboard opens mobile input · Esc returns locally · Hold F8 for remote Super.</span>
        <span id="desktop-mode">1280×720 @ 30</span>
      </div>
      <div class="desktop-network" aria-live="polite">
        <span id="desktop-ice-route">ICE ROUTE · waiting</span>
        <span id="desktop-ice-detail">TURN · probing</span>
      </div>
    </section>`, "desktop");

  document.querySelector<HTMLButtonElement>("#show-terminal")?.addEventListener("click", renderTerminal);
  document.querySelector<HTMLButtonElement>(".wake-screens")?.addEventListener("click", wakeScreens);
  document.querySelector<HTMLButtonElement>("#desktop-power")?.addEventListener("click", toggleDesktop);
  document.querySelector<HTMLButtonElement>("#desktop-terminal")?.addEventListener("click", launchDesktopTerminal);
  document.querySelector<HTMLButtonElement>("#desktop-clipboard")?.addEventListener("click", openClipboardPanel);
  document.querySelector<HTMLButtonElement>("#desktop-keyboard")?.addEventListener("click", focusDesktopKeyboard);
  const zoomReset = document.querySelector<HTMLButtonElement>("#desktop-zoom-reset");
  zoomReset?.addEventListener("pointerdown", (event) => event.stopPropagation());
  zoomReset?.addEventListener("click", resetDesktopView);
  document.querySelector<HTMLSelectElement>("#desktop-output-picker")?.addEventListener("change", switchDesktopOutput);
  document.querySelector<HTMLSelectElement>("#desktop-transport-picker")?.addEventListener("change", switchDesktopTransport);
  document.querySelector<HTMLSelectElement>("#desktop-vnc-profile-picker")?.addEventListener("change", switchVncPerformanceProfile);
  bindCockpitNavigation();
  installClipboardBridge();
  installDesktopInput();

  try {
    const response = await fetch(gateUrl("api/desktop/status"));
    if (response.status === 401) return renderLogin("Your gate session expired.");
    if (!response.ok) throw new Error("Desktop service did not answer.");
    const status = (await response.json()) as DesktopStatus;
    updateDesktopStatus(status);
    if (status.active) connectDesktop();
  } catch (error) {
    setDesktopState("UNAVAILABLE");
    showDesktopMessage(error instanceof Error ? error.message : "Desktop probe failed.");
  }
}

function updateDesktopStatus(status: DesktopStatus): void {
  desktopIceServers = status.ice_servers || [];
  desktopTransportProfiles = status.transport_profiles || [];
  const power = document.querySelector<HTMLButtonElement>("#desktop-power");
  if (power) {
    power.disabled = !status.available;
    power.textContent = status.active ? "STOP DESKTOP" : "START DESKTOP";
    power.dataset.active = String(status.active);
  }
  const output = document.querySelector<HTMLElement>("#desktop-output");
  if (output) output.textContent = `${status.output} / WS ${status.workspace}`;
  const mode = document.querySelector<HTMLElement>("#desktop-mode");
  if (mode) mode.textContent = status.mode.replace("x", "×").replace("@", " @ ");
  const transport = document.querySelector<HTMLElement>("#desktop-transport");
  if (transport) transport.textContent = `${status.transport.replace("WebRTC/", "")} / DATACHANNEL`;
  const picker = document.querySelector<HTMLSelectElement>("#desktop-output-picker");
  if (picker) {
    picker.innerHTML = status.outputs.map((candidate) => {
      const label = `${candidate.name} — ${candidate.description}`;
      return `<option value="${escapeHtml(candidate.id)}"${candidate.id === status.output_id ? " selected" : ""}>${escapeHtml(label)}</option>`;
    }).join("");
    picker.disabled = false;
  }
  const transportPicker = document.querySelector<HTMLSelectElement>("#desktop-transport-picker");
  if (transportPicker) {
    const preferred = loadTransportPreference();
    const selected = desktopTransportProfiles.some((profile) => profile.id === preferred) ? preferred : "auto";
    transportPicker.innerHTML = desktopTransportProfiles.map((profile) =>
      `<option value="${escapeHtml(profile.id)}" title="${escapeHtml(profile.description)}"${profile.id === selected ? " selected" : ""}>${escapeHtml(profile.label)}</option>`
    ).join("");
    transportPicker.disabled = desktopTransportProfiles.length < 2;
    transportPicker.title = selectedTransportProfile()?.description || "Connection protocol";
  }
  updateVncPerformancePicker();
  const terminalButton = document.querySelector<HTMLButtonElement>("#desktop-terminal");
  if (terminalButton) terminalButton.disabled = !status.active;
  const clipboardButton = document.querySelector<HTMLButtonElement>("#desktop-clipboard");
  if (clipboardButton) clipboardButton.disabled = !status.active;
  const keyboardButton = document.querySelector<HTMLButtonElement>("#desktop-keyboard");
  if (keyboardButton) keyboardButton.disabled = !status.active;
  setDesktopState(status.active ? "NEGOTIATING" : "OFFLINE");
}

async function toggleDesktop(): Promise<void> {
  const button = document.querySelector<HTMLButtonElement>("#desktop-power");
  if (!button) return;
  const active = button.dataset.active === "true";
  button.disabled = true;
  setDesktopState(active ? "STOPPING" : "STARTING");
  try {
    const selectedOutput = document.querySelector<HTMLSelectElement>("#desktop-output-picker")?.value;
    const startUrl = selectedOutput ? `api/desktop/start?output=${encodeURIComponent(selectedOutput)}` : "api/desktop/start";
    const response = await fetch(gateUrl(active ? "api/desktop/stop" : startUrl), { method: "POST" });
    const body = (await response.json()) as { active?: boolean; error?: string };
    if (!response.ok) throw new Error(body.error || "Desktop transition failed.");
    button.dataset.active = String(!active);
    button.textContent = active ? "START DESKTOP" : "STOP DESKTOP";
    if (active) {
      disposeDesktop();
      const terminalButton = document.querySelector<HTMLButtonElement>("#desktop-terminal");
      if (terminalButton) terminalButton.disabled = true;
      const clipboardButton = document.querySelector<HTMLButtonElement>("#desktop-clipboard");
      if (clipboardButton) clipboardButton.disabled = true;
      const keyboardButton = document.querySelector<HTMLButtonElement>("#desktop-keyboard");
      if (keyboardButton) keyboardButton.disabled = true;
      setDesktopState("OFFLINE");
      showDesktopMessage("Virtual Wayland output offline");
    } else {
      const terminalButton = document.querySelector<HTMLButtonElement>("#desktop-terminal");
      if (terminalButton) terminalButton.disabled = false;
      const clipboardButton = document.querySelector<HTMLButtonElement>("#desktop-clipboard");
      if (clipboardButton) clipboardButton.disabled = false;
      const keyboardButton = document.querySelector<HTMLButtonElement>("#desktop-keyboard");
      if (keyboardButton) keyboardButton.disabled = false;
      connectDesktop();
    }
  } catch (error) {
    setDesktopState("FAULT");
    showDesktopMessage(error instanceof Error ? error.message : "Desktop transition failed.");
  } finally {
    button.disabled = false;
  }
}

async function switchDesktopOutput(): Promise<void> {
  const picker = document.querySelector<HTMLSelectElement>("#desktop-output-picker");
  const power = document.querySelector<HTMLButtonElement>("#desktop-power");
  if (!picker || !power || power.dataset.active !== "true") return;
  picker.disabled = true;
  setDesktopState("SWITCHING");
  disposeDesktop();
  await new Promise((resolve) => window.setTimeout(resolve, 250));
  try {
    const response = await fetch(gateUrl(`api/desktop/start?output=${encodeURIComponent(picker.value)}`), { method: "POST" });
    const body = (await response.json()) as { error?: string };
    if (!response.ok) throw new Error(body.error || "Output switch failed.");
    const statusResponse = await fetch(gateUrl("api/desktop/status"));
    const status = (await statusResponse.json()) as DesktopStatus;
    updateDesktopStatus(status);
    connectDesktop();
  } catch (error) {
    setDesktopState("FAULT");
    showDesktopMessage(error instanceof Error ? error.message : "Output switch failed.");
  } finally {
    picker.disabled = false;
  }
}

async function switchDesktopTransport(): Promise<void> {
  const picker = document.querySelector<HTMLSelectElement>("#desktop-transport-picker");
  const power = document.querySelector<HTMLButtonElement>("#desktop-power");
  if (!picker) return;
  saveTransportPreference(picker.value);
  picker.title = selectedTransportProfile()?.description || "Connection protocol";
  updateVncPerformancePicker();
  resetIceDiagnostics();
  if (!power || power.dataset.active !== "true") return;
  picker.disabled = true;
  setDesktopState("CHANGING PROTOCOL");
  disposeDesktop();
  await new Promise((resolve) => window.setTimeout(resolve, 250));
  connectDesktop();
  picker.disabled = false;
}

async function switchVncPerformanceProfile(): Promise<void> {
  const picker = document.querySelector<HTMLSelectElement>("#desktop-vnc-profile-picker");
  const power = document.querySelector<HTMLButtonElement>("#desktop-power");
  if (!picker) return;
  saveVncProfilePreference(picker.value);
  picker.title = selectedVncPerformanceProfile().description;
  updateIceDiagnostics();
  if (!power || power.dataset.active !== "true" || selectedTransportProfile()?.id !== "vnc-wss") return;
  picker.disabled = true;
  setDesktopState("CHANGING VNC PROFILE");
  disposeDesktop();
  await new Promise((resolve) => window.setTimeout(resolve, 250));
  connectVncDesktop();
  picker.disabled = false;
}

async function launchDesktopTerminal(): Promise<void> {
  const button = document.querySelector<HTMLButtonElement>("#desktop-terminal");
  if (button) button.disabled = true;
  try {
    const url = gateUrl("api/desktop/launch-terminal");
    url.searchParams.set("session", activeTerminalSession);
    const response = await fetch(url, { method: "POST" });
    const body = (await response.json()) as { error?: string };
    if (!response.ok) throw new Error(body.error || "Could not launch terminal.");
  } catch (error) {
    setDesktopState("LAUNCH FAILED");
    showDesktopMessage(error instanceof Error ? error.message : "Could not launch terminal.");
  } finally {
    if (button) button.disabled = false;
  }
}

function installClipboardBridge(): void {
  document.querySelector<HTMLButtonElement>("#clipboard-close")?.addEventListener("click", closeClipboardPanel);
  document.querySelector<HTMLButtonElement>("#clipboard-from-remote")?.addEventListener("click", copyRemoteClipboardToLocal);
  document.querySelector<HTMLButtonElement>("#clipboard-to-remote")?.addEventListener("click", sendLocalClipboardToRemote);
  document.querySelector<HTMLDivElement>("#clipboard-paste-zone")?.addEventListener("paste", pasteClipboardToRemote);
}

function openClipboardPanel(): void {
  releaseDesktopControl();
  const panel = document.querySelector<HTMLElement>("#clipboard-panel");
  if (!panel) return;
  panel.hidden = false;
  void refreshRemoteClipboardPreview();
}

function closeClipboardPanel(): void {
  const panel = document.querySelector<HTMLElement>("#clipboard-panel");
  if (panel) panel.hidden = true;
}

async function refreshRemoteClipboardPreview(): Promise<void> {
  setClipboardStatus("Reading the selected Wayland clipboard…");
  remoteClipboardBlob = null;
  try {
    const response = await fetch(gateUrl("api/desktop/clipboard"), { cache: "no-store" });
    if (response.status === 204) {
      clearClipboardPreview();
      setClipboardStatus("The remote clipboard is empty or has no supported text/image format.");
      return;
    }
    if (!response.ok) throw new Error(await responseError(response, "Could not read remote clipboard."));
    const blob = await response.blob();
    if (blob.size > clipboardLimit) throw new Error("Remote clipboard exceeds the 8 MiB limit.");
    remoteClipboardBlob = blob;
    await showClipboardPreview(blob);
    setClipboardStatus(`${clipboardDescription(blob)} ready. Press REMOTE → HERE to copy it.`);
  } catch (error) {
    clearClipboardPreview();
    setClipboardStatus(error instanceof Error ? error.message : "Could not read remote clipboard.", true);
  }
}

async function copyRemoteClipboardToLocal(): Promise<void> {
  const blob = remoteClipboardBlob;
  if (!blob) {
    setClipboardStatus("No remote clipboard value is loaded. Close and reopen the panel to refresh.", true);
    return;
  }
  try {
    if (blob.type.startsWith("text/plain")) {
      const text = await blob.text();
      await navigator.clipboard.writeText(text);
    } else if (navigator.clipboard.write && typeof ClipboardItem !== "undefined") {
      await navigator.clipboard.write([new ClipboardItem({ [blob.type]: blob })]);
    } else {
      throw new Error("This browser cannot write images directly to the clipboard.");
    }
    setClipboardStatus(`${clipboardDescription(blob)} copied to this computer.`);
  } catch (error) {
    const text = document.querySelector<HTMLTextAreaElement>("#clipboard-text");
    if (text && !text.hidden) {
      text.focus();
      text.select();
      setClipboardStatus("Browser copy was blocked. The text is selected—press Ctrl+C.", true);
    } else {
      setClipboardStatus("Browser image copy was blocked. Right-click the preview and choose Copy Image.", true);
    }
  }
}

async function sendLocalClipboardToRemote(): Promise<void> {
  try {
    if (!navigator.clipboard.read) throw new Error("Clipboard item reading is unavailable.");
    const items = await navigator.clipboard.read();
    for (const item of items) {
      const imageType = ["image/png", "image/jpeg", "image/webp"].find((type) => item.types.includes(type));
      if (imageType) {
        await uploadClipboard(await item.getType(imageType));
        return;
      }
    }
    for (const item of items) {
      if (item.types.includes("text/plain")) {
        await uploadClipboard(await item.getType("text/plain"));
        return;
      }
    }
    throw new Error("Local clipboard has no supported text or image.");
  } catch {
    const zone = document.querySelector<HTMLDivElement>("#clipboard-paste-zone");
    zone?.focus();
    setClipboardStatus("Direct access was blocked. Press Ctrl+V in the highlighted box.", true);
  }
}

function pasteClipboardToRemote(event: ClipboardEvent): void {
  event.preventDefault();
  const clipboard = event.clipboardData;
  if (!clipboard) return;
  const image = Array.from(clipboard.items)
    .find((item) => ["image/png", "image/jpeg", "image/webp"].includes(item.type))
    ?.getAsFile();
  if (image) {
    void uploadClipboard(image);
    return;
  }
  const text = clipboard.getData("text/plain");
  if (text) {
    void uploadClipboard(new Blob([text], { type: "text/plain;charset=utf-8" }));
    return;
  }
  setClipboardStatus("The pasted value has no supported text or image.", true);
}

async function uploadClipboard(blob: Blob): Promise<void> {
  if (!blob.size) {
    setClipboardStatus("The local clipboard is empty.", true);
    return;
  }
  if (blob.size > clipboardLimit) {
    setClipboardStatus("Clipboard payload exceeds the 8 MiB limit.", true);
    return;
  }
  const mime = blob.type.split(";", 1)[0].toLowerCase();
  if (!["text/plain", "image/png", "image/jpeg", "image/webp"].includes(mime)) {
    setClipboardStatus(`Unsupported clipboard type: ${mime || "unknown"}.`, true);
    return;
  }
  setClipboardStatus(`Sending ${clipboardDescription(blob)} to the selected Wayland session…`);
  try {
    const response = await fetch(gateUrl("api/desktop/clipboard"), {
      method: "POST",
      headers: { "content-type": blob.type || mime },
      body: blob,
    });
    if (!response.ok) throw new Error(await responseError(response, "Could not update remote clipboard."));
    remoteClipboardBlob = blob;
    await showClipboardPreview(blob);
    setClipboardStatus(`${clipboardDescription(blob)} is now on the remote clipboard.`);
  } catch (error) {
    setClipboardStatus(error instanceof Error ? error.message : "Could not update remote clipboard.", true);
  }
}

async function showClipboardPreview(blob: Blob): Promise<void> {
  clearClipboardPreview();
  const placeholder = document.querySelector<HTMLSpanElement>("#clipboard-preview > span");
  const text = document.querySelector<HTMLTextAreaElement>("#clipboard-text");
  const image = document.querySelector<HTMLImageElement>("#clipboard-image");
  if (placeholder) placeholder.hidden = true;
  if (blob.type.startsWith("text/plain") && text) {
    text.value = await blob.text();
    text.hidden = false;
  } else if (image) {
    clipboardPreviewUrl = URL.createObjectURL(blob);
    image.src = clipboardPreviewUrl;
    image.hidden = false;
  }
}

function clearClipboardPreview(): void {
  if (clipboardPreviewUrl) URL.revokeObjectURL(clipboardPreviewUrl);
  clipboardPreviewUrl = null;
  const placeholder = document.querySelector<HTMLSpanElement>("#clipboard-preview > span");
  const text = document.querySelector<HTMLTextAreaElement>("#clipboard-text");
  const image = document.querySelector<HTMLImageElement>("#clipboard-image");
  if (placeholder) placeholder.hidden = false;
  if (text) {
    text.value = "";
    text.hidden = true;
  }
  if (image) {
    image.removeAttribute("src");
    image.hidden = true;
  }
}

function clipboardDescription(blob: Blob): string {
  const kind = blob.type.startsWith("text/plain") ? "Text" : "Image";
  const size = blob.size < 1024 ? `${blob.size} B` : `${(blob.size / 1024).toFixed(blob.size < 1024 * 1024 ? 1 : 0)} KiB`;
  return `${kind} (${size})`;
}

function setClipboardStatus(message: string, error = false): void {
  const status = document.querySelector<HTMLParagraphElement>("#clipboard-status");
  if (!status) return;
  status.textContent = message;
  status.classList.toggle("error", error);
}

async function responseError(response: Response, fallback: string): Promise<string> {
  try {
    const body = (await response.json()) as { error?: string };
    return body.error || fallback;
  } catch {
    return fallback;
  }
}

function connectDesktop(): void {
  disposeDesktop();
  remoteCandidates = [];
  desktopVideoReady = false;
  resetIceDiagnostics();

  const profile = selectedTransportProfile();
  if (profile?.id === "vnc-wss") {
    connectVncDesktop();
    return;
  }
  if (profile?.id === "https-wss") {
    connectFallbackDesktop();
    return;
  }

  socket = new WebSocket(gateWebSocket("ws/desktop"));
  setDesktopState("NEGOTIATING");
  activeDesktopIceServers = iceServersForProfile(profile);
  peer = new RTCPeerConnection({
    bundlePolicy: "max-bundle",
    iceServers: activeDesktopIceServers,
    iceTransportPolicy: profile?.ice_transport_policy || "all",
  });
  peer.addTransceiver("video", { direction: "recvonly" });
  // Position must arrive before its click, and modifiers before their key.
  // SCTP's reliable ordered mode is still independent of the video stream.
  inputChannel = peer.createDataChannel("input", { ordered: true });
  inputChannel.addEventListener("open", () => setDesktopState(desktopVideoReady ? "LIVE" : "WAITING FOR VIDEO"));
  inputChannel.addEventListener("close", () => setDesktopState("VIDEO ONLY"));
  peer.addEventListener("track", (event) => {
    const video = document.querySelector<HTMLVideoElement>("#desktop-video");
    if (!video) return;
    video.srcObject = event.streams[0] || new MediaStream([event.track]);
    video.addEventListener("playing", markDesktopVideoReady, { once: true });
    void video.play().catch((error: unknown) => {
      setDesktopState("PLAYBACK BLOCKED");
      showDesktopMessage(error instanceof Error ? error.message : "Browser blocked video playback.");
    });
  });
  peer.addEventListener("icecandidate", (event) => {
    if (event.candidate) {
      const summary = describeIceCandidate(event.candidate);
      if (!desktopIceCandidates.includes(summary)) desktopIceCandidates.push(summary);
      updateIceDiagnostics();
    }
    if (event.candidate && socket?.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify({ type: "ice", ...event.candidate.toJSON() }));
    }
  });
  peer.addEventListener("icecandidateerror", (event) => {
    const failure = event as RTCPeerConnectionIceErrorEvent;
    const server = describeIceUrl(failure.url);
    const detail = `${server} · ${failure.errorCode} ${failure.errorText}`.trim();
    if (!desktopIceErrors.includes(detail)) desktopIceErrors.push(detail);
    updateIceDiagnostics();
  });
  peer.addEventListener("icegatheringstatechange", updateIceDiagnostics);
  peer.addEventListener("connectionstatechange", () => {
    if (peer?.connectionState === "connected") {
      setDesktopState(desktopVideoReady ? (inputChannel?.readyState === "open" ? "LIVE" : "VIDEO") : "WAITING FOR VIDEO");
      void updateSelectedIceRoute();
    }
    if (["failed", "disconnected", "closed"].includes(peer?.connectionState || "")) {
      setDesktopState("RECONNECTING");
      void updateSelectedIceRoute();
    }
  });
  peer.addEventListener("iceconnectionstatechange", () => {
    if (!peer || desktopVideoReady) return;
    setDesktopState(`ICE ${peer.iceConnectionState.toUpperCase()}`);
  });

  socket.addEventListener("open", async () => {
    if (!peer || socket?.readyState !== WebSocket.OPEN) return;
    const offer = await peer.createOffer();
    await peer.setLocalDescription(offer);
    socket.send(JSON.stringify({ type: "offer", sdp: offer.sdp }));
    desktopNegotiationTimer = window.setTimeout(() => {
      if (!desktopVideoReady) {
        const rtcState = peer?.connectionState || "unknown";
        const iceState = peer?.iceConnectionState || "unknown";
        void updateSelectedIceRoute();
        setDesktopState("NO VIDEO");
        showDesktopMessage(`No video frames yet (WebRTC ${rtcState}, ICE ${iceState}).`);
      }
    }, 8000);
  });
  socket.addEventListener("message", async (event) => {
    const message = JSON.parse(String(event.data)) as Record<string, unknown>;
    if (message.type === "ready") {
      const transport = document.querySelector<HTMLElement>("#desktop-transport");
      if (transport) transport.textContent = `${String(message.codec)} / DATACHANNEL`;
    } else if (message.type === "answer" && peer) {
      await peer.setRemoteDescription({ type: "answer", sdp: String(message.sdp) });
      for (const candidate of remoteCandidates) await peer.addIceCandidate(candidate);
      remoteCandidates = [];
    } else if (message.type === "ice" && peer) {
      const candidate = { candidate: String(message.candidate), sdpMLineIndex: Number(message.sdpMLineIndex) };
      if (peer.remoteDescription) await peer.addIceCandidate(candidate);
      else remoteCandidates.push(candidate);
    } else if (message.type === "error" || message.type === "fatal") {
      showDesktopMessage(String(message.message || "WebRTC media failure"));
      setDesktopState("FAULT");
    } else if (message.type === "capture-warning" || message.type === "input-warning") {
      console.warn("EutherGate desktop:", message.message);
    }
  });
  socket.addEventListener("close", () => {
    if (peer) setDesktopState("DISCONNECTED");
  });
}

function connectVncDesktop(attempt = 0): void {
  desktopVncActive = true;
  activeDesktopIceServers = [];
  const performance = selectedVncPerformanceProfile();
  const video = document.querySelector<HTMLVideoElement>("#desktop-video");
  const target = document.querySelector<HTMLDivElement>("#desktop-vnc");
  if (!target) {
    setDesktopState("VNC UNAVAILABLE");
    return;
  }
  if (video) video.hidden = true;
  target.hidden = false;
  updateIceDiagnostics();
  setDesktopState("CONNECTING VNC/WSS");
  window.addEventListener("keydown", vncSuperKeyEvent, { capture: true });
  window.addEventListener("keyup", vncSuperKeyEvent, { capture: true });
  window.addEventListener("blur", releaseVncSuperKey);

  // noVNC measures its target when scaleViewport is enabled. Wait until the
  // previously hidden target has a definite laid-out size before constructing
  // RFB, otherwise its canvas can remain at a zero-sized black viewport.
  window.requestAnimationFrame(() => {
    if (!desktopVncActive || !target.isConnected) return;
    target.replaceChildren();
    const vncUrl = gateWebSocket("ws/desktop-vnc");
    vncUrl.searchParams.set("profile", performance.id);
    const session = new RFB(target, vncUrl.toString(), { shared: false });
    vnc = session;
    session.background = "#050608";
    session.scaleViewport = true;
    session.resizeSession = false;
    session.qualityLevel = 7;
    session.compressionLevel = 5;
    session.addEventListener("connect", () => {
      if (vnc !== session) return;
      desktopVideoReady = true;
      hideDesktopMessage();
      setDesktopState("LIVE");
      const transport = document.querySelector<HTMLElement>("#desktop-transport");
      if (transport) transport.textContent = `RFB ${performance.fps} FPS${performance.gpu ? " GPU/H.264" : ""} / WSS`;
      session.focus();
      window.requestAnimationFrame(() => installVncTouchZoom(session, target));
    });
    session.addEventListener("disconnect", (event) => {
      if (vnc !== session) return;
      vnc = null;
      const clean = Boolean((event as CustomEvent<{ clean?: boolean }>).detail?.clean);
      if (!clean && desktopVncActive && attempt < 3) {
        const delay = 400 * (2 ** attempt);
        setDesktopState("RETRYING VNC/WSS");
        desktopVncRetryTimer = window.setTimeout(() => {
          desktopVncRetryTimer = null;
          if (desktopVncActive) connectVncDesktop(attempt + 1);
        }, delay);
        return;
      }
      setDesktopState(clean ? "DISCONNECTED" : "VNC FAILED");
      if (!clean) showDesktopMessage("The authenticated VNC/WSS desktop connection failed.");
    });
    session.addEventListener("securityfailure", (event) => {
      if (vnc !== session) return;
      const reason = String((event as CustomEvent<{ reason?: string }>).detail?.reason || "VNC security negotiation failed.");
      setDesktopState("VNC REJECTED");
      showDesktopMessage(reason);
    });
  });
}

function connectFallbackDesktop(): void {
  desktopFallbackActive = true;
  activeDesktopIceServers = [];
  const video = document.querySelector<HTMLVideoElement>("#desktop-video");
  const image = document.querySelector<HTMLImageElement>("#desktop-fallback-image");
  if (video) video.hidden = true;
  if (image) image.hidden = false;
  updateIceDiagnostics();
  setDesktopState("CONNECTING HTTPS/WSS");

  const fallbackSocket = new WebSocket(gateWebSocket("ws/desktop-fallback"));
  fallbackSocket.binaryType = "arraybuffer";
  socket = fallbackSocket;
  fallbackSocket.addEventListener("open", () => {
    if (socket !== fallbackSocket) return;
    setDesktopState("WAITING FOR HTTPS/WSS VIDEO");
    desktopNegotiationTimer = window.setTimeout(() => {
      if (!desktopVideoReady && socket === fallbackSocket) {
        setDesktopState("NO VIDEO");
        showDesktopMessage("No HTTPS/WSS desktop frame arrived within 8 seconds.");
      }
    }, 8000);
  });
  fallbackSocket.addEventListener("message", (event) => {
    if (socket !== fallbackSocket) return;
    if (event.data instanceof ArrayBuffer) {
      showFallbackFrame(event.data);
      return;
    }
    const message = JSON.parse(String(event.data)) as Record<string, unknown>;
    if (message.type === "ready") {
      const transport = document.querySelector<HTMLElement>("#desktop-transport");
      if (transport) transport.textContent = `${String(message.codec)} / WSS INPUT`;
    } else if (message.type === "error" || message.type === "fatal") {
      showDesktopMessage(String(message.message || "HTTPS/WSS desktop failure"));
      setDesktopState("FAULT");
    } else if (message.type === "capture-warning" || message.type === "input-warning") {
      console.warn("EutherGate HTTPS/WSS desktop:", message.message);
    }
  });
  fallbackSocket.addEventListener("close", () => {
    if (socket === fallbackSocket) setDesktopState("DISCONNECTED");
  });
  fallbackSocket.addEventListener("error", () => {
    if (socket === fallbackSocket) {
      setDesktopState("WSS FAILED");
      showDesktopMessage("The authenticated HTTPS/WSS desktop connection failed.");
    }
  });
}

function showFallbackFrame(frame: ArrayBuffer): void {
  const image = document.querySelector<HTMLImageElement>("#desktop-fallback-image");
  if (!image) return;
  if (desktopFallbackFrameUrl) URL.revokeObjectURL(desktopFallbackFrameUrl);
  desktopFallbackFrameUrl = URL.createObjectURL(new Blob([frame], { type: "image/jpeg" }));
  image.addEventListener("load", () => {
    if (!desktopVideoReady) markDesktopVideoReady();
  }, { once: true });
  image.src = desktopFallbackFrameUrl;
}

function resetIceDiagnostics(): void {
  desktopIceCandidates = [];
  desktopIceErrors = [];
  desktopSelectedRoute = "none";
  updateIceDiagnostics();
}

function iceServerUrls(): string[] {
  return activeDesktopIceServers.flatMap((server) => typeof server.urls === "string" ? [server.urls] : server.urls);
}

function selectedTransportProfile(): DesktopTransportProfile | undefined {
  const picker = document.querySelector<HTMLSelectElement>("#desktop-transport-picker");
  const id = picker?.value || loadTransportPreference();
  return desktopTransportProfiles.find((profile) => profile.id === id)
    || desktopTransportProfiles.find((profile) => profile.id === "auto");
}

function iceServersForProfile(profile: DesktopTransportProfile | undefined): RTCIceServer[] {
  if (!profile) return desktopIceServers;
  const allowed = new Set(profile.urls);
  return desktopIceServers.flatMap((server) => {
    const urls = (typeof server.urls === "string" ? [server.urls] : server.urls)
      .filter((url) => allowed.has(url));
    return urls.length ? [{ ...server, urls }] : [];
  });
}

function loadTransportPreference(): string {
  try {
    return window.localStorage.getItem(transportPreferenceKey) || "auto";
  } catch {
    return "auto";
  }
}

function saveTransportPreference(profile: string): void {
  try {
    window.localStorage.setItem(transportPreferenceKey, profile);
  } catch {
    // Private browsing can deny persistent storage; the current selection still works.
  }
}

function selectedVncPerformanceProfile(): VncPerformanceProfile {
  const picker = document.querySelector<HTMLSelectElement>("#desktop-vnc-profile-picker");
  const id = picker?.value || loadVncProfilePreference();
  return vncPerformanceProfiles.find((profile) => profile.id === id) || vncPerformanceProfiles[0];
}

function loadVncProfilePreference(): string {
  try {
    return window.localStorage.getItem(vncProfilePreferenceKey) || "compatible";
  } catch {
    return "compatible";
  }
}

function saveVncProfilePreference(profile: string): void {
  try {
    window.localStorage.setItem(vncProfilePreferenceKey, profile);
  } catch {
    // The selected profile remains active even when local storage is unavailable.
  }
}

function updateVncPerformancePicker(): void {
  const picker = document.querySelector<HTMLSelectElement>("#desktop-vnc-profile-picker");
  if (!picker) return;
  const visible = selectedTransportProfile()?.id === "vnc-wss";
  const preferred = loadVncProfilePreference();
  const selected = vncPerformanceProfiles.some((profile) => profile.id === preferred) ? preferred : "compatible";
  picker.innerHTML = vncPerformanceProfiles.map((profile) =>
    `<option value="${profile.id}" title="${escapeHtml(profile.description)}"${profile.id === selected ? " selected" : ""}>${escapeHtml(profile.label)}</option>`
  ).join("");
  picker.hidden = !visible;
  picker.disabled = !visible;
  picker.title = selectedVncPerformanceProfile().description;
}

function describeIceUrl(url: string): string {
  if (!url) return "unknown ICE server";
  const match = url.match(/^(turns?):(?:[^@]+@)?([^?]+)(?:\?transport=([^&]+))?/i);
  if (!match) return "ICE server";
  const transport = match[3] ? `/${match[3].toLowerCase()}` : "";
  return `${match[1].toLowerCase()}:${match[2]}${transport}`;
}

function describeIceCandidate(candidate: RTCIceCandidate): string {
  const extended = candidate as ExtendedIceCandidate;
  const type = candidate.type || "unknown";
  const protocol = candidate.protocol || "unknown";
  const relayProtocol = extended.relayProtocol ? `/${extended.relayProtocol}` : "";
  const server = extended.url ? ` via ${describeIceUrl(extended.url)}` : "";
  return `${type}/${protocol}${relayProtocol}${server}`;
}

function updateIceDiagnostics(): void {
  const route = document.querySelector<HTMLElement>("#desktop-ice-route");
  const detail = document.querySelector<HTMLElement>("#desktop-ice-detail");
  if (desktopVncActive) {
    const performance = selectedVncPerformanceProfile();
    if (route) route.textContent = "STREAM ROUTE · authenticated VNC/WSS";
    if (detail) {
      detail.textContent = `RFB · ${performance.fps} FPS${performance.gpu ? " + GPU/H.264 when supported" : ""} · private WayVNC Unix socket + WebSocket/TCP 443 · F8 = SUPER`;
      detail.classList.remove("error");
    }
    return;
  }
  if (desktopFallbackActive) {
    if (route) route.textContent = "STREAM ROUTE · authenticated HTTPS/WSS";
    if (detail) {
      detail.textContent = "FALLBACK · JPEG frames + input over WebSocket/TCP 443";
      detail.classList.remove("error");
    }
    return;
  }
  const gathering = peer?.iceGatheringState || "new";
  if (route) route.textContent = `ICE ROUTE · ${desktopSelectedRoute} · gathering ${gathering}`;
  if (!detail) return;
  if (desktopIceErrors.length) {
    detail.textContent = `TURN ERROR · ${desktopIceErrors.join(" | ")}`;
    detail.classList.add("error");
    return;
  }
  detail.classList.remove("error");
  const candidates = desktopIceCandidates.length ? desktopIceCandidates.join(" | ") : "no candidates yet";
  const servers = iceServerUrls().map(describeIceUrl).join(" | ") || "not configured";
  detail.textContent = `CANDIDATES · ${candidates} · SERVERS · ${servers}`;
}

async function updateSelectedIceRoute(): Promise<void> {
  const activePeer = peer;
  if (!activePeer) return;
  try {
    const report = await activePeer.getStats();
    if (peer !== activePeer) return;
    let selectedPair: IceCandidatePairStats | undefined;
    report.forEach((entry) => {
      if (entry.type === "transport" && entry.selectedCandidatePairId) {
        selectedPair = report.get(entry.selectedCandidatePairId) as IceCandidatePairStats | undefined;
      }
    });
    if (!selectedPair) {
      report.forEach((entry) => {
        if (entry.type === "candidate-pair" && entry.state === "succeeded" && (entry.nominated || entry.selected)) {
          selectedPair = entry as IceCandidatePairStats;
        }
      });
    }
    if (!selectedPair) {
      desktopSelectedRoute = activePeer.connectionState;
      updateIceDiagnostics();
      return;
    }
    const local = report.get(selectedPair.localCandidateId);
    const remote = report.get(selectedPair.remoteCandidateId);
    const localType = local?.candidateType || "unknown";
    const localProtocol = local?.protocol || "unknown";
    const relayProtocol = local?.relayProtocol ? `/${local.relayProtocol}` : "";
    const remoteType = remote?.candidateType || "unknown";
    desktopSelectedRoute = `${localType}/${localProtocol}${relayProtocol} → ${remoteType}`;
  } catch (error) {
    desktopSelectedRoute = `stats unavailable: ${error instanceof Error ? error.message : "unknown error"}`;
  }
  updateIceDiagnostics();
}

function markDesktopVideoReady(): void {
  desktopVideoReady = true;
  if (desktopNegotiationTimer !== null) window.clearTimeout(desktopNegotiationTimer);
  desktopNegotiationTimer = null;
  hideDesktopMessage();
  setDesktopState(desktopFallbackActive || inputChannel?.readyState === "open" ? "LIVE" : "VIDEO");
  if (!desktopFallbackActive) void updateSelectedIceRoute();
}

function installDesktopInput(): void {
  const frame = document.querySelector<HTMLDivElement>("#desktop-frame");
  const mobileInput = document.querySelector<HTMLTextAreaElement>("#desktop-mobile-input");
  if (!frame) return;
  mobileInput?.addEventListener("input", desktopMobileInput);
  mobileInput?.addEventListener("compositionend", flushDesktopMobileInput);
  mobileInput?.addEventListener("beforeinput", desktopMobileBeforeInput);
  frame.addEventListener("pointermove", (event) => {
    if (desktopVncActive) return;
    if (event.pointerType === "touch") {
      desktopTouchMove(frame, event);
      return;
    }
    if (!desktopControlActive) return;
    if (document.pointerLockElement === frame) {
      sendDesktopInput({ type: "pointer_delta", dx: event.movementX, dy: event.movementY });
      return;
    }
    const position = remotePositionFromClient(event.clientX, event.clientY);
    if (!position) return;
    sendDesktopInput({
      type: "pointer_move",
      x: position.x,
      y: position.y,
    });
  });
  frame.addEventListener("pointerdown", (event) => {
    if (desktopVncActive) return;
    if (!desktopVideoReady) return;
    event.preventDefault();
    if (event.pointerType === "touch") {
      desktopTouchStart(frame, event);
      return;
    }
    const position = remotePositionFromClient(event.clientX, event.clientY);
    if (position) {
      sendDesktopInput({ type: "pointer_move", x: position.x, y: position.y });
    }
    desktopControlActive = true;
    frame.focus();
    if (document.pointerLockElement !== frame) {
      const lockRequest = frame.requestPointerLock();
      if (lockRequest) {
        void lockRequest.catch(() => {
          setDesktopState("CONTROL / POINTER LOCK BLOCKED");
        });
      }
    }
    sendDesktopInput({ type: "pointer_button", button: event.button, state: "pressed" });
  });
  frame.addEventListener("pointerup", (event) => {
    if (desktopVncActive) return;
    if (event.pointerType === "touch") {
      desktopTouchEnd(frame, event, false);
      return;
    }
    sendDesktopInput({ type: "pointer_button", button: event.button, state: "released" });
  });
  frame.addEventListener("pointercancel", (event) => {
    if (event.pointerType === "touch") desktopTouchEnd(frame, event, true);
  });
  frame.addEventListener("contextmenu", (event) => event.preventDefault());
  frame.addEventListener("wheel", (event) => {
    if (desktopVncActive) return;
    event.preventDefault();
    sendDesktopInput({ type: "wheel", dx: event.deltaX, dy: event.deltaY });
  }, { passive: false });
  window.addEventListener("keydown", desktopKeyEvent, { capture: true });
  window.addEventListener("keyup", desktopKeyEvent, { capture: true });
  window.addEventListener("blur", releaseDesktopSuperKey);
  document.addEventListener("pointerlockchange", desktopPointerLockChange);
}

function desktopTouchStart(frame: HTMLDivElement, event: PointerEvent): void {
  try {
    frame.setPointerCapture(event.pointerId);
  } catch {
    // Safari can retain the pointer without explicit capture.
  }
  desktopTouches.set(event.pointerId, {
    startX: event.clientX,
    startY: event.clientY,
    clientX: event.clientX,
    clientY: event.clientY,
    startPanX: desktopViewPanX,
    startPanY: desktopViewPanY,
    moved: false,
    consumed: false,
  });
  desktopControlActive = true;
  frame.focus({ preventScroll: true });
  if (desktopTouches.size >= 2) beginDesktopPinch(frame);
  setDesktopState(desktopTouches.size >= 2 ? "PINCH ZOOM" : "TOUCH CONTROL");
}

function desktopTouchMove(frame: HTMLDivElement, event: PointerEvent): void {
  const point = desktopTouches.get(event.pointerId);
  if (!point) return;
  event.preventDefault();
  point.clientX = event.clientX;
  point.clientY = event.clientY;
  if (Math.hypot(point.clientX - point.startX, point.clientY - point.startY) > 16) {
    point.moved = true;
  }

  if (desktopTouches.size >= 2) {
    if (!desktopPinch) beginDesktopPinch(frame);
    updateDesktopPinch(frame);
    return;
  }
  if (point.consumed || !point.moved) return;

  if (desktopViewScale > 1.01) {
    desktopViewPanX = point.startPanX + point.clientX - point.startX;
    desktopViewPanY = point.startPanY + point.clientY - point.startY;
    clampDesktopViewPan(frame);
    applyDesktopViewTransform();
    return;
  }

  const position = remotePositionFromClient(event.clientX, event.clientY);
  if (position) sendDesktopInput({ type: "pointer_move", x: position.x, y: position.y });
}

function desktopTouchEnd(frame: HTMLDivElement, event: PointerEvent, cancelled: boolean): void {
  const point = desktopTouches.get(event.pointerId);
  if (!point) return;
  event.preventDefault();
  point.clientX = event.clientX;
  point.clientY = event.clientY;
  const shouldClick = !cancelled && !point.consumed && !point.moved && desktopTouches.size === 1;
  desktopTouches.delete(event.pointerId);
  if (desktopTouches.size < 2) desktopPinch = null;

  if (shouldClick) {
    const position = remotePositionFromClient(event.clientX, event.clientY);
    if (position) {
      sendDesktopInput({ type: "pointer_move", x: position.x, y: position.y });
      sendDesktopInput({ type: "pointer_button", button: 0, state: "pressed" });
      const releaseTimer = window.setTimeout(() => {
        desktopTouchReleaseTimers.delete(releaseTimer);
        sendDesktopInput({ type: "pointer_button", button: 0, state: "released" });
      }, 45);
      desktopTouchReleaseTimers.add(releaseTimer);
    }
  }
  if (desktopTouches.size === 0) setDesktopState(desktopViewScale > 1.01 ? `ZOOM ${Math.round(desktopViewScale * 100)}%` : "LIVE");
  try {
    frame.releasePointerCapture(event.pointerId);
  } catch {
    // The browser may already have released a cancelled pointer.
  }
}

function beginDesktopPinch(frame: HTMLDivElement): void {
  const points = [...desktopTouches.values()].slice(0, 2);
  if (points.length < 2) return;
  for (const point of desktopTouches.values()) point.consumed = true;
  const rect = frame.getBoundingClientRect();
  desktopPinch = {
    distance: Math.max(1, Math.hypot(points[0].clientX - points[1].clientX, points[0].clientY - points[1].clientY)),
    midpointX: (points[0].clientX + points[1].clientX) / 2 - rect.left,
    midpointY: (points[0].clientY + points[1].clientY) / 2 - rect.top,
    scale: desktopViewScale,
    panX: desktopViewPanX,
    panY: desktopViewPanY,
  };
}

function updateDesktopPinch(frame: HTMLDivElement): void {
  const points = [...desktopTouches.values()].slice(0, 2);
  if (!desktopPinch || points.length < 2) return;
  const rect = frame.getBoundingClientRect();
  const midpointX = (points[0].clientX + points[1].clientX) / 2 - rect.left;
  const midpointY = (points[0].clientY + points[1].clientY) / 2 - rect.top;
  const distance = Math.max(1, Math.hypot(points[0].clientX - points[1].clientX, points[0].clientY - points[1].clientY));
  const nextScale = Math.max(1, Math.min(4, desktopPinch.scale * distance / desktopPinch.distance));
  const ratio = nextScale / desktopPinch.scale;
  desktopViewScale = nextScale;
  desktopViewPanX = midpointX - frame.clientWidth / 2
    - (desktopPinch.midpointX - frame.clientWidth / 2 - desktopPinch.panX) * ratio;
  desktopViewPanY = midpointY - frame.clientHeight / 2
    - (desktopPinch.midpointY - frame.clientHeight / 2 - desktopPinch.panY) * ratio;
  clampDesktopViewPan(frame);
  applyDesktopViewTransform();
  setDesktopState(`ZOOM ${Math.round(desktopViewScale * 100)}%`);
}

function clampDesktopViewPan(frame: HTMLDivElement): void {
  const maxX = frame.clientWidth * (desktopViewScale - 1) / 2;
  const maxY = frame.clientHeight * (desktopViewScale - 1) / 2;
  desktopViewPanX = Math.max(-maxX, Math.min(maxX, desktopViewPanX));
  desktopViewPanY = Math.max(-maxY, Math.min(maxY, desktopViewPanY));
  if (desktopViewScale <= 1.01) {
    desktopViewScale = 1;
    desktopViewPanX = 0;
    desktopViewPanY = 0;
  }
}

function applyDesktopViewTransform(): void {
  const frame = document.querySelector<HTMLDivElement>("#desktop-frame");
  if (!frame) return;
  frame.style.setProperty("--desktop-view-scale", String(desktopViewScale));
  frame.style.setProperty("--desktop-view-pan-x", `${desktopViewPanX}px`);
  frame.style.setProperty("--desktop-view-pan-y", `${desktopViewPanY}px`);
  const reset = document.querySelector<HTMLButtonElement>("#desktop-zoom-reset");
  if (reset) {
    reset.hidden = desktopViewScale <= 1.01;
    reset.textContent = `VIEW ${Math.round(desktopViewScale * 100)}% · RESET`;
  }
}

function vncDisplayBridge(session: RFB): VncDisplayBridge | null {
  return (session as unknown as VncSessionBridge)._display || null;
}

function installVncTouchZoom(session: RFB, target: HTMLDivElement): void {
  const canvas = target.querySelector<HTMLCanvasElement>("canvas");
  const display = vncDisplayBridge(session);
  const sessionBridge = session as unknown as VncSessionBridge;
  if (!canvas || !display || vnc !== session) return;
  desktopVncFitScale = display.scale || 1;

  const handleGesture = (rawEvent: Event): void => {
    const event = rawEvent as CustomEvent<VncGestureDetail>;
    if (event.detail?.type !== "pinch" || vnc !== session) return;
    event.stopImmediatePropagation();
    const magnitude = Math.max(1, Math.hypot(event.detail.magnitudeX || 0, event.detail.magnitudeY || 0));
    if (event.type === "gesturestart") {
      desktopVncPinchDistance = magnitude;
      desktopVncPinchStartScale = desktopViewScale;
      if (desktopViewScale <= 1.01) desktopVncFitScale = display.scale || 1;
      session.scaleViewport = false;
      sessionBridge.clipViewport = true;
      sessionBridge.dragViewport = true;
      display.scale = desktopVncFitScale * desktopViewScale;
      setDesktopState("PINCH ZOOM");
      return;
    }
    if (event.type === "gesturemove") {
      desktopViewScale = Math.max(1, Math.min(4, desktopVncPinchStartScale * magnitude / desktopVncPinchDistance));
      if (desktopViewScale <= 1.01) {
        resetDesktopView();
        return;
      }
      display.scale = desktopVncFitScale * desktopViewScale;
      applyDesktopViewTransform();
      setDesktopState(`ZOOM ${Math.round(desktopViewScale * 100)}%`);
      return;
    }
    if (event.type === "gestureend") {
      setDesktopState(desktopViewScale > 1.01 ? `ZOOM ${Math.round(desktopViewScale * 100)}%` : "LIVE");
    }
  };

  canvas.addEventListener("gesturestart", handleGesture, { capture: true });
  canvas.addEventListener("gesturemove", handleGesture, { capture: true });
  canvas.addEventListener("gestureend", handleGesture, { capture: true });
}

function resetDesktopView(event?: Event): void {
  event?.preventDefault();
  event?.stopPropagation();
  desktopViewScale = 1;
  desktopViewPanX = 0;
  desktopViewPanY = 0;
  desktopPinch = null;
  desktopTouches.clear();
  if (desktopVncActive && vnc) {
    const sessionBridge = vnc as unknown as VncSessionBridge;
    sessionBridge.dragViewport = false;
    vnc.scaleViewport = true;
    sessionBridge.clipViewport = false;
    desktopVncFitScale = vncDisplayBridge(vnc)?.scale || 1;
  }
  applyDesktopViewTransform();
  if (desktopVideoReady) setDesktopState("LIVE");
}

function desktopKeyEvent(event: KeyboardEvent): void {
  const frame = document.querySelector<HTMLDivElement>("#desktop-frame");
  const mobileInput = document.querySelector<HTMLTextAreaElement>("#desktop-mobile-input");
  const activeElement = document.activeElement;
  if (!frame || (activeElement !== frame && activeElement !== mobileInput)) return;
  if (desktopVncActive && activeElement !== mobileInput) return;
  if (activeElement === mobileInput && (!event.code || event.code === "Unidentified")) return;
  event.preventDefault();
  if (activeElement === mobileInput) event.stopImmediatePropagation();
  if (!desktopVncActive && event.code === "F8") {
    desktopSuperHeld = event.type === "keydown";
    return;
  }
  if (event.code === "Escape" && activeElement === frame) {
    if (event.type === "keydown") releaseDesktopControl();
    return;
  }
  if (desktopVncActive) {
    sendVncKeyboardEvent(event);
    return;
  }
  sendDesktopInput({
    type: "key",
    code: event.code,
    state: event.type === "keydown" ? "pressed" : "released",
    repeat: event.repeat,
    ctrl: event.ctrlKey,
    alt: event.altKey,
    shift: event.shiftKey,
    meta: event.metaKey || desktopSuperHeld,
  });
}

function releaseDesktopSuperKey(): void {
  desktopSuperHeld = false;
}

function focusDesktopKeyboard(): void {
  const input = document.querySelector<HTMLTextAreaElement>("#desktop-mobile-input");
  if (!input || !desktopVideoReady) return;
  if (document.pointerLockElement) document.exitPointerLock();
  input.value = "";
  input.focus({ preventScroll: true });
  setDesktopState("MOBILE KEYBOARD");
}

function desktopMobileInput(event: Event): void {
  if ((event as InputEvent).isComposing) return;
  flushDesktopMobileInput();
}

function flushDesktopMobileInput(): void {
  const input = document.querySelector<HTMLTextAreaElement>("#desktop-mobile-input");
  if (!input?.value) return;
  sendDesktopText(input.value);
  input.value = "";
}

function desktopMobileBeforeInput(event: InputEvent): void {
  const code = {
    deleteContentBackward: "Backspace",
    deleteContentForward: "Delete",
    insertLineBreak: "Enter",
    insertParagraph: "Enter",
  }[event.inputType];
  if (!code) return;
  event.preventDefault();
  sendDesktopKeyTap(code);
}

function sendDesktopText(text: string): void {
  if (!desktopVideoReady || !text) return;
  if (desktopVncActive) {
    for (const character of text) {
      const codePoint = character.codePointAt(0);
      if (codePoint === undefined) continue;
      const keysym = unicodeKeysym(codePoint);
      vnc?.sendKey(keysym, "", true);
      vnc?.sendKey(keysym, "", false);
    }
    return;
  }
  for (let offset = 0; offset < text.length; offset += 128) {
    sendDesktopInput({ type: "text", text: text.slice(offset, offset + 128) });
  }
}

function sendDesktopKeyTap(code: string): void {
  if (desktopVncActive) {
    const keysym = vncKeysymForCode(code);
    if (keysym === null) return;
    vnc?.sendKey(keysym, code, true);
    vnc?.sendKey(keysym, code, false);
    return;
  }
  sendDesktopInput({ type: "key", code, state: "pressed", repeat: false });
  sendDesktopInput({ type: "key", code, state: "released", repeat: false });
}

function sendVncKeyboardEvent(event: KeyboardEvent): void {
  const keysym = vncKeysymForKeyboardEvent(event);
  if (keysym === null) return;
  vnc?.sendKey(keysym, event.code, event.type === "keydown");
}

function vncKeysymForKeyboardEvent(event: KeyboardEvent): number | null {
  if (event.key.length === 1) return unicodeKeysym(event.key.codePointAt(0) || 0);
  const side = event.location === KeyboardEvent.DOM_KEY_LOCATION_RIGHT ? "Right" : "Left";
  const modifier = {
    Shift: side === "Right" ? 0xffe2 : 0xffe1,
    Control: side === "Right" ? 0xffe4 : 0xffe3,
    Alt: side === "Right" ? 0xffea : 0xffe9,
    Meta: side === "Right" ? 0xffec : 0xffeb,
  }[event.key];
  if (modifier !== undefined) return modifier;
  return vncKeysymForCode(event.code);
}

function vncKeysymForCode(code: string): number | null {
  const special: Record<string, number> = {
    Backspace: 0xff08, Tab: 0xff09, Enter: 0xff0d, NumpadEnter: 0xff0d,
    Escape: 0xff1b, Home: 0xff50, ArrowLeft: 0xff51, ArrowUp: 0xff52,
    ArrowRight: 0xff53, ArrowDown: 0xff54, PageUp: 0xff55, PageDown: 0xff56,
    End: 0xff57, Insert: 0xff63, Delete: 0xffff,
  };
  if (special[code] !== undefined) return special[code];
  const functionKey = /^F(\d{1,2})$/.exec(code);
  if (functionKey) {
    const number = Number(functionKey[1]);
    if (number >= 1 && number <= 12) return 0xffbd + number;
  }
  return null;
}

function unicodeKeysym(codePoint: number): number {
  return codePoint >= 0x20 && codePoint <= 0xff ? codePoint : 0x01000000 | codePoint;
}

function vncSuperKeyEvent(event: KeyboardEvent): void {
  if (!desktopVncActive || !desktopVideoReady || event.code !== "F8" || !vnc) return;
  event.preventDefault();
  event.stopImmediatePropagation();
  if (event.type === "keydown") {
    if (vncSuperHeld || event.repeat) return;
    vncSuperHeld = true;
    vnc.sendKey(remoteSuperKeysym, "MetaLeft", true);
  } else {
    releaseVncSuperKey();
  }
}

function releaseVncSuperKey(): void {
  if (!vncSuperHeld) return;
  vncSuperHeld = false;
  vnc?.sendKey(remoteSuperKeysym, "MetaLeft", false);
}

function desktopPointerLockChange(): void {
  const frame = document.querySelector<HTMLDivElement>("#desktop-frame");
  if (frame && document.pointerLockElement === frame) {
    desktopControlActive = true;
    setDesktopState("CONTROL / ESC TO EXIT");
  } else if (desktopControlActive) {
    releaseDesktopControl(false);
  }
}

function releaseDesktopControl(exitPointerLock = true): void {
  if (!desktopControlActive) return;
  desktopControlActive = false;
  if (desktopVideoReady) sendDesktopInput({ type: "release_control" });
  if (exitPointerLock && document.pointerLockElement) document.exitPointerLock();
  document.querySelector<HTMLDivElement>("#desktop-frame")?.blur();
  setDesktopState("LIVE");
}

function remoteVideoMetrics(): { left: number; top: number; width: number; height: number; sourceWidth: number; sourceHeight: number } | null {
  const frame = document.querySelector<HTMLDivElement>("#desktop-frame");
  const video = document.querySelector<HTMLVideoElement>("#desktop-video");
  const image = document.querySelector<HTMLImageElement>("#desktop-fallback-image");
  if (!frame || !video || !image) return null;
  const sourceWidth = desktopFallbackActive ? (image.naturalWidth || 1280) : (video.videoWidth || 1280);
  const sourceHeight = desktopFallbackActive ? (image.naturalHeight || 720) : (video.videoHeight || 720);
  const frameWidth = frame.clientWidth;
  const frameHeight = frame.clientHeight;
  if (!frameWidth || !frameHeight) return null;
  const scale = Math.min(frameWidth / sourceWidth, frameHeight / sourceHeight);
  const baseWidth = sourceWidth * scale;
  const baseHeight = sourceHeight * scale;
  const baseLeft = (frameWidth - baseWidth) / 2;
  const baseTop = (frameHeight - baseHeight) / 2;
  const width = baseWidth * desktopViewScale;
  const height = baseHeight * desktopViewScale;
  return {
    left: frameWidth / 2 + (baseLeft - frameWidth / 2) * desktopViewScale + desktopViewPanX,
    top: frameHeight / 2 + (baseTop - frameHeight / 2) * desktopViewScale + desktopViewPanY,
    width,
    height,
    sourceWidth,
    sourceHeight,
  };
}

function remotePositionFromClient(clientX: number, clientY: number): { x: number; y: number } | null {
  const frame = document.querySelector<HTMLDivElement>("#desktop-frame");
  const metrics = remoteVideoMetrics();
  if (!frame || !metrics) return null;
  const rect = frame.getBoundingClientRect();
  return {
    x: Math.max(0, Math.min(metrics.sourceWidth - 1, ((clientX - rect.left - metrics.left) / metrics.width) * metrics.sourceWidth)),
    y: Math.max(0, Math.min(metrics.sourceHeight - 1, ((clientY - rect.top - metrics.top) / metrics.height) * metrics.sourceHeight)),
  };
}

function sendDesktopInput(message: Record<string, unknown>): void {
  if (!desktopVideoReady) return;
  const payload = JSON.stringify(message);
  if (desktopFallbackActive && socket?.readyState === WebSocket.OPEN) socket.send(payload);
  else if (inputChannel?.readyState === "open") inputChannel.send(payload);
}

function setDesktopState(value: string): void {
  const state = document.querySelector<HTMLElement>("#desktop-state");
  if (state) state.textContent = value;
  const label = document.querySelector<HTMLElement>("#connection-label");
  if (label) label.textContent = value === "LIVE" ? "DESKTOP LIVE" : value;
}

async function wakeScreens(event: Event): Promise<void> {
  const button = event.currentTarget as HTMLButtonElement;
  const original = "WAKE SCREENS";
  button.disabled = true;
  button.classList.remove("success");
  button.textContent = "WAKING…";
  try {
    const response = await fetch(gateUrl("api/displays/wake"), { method: "POST" });
    if (response.status === 401) return renderLogin("Your gate session expired.");
    const body = (await response.json()) as DisplayWakeResult & { error?: string };
    if (!response.ok) throw new Error(body.error || "The displays did not wake.");
    const lockState = body.locked ? "LOCKED" : "UNLOCKED";
    button.textContent = `AWAKE · ${lockState}`;
    button.title = `${body.woken.join(", ")} awake${body.hold_seconds ? ` for ${body.hold_seconds} seconds` : ""}`;
    button.classList.add("success");
  } catch (error) {
    button.textContent = "WAKE FAILED";
    button.title = error instanceof Error ? error.message : "The displays did not wake.";
  } finally {
    window.setTimeout(() => {
      if (!button.isConnected) return;
      button.disabled = false;
      button.classList.remove("success");
      button.textContent = original;
    }, 3500);
  }
}

function showDesktopMessage(message: string): void {
  const empty = document.querySelector<HTMLElement>("#desktop-empty");
  if (!empty) return;
  empty.hidden = false;
  const strong = empty.querySelector("strong");
  if (strong) strong.textContent = message;
}

function hideDesktopMessage(): void {
  const empty = document.querySelector<HTMLElement>("#desktop-empty");
  if (empty) empty.hidden = true;
}

async function login(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  const form = event.currentTarget as HTMLFormElement;
  const button = form.querySelector<HTMLButtonElement>("button");
  const input = form.elements.namedItem("token") as HTMLInputElement;
  const message = form.querySelector<HTMLParagraphElement>("#login-message");
  if (button) button.disabled = true;
  if (message) message.textContent = "Opening secure session…";

  try {
    const response = await fetch(gateUrl("api/login"), {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ token: input.value }),
    });
    if (!response.ok) throw new Error("The token did not open this gate.");
    renderTerminal();
  } catch (error) {
    if (message) message.textContent = error instanceof Error ? error.message : "Login failed.";
    input.select();
  } finally {
    if (button) button.disabled = false;
  }
}

async function logout(): Promise<void> {
  await fetch(gateUrl("api/logout"), { method: "POST" });
  renderLogin("Gate closed.");
}

function connectSocket(): void {
  if (!terminal || !fitAddon) return;
  const url = gateWebSocket("ws/terminal");
  url.searchParams.set("session", activeTerminalSession);
  socket = new WebSocket(url);
  socket.binaryType = "arraybuffer";
  setSocketState("CONNECTING");

  socket.addEventListener("open", () => {
    setSocketState("LIVE");
    fitAddon?.fit();
    if (terminal) sendResize({ cols: terminal.cols, rows: terminal.rows });
    terminal?.focus();
  });
  socket.addEventListener("message", (event) => {
    if (typeof event.data === "string") terminal?.write(event.data);
    else terminal?.write(decoder.decode(event.data));
  });
  socket.addEventListener("close", (event) => {
    setSocketState(event.code === 4401 ? "LOCKED" : "RECONNECTING");
    if (event.code === 4401) {
      renderLogin("Your gate session expired.");
      return;
    }
    window.setTimeout(() => {
      if (terminal && socket?.readyState === WebSocket.CLOSED) connectSocket();
    }, 1200);
  });
}

function sendResize(size: { cols: number; rows: number }): void {
  if (socket?.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify({ type: "resize", cols: size.cols, rows: size.rows }));
  }
}

function fitTerminal(): void {
  fitAddon?.fit();
}

function setSocketState(value: string): void {
  const state = document.querySelector<HTMLElement>("#socket-state");
  if (state) state.textContent = value;
  const label = document.querySelector<HTMLElement>("#connection-label");
  if (label) label.textContent = value === "LIVE" ? "GATE ONLINE" : value;
}

function disposeViews(): void {
  disposeTerminal();
  disposeDesktop();
}

function disposeTerminal(): void {
  if (terminalSessionRefreshTimer !== null) window.clearInterval(terminalSessionRefreshTimer);
  terminalSessionRefreshTimer = null;
  window.removeEventListener("resize", fitTerminal);
  socket?.close();
  socket = null;
  terminal?.dispose();
  terminal = null;
  fitAddon = null;
}

function disposeDesktop(): void {
  releaseVncSuperKey();
  releaseDesktopSuperKey();
  window.removeEventListener("keydown", vncSuperKeyEvent, { capture: true });
  window.removeEventListener("keyup", vncSuperKeyEvent, { capture: true });
  window.removeEventListener("blur", releaseVncSuperKey);
  releaseDesktopControl();
  closeClipboardPanel();
  clearClipboardPreview();
  remoteClipboardBlob = null;
  window.removeEventListener("keydown", desktopKeyEvent, { capture: true });
  window.removeEventListener("keyup", desktopKeyEvent, { capture: true });
  window.removeEventListener("blur", releaseDesktopSuperKey);
  document.removeEventListener("pointerlockchange", desktopPointerLockChange);
  inputChannel?.close();
  inputChannel = null;
  peer?.close();
  peer = null;
  if (!terminal) {
    socket?.close();
    socket = null;
  }
  remoteCandidates = [];
  desktopVideoReady = false;
  desktopControlActive = false;
  if (desktopTouchReleaseTimers.size > 0) {
    for (const timer of desktopTouchReleaseTimers) window.clearTimeout(timer);
    desktopTouchReleaseTimers.clear();
    if (desktopVideoReady && !desktopVncActive) {
      sendDesktopInput({ type: "pointer_button", button: 0, state: "released" });
    }
  }
  resetDesktopView();
  const mobileInput = document.querySelector<HTMLTextAreaElement>("#desktop-mobile-input");
  if (mobileInput) {
    mobileInput.value = "";
    mobileInput.blur();
  }
  desktopFallbackActive = false;
  desktopVncActive = false;
  vnc?.disconnect();
  vnc = null;
  if (desktopFallbackFrameUrl) URL.revokeObjectURL(desktopFallbackFrameUrl);
  desktopFallbackFrameUrl = null;
  const fallbackImage = document.querySelector<HTMLImageElement>("#desktop-fallback-image");
  if (fallbackImage) {
    fallbackImage.removeAttribute("src");
    fallbackImage.hidden = true;
  }
  const video = document.querySelector<HTMLVideoElement>("#desktop-video");
  if (video) {
    video.srcObject = null;
    video.hidden = false;
  }
  const vncTarget = document.querySelector<HTMLDivElement>("#desktop-vnc");
  if (vncTarget) {
    vncTarget.replaceChildren();
    vncTarget.hidden = true;
  }
  activeDesktopIceServers = [];
  if (desktopNegotiationTimer !== null) window.clearTimeout(desktopNegotiationTimer);
  desktopNegotiationTimer = null;
  if (desktopVncRetryTimer !== null) window.clearTimeout(desktopVncRetryTimer);
  desktopVncRetryTimer = null;
}

function escapeHtml(value: string): string {
  const node = document.createElement("div");
  node.textContent = value;
  return node.innerHTML;
}

async function boot(): Promise<void> {
  try {
    const response = await fetch(gateUrl("api/status"));
    const status = (await response.json()) as Status;
    proxiedSession = status.auth_mode === "eutheroxide_proxy";
    status.authenticated ? renderTerminal() : renderLogin();
  } catch {
    renderLogin("The local gateway is not responding.");
  }
}

void boot();
