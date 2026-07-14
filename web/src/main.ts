import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import "./style.css";

type Status = {
  authenticated: boolean;
  terminal_ready: boolean;
};

type DesktopStatus = {
  available: boolean;
  active: boolean;
  viewer_connected: boolean;
  output: string;
  mode: string;
  workspace: number;
  transport: string;
  input: string;
  virtual_output: boolean;
  outputs: DesktopOutput[];
};

type DesktopOutput = {
  name: string;
  description: string;
  mode: string;
  workspace: number;
  virtual_output: boolean;
};

const appNode = document.querySelector<HTMLDivElement>("#app");
if (!appNode) throw new Error("Missing #app");
const app: HTMLDivElement = appNode;

const encoder = new TextEncoder();
const decoder = new TextDecoder();
let socket: WebSocket | null = null;
let terminal: Terminal | null = null;
let fitAddon: FitAddon | null = null;
let peer: RTCPeerConnection | null = null;
let inputChannel: RTCDataChannel | null = null;
let remoteCandidates: RTCIceCandidateInit[] = [];
let desktopVideoReady = false;
let desktopNegotiationTimer: number | null = null;
let desktopControlActive = false;

function gateShell(content: string): string {
  return `
    <main class="gate-shell">
      <header class="topbar">
        <a class="brand" href="/" aria-label="EutherGate home">
          <span class="brand-mark" aria-hidden="true"><i></i></span>
          <span><strong>Euther</strong>Gate</span>
        </a>
        <div class="gate-state"><span class="pulse"></span><span id="connection-label">LOCAL GATE</span></div>
      </header>
      ${content}
    </main>`;
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
          <span id="socket-state" class="socket-state">CONNECTING</span>
          <button id="show-desktop" class="ghost-button primary-action" type="button">DESKTOP</button>
          <button id="logout" class="ghost-button" type="button">CLOSE GATE</button>
        </div>
      </div>
      <div class="terminal-frame">
        <div class="terminal-chrome"><span></span><span></span><span></span><b>euthergate://local/shell</b></div>
        <div id="terminal" aria-label="EutherGate terminal"></div>
      </div>
      <p class="hint">The shell remains alive when this page is reloaded.</p>
    </section>`);

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

  terminal.onData((data) => {
    if (socket?.readyState === WebSocket.OPEN) socket.send(encoder.encode(data));
  });
  terminal.onResize(sendResize);
  window.addEventListener("resize", fitTerminal);
  document.querySelector<HTMLButtonElement>("#logout")?.addEventListener("click", logout);
  document.querySelector<HTMLButtonElement>("#show-desktop")?.addEventListener("click", renderDesktop);
  connectSocket();
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
          <select id="desktop-output-picker" class="output-picker" aria-label="Wayland output" disabled></select>
          <button id="desktop-terminal" class="ghost-button" type="button" disabled>OPEN TERMINAL</button>
          <button id="show-terminal" class="ghost-button" type="button">TERMINAL</button>
          <button id="desktop-power" class="ghost-button primary-action" type="button" disabled>START DESKTOP</button>
        </div>
      </div>
      <div class="desktop-frame" id="desktop-frame" tabindex="0">
        <video id="desktop-video" autoplay playsinline muted></video>
        <div class="desktop-empty" id="desktop-empty">
          <span class="brand-mark large" aria-hidden="true"><i></i></span>
          <strong>Virtual Wayland output offline</strong>
          <p>Start the headless forge, then video and input travel over WebRTC.</p>
        </div>
        <div class="stream-hud">
          <span id="desktop-output">EUTHERGATE-1</span>
          <span id="desktop-transport">VP8 / DATACHANNEL</span>
        </div>
      </div>
      <div class="desktop-footer">
        <span>Click to enter remote control · Esc returns to the local desktop.</span>
        <span id="desktop-mode">1280×720 @ 30</span>
      </div>
    </section>`);

  document.querySelector<HTMLButtonElement>("#show-terminal")?.addEventListener("click", renderTerminal);
  document.querySelector<HTMLButtonElement>("#desktop-power")?.addEventListener("click", toggleDesktop);
  document.querySelector<HTMLButtonElement>("#desktop-terminal")?.addEventListener("click", launchDesktopTerminal);
  document.querySelector<HTMLSelectElement>("#desktop-output-picker")?.addEventListener("change", switchDesktopOutput);
  installDesktopInput();

  try {
    const response = await fetch("/api/desktop/status");
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
      const label = candidate.virtual_output
        ? `${candidate.name} — Virtual Forge`
        : `${candidate.name} — ${candidate.description}`;
      return `<option value="${escapeHtml(candidate.name)}"${candidate.name === status.output ? " selected" : ""}>${escapeHtml(label)}</option>`;
    }).join("");
    picker.disabled = false;
  }
  const terminalButton = document.querySelector<HTMLButtonElement>("#desktop-terminal");
  if (terminalButton) terminalButton.disabled = !status.active;
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
    const startUrl = selectedOutput ? `/api/desktop/start?output=${encodeURIComponent(selectedOutput)}` : "/api/desktop/start";
    const response = await fetch(active ? "/api/desktop/stop" : startUrl, { method: "POST" });
    const body = (await response.json()) as { active?: boolean; error?: string };
    if (!response.ok) throw new Error(body.error || "Desktop transition failed.");
    button.dataset.active = String(!active);
    button.textContent = active ? "START DESKTOP" : "STOP DESKTOP";
    if (active) {
      disposeDesktop();
      setDesktopState("OFFLINE");
      showDesktopMessage("Virtual Wayland output offline");
    } else {
      const terminalButton = document.querySelector<HTMLButtonElement>("#desktop-terminal");
      if (terminalButton) terminalButton.disabled = false;
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
    const response = await fetch(`/api/desktop/start?output=${encodeURIComponent(picker.value)}`, { method: "POST" });
    const body = (await response.json()) as { error?: string };
    if (!response.ok) throw new Error(body.error || "Output switch failed.");
    const statusResponse = await fetch("/api/desktop/status");
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

async function launchDesktopTerminal(): Promise<void> {
  const button = document.querySelector<HTMLButtonElement>("#desktop-terminal");
  if (button) button.disabled = true;
  try {
    const response = await fetch("/api/desktop/launch-terminal", { method: "POST" });
    const body = (await response.json()) as { error?: string };
    if (!response.ok) throw new Error(body.error || "Could not launch terminal.");
  } catch (error) {
    setDesktopState("LAUNCH FAILED");
    showDesktopMessage(error instanceof Error ? error.message : "Could not launch terminal.");
  } finally {
    if (button) button.disabled = false;
  }
}

function connectDesktop(): void {
  disposeDesktop();
  const protocol = location.protocol === "https:" ? "wss:" : "ws:";
  socket = new WebSocket(`${protocol}//${location.host}/ws/desktop`);
  remoteCandidates = [];
  desktopVideoReady = false;
  setDesktopState("NEGOTIATING");

  peer = new RTCPeerConnection({ bundlePolicy: "max-bundle" });
  peer.addTransceiver("video", { direction: "recvonly" });
  inputChannel = peer.createDataChannel("input", { ordered: false, maxRetransmits: 0 });
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
    if (event.candidate && socket?.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify({ type: "ice", ...event.candidate.toJSON() }));
    }
  });
  peer.addEventListener("connectionstatechange", () => {
    if (peer?.connectionState === "connected") {
      setDesktopState(desktopVideoReady ? (inputChannel?.readyState === "open" ? "LIVE" : "VIDEO") : "WAITING FOR VIDEO");
    }
    if (["failed", "disconnected", "closed"].includes(peer?.connectionState || "")) setDesktopState("RECONNECTING");
  });

  socket.addEventListener("open", async () => {
    if (!peer || socket?.readyState !== WebSocket.OPEN) return;
    const offer = await peer.createOffer();
    await peer.setLocalDescription(offer);
    socket.send(JSON.stringify({ type: "offer", sdp: offer.sdp }));
    desktopNegotiationTimer = window.setTimeout(() => {
      if (!desktopVideoReady) {
        setDesktopState("NO VIDEO");
        showDesktopMessage("WebRTC connected without a video track. Reload and try again.");
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

function markDesktopVideoReady(): void {
  desktopVideoReady = true;
  if (desktopNegotiationTimer !== null) window.clearTimeout(desktopNegotiationTimer);
  desktopNegotiationTimer = null;
  hideDesktopMessage();
  setDesktopState(inputChannel?.readyState === "open" ? "LIVE" : "VIDEO");
}

function installDesktopInput(): void {
  const frame = document.querySelector<HTMLDivElement>("#desktop-frame");
  const video = document.querySelector<HTMLVideoElement>("#desktop-video");
  if (!frame || !video) return;
  frame.addEventListener("pointermove", (event) => {
    if (!desktopControlActive) return;
    if (document.pointerLockElement === frame) {
      sendDesktopInput({ type: "pointer_delta", dx: event.movementX, dy: event.movementY });
      return;
    }
    const rect = video.getBoundingClientRect();
    if (!rect.width || !rect.height) return;
    sendDesktopInput({
      type: "pointer_move",
      x: ((event.clientX - rect.left) / rect.width) * (video.videoWidth || 1280),
      y: ((event.clientY - rect.top) / rect.height) * (video.videoHeight || 720),
    });
  });
  frame.addEventListener("pointerdown", (event) => {
    if (!desktopVideoReady) return;
    event.preventDefault();
    desktopControlActive = true;
    frame.focus();
    if (document.pointerLockElement !== frame) frame.requestPointerLock();
    sendDesktopInput({ type: "pointer_button", button: event.button, state: "pressed" });
  });
  frame.addEventListener("pointerup", (event) => {
    sendDesktopInput({ type: "pointer_button", button: event.button, state: "released" });
  });
  frame.addEventListener("contextmenu", (event) => event.preventDefault());
  frame.addEventListener("wheel", (event) => {
    event.preventDefault();
    sendDesktopInput({ type: "wheel", dx: event.deltaX, dy: event.deltaY });
  }, { passive: false });
  window.addEventListener("keydown", desktopKeyEvent, { capture: true });
  window.addEventListener("keyup", desktopKeyEvent, { capture: true });
  document.addEventListener("pointerlockchange", desktopPointerLockChange);
}

function desktopKeyEvent(event: KeyboardEvent): void {
  const frame = document.querySelector<HTMLDivElement>("#desktop-frame");
  if (!frame || document.activeElement !== frame) return;
  event.preventDefault();
  if (event.code === "Escape") {
    if (event.type === "keydown") releaseDesktopControl();
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
    meta: event.metaKey,
  });
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
  if (desktopVideoReady && inputChannel?.readyState === "open") {
    inputChannel.send(JSON.stringify({ type: "release_control" }));
  }
  if (exitPointerLock && document.pointerLockElement) document.exitPointerLock();
  document.querySelector<HTMLDivElement>("#desktop-frame")?.blur();
  setDesktopState("LIVE");
}

function sendDesktopInput(message: Record<string, unknown>): void {
  if (desktopVideoReady && inputChannel?.readyState === "open") inputChannel.send(JSON.stringify(message));
}

function setDesktopState(value: string): void {
  const state = document.querySelector<HTMLElement>("#desktop-state");
  if (state) state.textContent = value;
  const label = document.querySelector<HTMLElement>("#connection-label");
  if (label) label.textContent = value === "LIVE" ? "DESKTOP LIVE" : value;
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
    const response = await fetch("/api/login", {
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
  await fetch("/api/logout", { method: "POST" });
  renderLogin("Gate closed.");
}

function connectSocket(): void {
  if (!terminal || !fitAddon) return;
  const protocol = location.protocol === "https:" ? "wss:" : "ws:";
  socket = new WebSocket(`${protocol}//${location.host}/ws/terminal`);
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
  window.removeEventListener("resize", fitTerminal);
  socket?.close();
  socket = null;
  terminal?.dispose();
  terminal = null;
  fitAddon = null;
}

function disposeDesktop(): void {
  releaseDesktopControl();
  window.removeEventListener("keydown", desktopKeyEvent, { capture: true });
  window.removeEventListener("keyup", desktopKeyEvent, { capture: true });
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
  if (desktopNegotiationTimer !== null) window.clearTimeout(desktopNegotiationTimer);
  desktopNegotiationTimer = null;
}

function escapeHtml(value: string): string {
  const node = document.createElement("div");
  node.textContent = value;
  return node.innerHTML;
}

async function boot(): Promise<void> {
  try {
    const response = await fetch("/api/status");
    const status = (await response.json()) as Status;
    status.authenticated ? renderTerminal() : renderLogin();
  } catch {
    renderLogin("The local gateway is not responding.");
  }
}

void boot();
