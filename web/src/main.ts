import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import "./style.css";

type Status = {
  authenticated: boolean;
  terminal_ready: boolean;
};

const appNode = document.querySelector<HTMLDivElement>("#app");
if (!appNode) throw new Error("Missing #app");
const app: HTMLDivElement = appNode;

const encoder = new TextEncoder();
const decoder = new TextDecoder();
let socket: WebSocket | null = null;
let terminal: Terminal | null = null;
let fitAddon: FitAddon | null = null;

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
  disposeTerminal();
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
  app.innerHTML = gateShell(`
    <section class="workspace">
      <div class="workspace-bar">
        <div>
          <span class="eyebrow">GATE SHELL / SESSION 01</span>
          <h1>Forge terminal</h1>
        </div>
        <div class="actions">
          <span id="socket-state" class="socket-state">CONNECTING</span>
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
  connectSocket();
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

function disposeTerminal(): void {
  window.removeEventListener("resize", fitTerminal);
  socket?.close();
  socket = null;
  terminal?.dispose();
  terminal = null;
  fitAddon = null;
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
