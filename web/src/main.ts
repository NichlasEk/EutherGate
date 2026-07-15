import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import "./style.css";

type Status = {
  authenticated: boolean;
  terminal_ready: boolean;
  auth_mode: "none" | "gate_cookie" | "eutheroxide_proxy";
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
let desktopIceServers: RTCIceServer[] = [];
let desktopVideoReady = false;
let desktopNegotiationTimer: number | null = null;
let desktopControlActive = false;
let proxiedSession = false;
let clipboardPreviewUrl: string | null = null;
let remoteClipboardBlob: Blob | null = null;

const clipboardLimit = 8 * 1024 * 1024;

const gateRoot = new URL("./", document.baseURI);

function gateUrl(path: string): URL {
  return new URL(path.replace(/^\//, ""), gateRoot);
}

function gateWebSocket(path: string): URL {
  const url = gateUrl(path);
  url.protocol = location.protocol === "https:" ? "wss:" : "ws:";
  return url;
}

function gateShell(content: string): string {
  return `
    <main class="gate-shell">
      <header class="topbar">
        <a class="brand" href="${escapeHtml(gateRoot.pathname)}" aria-label="EutherGate home">
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
          <button class="ghost-button wake-screens" type="button">WAKE SCREENS</button>
          <button id="show-desktop" class="ghost-button primary-action" type="button">DESKTOP</button>
          ${proxiedSession ? "" : '<button id="logout" class="ghost-button" type="button">CLOSE GATE</button>'}
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
  document.querySelector<HTMLButtonElement>(".wake-screens")?.addEventListener("click", wakeScreens);
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
          <button class="ghost-button wake-screens" type="button">WAKE SCREENS</button>
          <select id="desktop-output-picker" class="output-picker" aria-label="Wayland output" disabled></select>
          <button id="desktop-terminal" class="ghost-button" type="button" disabled>OPEN TERMINAL</button>
          <button id="desktop-clipboard" class="ghost-button" type="button" disabled>CLIPBOARD</button>
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
  document.querySelector<HTMLButtonElement>(".wake-screens")?.addEventListener("click", wakeScreens);
  document.querySelector<HTMLButtonElement>("#desktop-power")?.addEventListener("click", toggleDesktop);
  document.querySelector<HTMLButtonElement>("#desktop-terminal")?.addEventListener("click", launchDesktopTerminal);
  document.querySelector<HTMLButtonElement>("#desktop-clipboard")?.addEventListener("click", openClipboardPanel);
  document.querySelector<HTMLSelectElement>("#desktop-output-picker")?.addEventListener("change", switchDesktopOutput);
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
  const terminalButton = document.querySelector<HTMLButtonElement>("#desktop-terminal");
  if (terminalButton) terminalButton.disabled = !status.active;
  const clipboardButton = document.querySelector<HTMLButtonElement>("#desktop-clipboard");
  if (clipboardButton) clipboardButton.disabled = !status.active;
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
      setDesktopState("OFFLINE");
      showDesktopMessage("Virtual Wayland output offline");
    } else {
      const terminalButton = document.querySelector<HTMLButtonElement>("#desktop-terminal");
      if (terminalButton) terminalButton.disabled = false;
      const clipboardButton = document.querySelector<HTMLButtonElement>("#desktop-clipboard");
      if (clipboardButton) clipboardButton.disabled = false;
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

async function launchDesktopTerminal(): Promise<void> {
  const button = document.querySelector<HTMLButtonElement>("#desktop-terminal");
  if (button) button.disabled = true;
  try {
    const response = await fetch(gateUrl("api/desktop/launch-terminal"), { method: "POST" });
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
  socket = new WebSocket(gateWebSocket("ws/desktop"));
  remoteCandidates = [];
  desktopVideoReady = false;
  setDesktopState("NEGOTIATING");

  peer = new RTCPeerConnection({ bundlePolicy: "max-bundle", iceServers: desktopIceServers });
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
    const position = remotePositionFromClient(event.clientX, event.clientY);
    if (!position) return;
    sendDesktopInput({
      type: "pointer_move",
      x: position.x,
      y: position.y,
    });
  });
  frame.addEventListener("pointerdown", (event) => {
    if (!desktopVideoReady) return;
    event.preventDefault();
    const position = remotePositionFromClient(event.clientX, event.clientY);
    if (position) {
      sendDesktopInput({ type: "pointer_move", x: position.x, y: position.y });
    }
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

function remoteVideoMetrics(): { left: number; top: number; width: number; height: number; sourceWidth: number; sourceHeight: number } | null {
  const frame = document.querySelector<HTMLDivElement>("#desktop-frame");
  const video = document.querySelector<HTMLVideoElement>("#desktop-video");
  if (!frame || !video) return null;
  const sourceWidth = video.videoWidth || 1280;
  const sourceHeight = video.videoHeight || 720;
  const frameWidth = frame.clientWidth;
  const frameHeight = frame.clientHeight;
  if (!frameWidth || !frameHeight) return null;
  const scale = Math.min(frameWidth / sourceWidth, frameHeight / sourceHeight);
  const width = sourceWidth * scale;
  const height = sourceHeight * scale;
  return {
    left: (frameWidth - width) / 2,
    top: (frameHeight - height) / 2,
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
  if (desktopVideoReady && inputChannel?.readyState === "open") inputChannel.send(JSON.stringify(message));
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
  socket = new WebSocket(gateWebSocket("ws/terminal"));
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
  closeClipboardPanel();
  clearClipboardPreview();
  remoteClipboardBlob = null;
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
    const response = await fetch(gateUrl("api/status"));
    const status = (await response.json()) as Status;
    proxiedSession = status.auth_mode === "eutheroxide_proxy";
    status.authenticated ? renderTerminal() : renderLogin();
  } catch {
    renderLogin("The local gateway is not responding.");
  }
}

void boot();
