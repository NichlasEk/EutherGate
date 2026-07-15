#!/usr/bin/env python3
"""GStreamer WebRTC bridge for one EutherGate Wayland output.

Signaling is newline-delimited JSON on stdin/stdout. Desktop input arrives on a
WebRTC DataChannel and is injected through compositor-local IPC.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import re
import signal
import subprocess
import sys
import threading
import time
from dataclasses import dataclass

import gi

gi.require_version("Gst", "1.0")
gi.require_version("GstSdp", "1.0")
gi.require_version("GstWebRTC", "1.0")
from gi.repository import GLib, Gst, GstSdp, GstWebRTC  # noqa: E402


@dataclass(frozen=True)
class Mode:
    width: int
    height: int
    fps: int


def parse_mode(value: str) -> Mode:
    dimensions, separator, rate = value.partition("@")
    width, x, height = dimensions.partition("x")
    if not separator or not x:
        raise ValueError("mode must look like 1280x720@30")
    mode = Mode(int(width), int(height), int(float(rate)))
    if not 320 <= mode.width <= 7680 or not 200 <= mode.height <= 4320:
        raise ValueError("desktop dimensions are outside the supported range")
    if not 1 <= mode.fps <= 60:
        raise ValueError("desktop frame rate must be between 1 and 60")
    return mode


class DesktopBridge:
    def __init__(self, backend: str, output: str, mode: Mode) -> None:
        self.backend = backend
        self.output = output
        self.mode = mode
        self.loop = GLib.MainLoop()
        self.running = threading.Event()
        self.running.set()
        self.input_events: queue.Queue[dict] = queue.Queue(maxsize=256)
        self.media_ready = threading.Event()
        self.pipeline = self._build_pipeline()
        self.source = self.pipeline.get_by_name("source")
        self.webrtc = self.pipeline.get_by_name("webrtc")
        self.webrtc.connect("on-ice-candidate", self._on_ice_candidate)
        self.webrtc.connect("on-data-channel", self._on_data_channel)

    def _build_pipeline(self):
        width, height, fps = self.mode.width, self.mode.height, self.mode.fps
        description = f"""
            appsrc name=source is-live=true block=true format=time do-timestamp=true
                caps=video/x-raw,format=RGB,width={width},height={height},framerate={fps}/1 !
            queue leaky=downstream max-size-buffers=1 !
            videoconvert n-threads=2 !
            vp8enc deadline=1 cpu-used=8 end-usage=cbr target-bitrate=5000000
                keyframe-max-dist={fps * 2} lag-in-frames=0 threads=4 !
            rtpvp8pay name=payloader picture-id-mode=15-bit pt=96 !
            webrtcbin name=webrtc bundle-policy=max-bundle latency=30
        """
        return Gst.parse_launch(" ".join(description.split()))

    def run(self) -> None:
        self.pipeline.set_state(Gst.State.PLAYING)
        threads = [
            threading.Thread(target=self._signaling_loop, name="signaling", daemon=True),
            threading.Thread(target=self._capture_loop, name="capture", daemon=True),
            threading.Thread(target=self._input_loop, name="input", daemon=True),
            threading.Thread(target=self._bus_loop, name="gstreamer-bus", daemon=True),
        ]
        for thread in threads:
            thread.start()
        emit({"type": "ready", "output": self.output, "codec": "VP8", "input": "datachannel"})
        try:
            self.loop.run()
        finally:
            self.running.clear()
            self.pipeline.set_state(Gst.State.NULL)

    def stop(self) -> None:
        self.running.clear()
        GLib.idle_add(self.loop.quit)

    def _signaling_loop(self) -> None:
        for line in sys.stdin:
            if not self.running.is_set():
                return
            try:
                message = json.loads(line)
                kind = message.get("type")
                if kind == "offer":
                    GLib.idle_add(self._accept_offer, message["sdp"])
                elif kind == "ice":
                    GLib.idle_add(
                        self.webrtc.emit,
                        "add-ice-candidate",
                        int(message.get("sdpMLineIndex", 0)),
                        message["candidate"],
                    )
                elif kind == "stop":
                    self.stop()
                    return
            except Exception as error:  # signaling errors must reach the browser
                emit({"type": "error", "message": str(error)})
        self.stop()

    def _accept_offer(self, sdp_text: str) -> bool:
        payload = offered_video_payload(sdp_text, "VP8")
        if payload is None:
            emit({"type": "error", "message": "Browser offer contains no VP8 video codec"})
            return False
        self.pipeline.get_by_name("payloader").set_property("pt", payload)
        emit({"type": "offer-info", "codec": "VP8", "payload": payload})
        result, sdp = GstSdp.SDPMessage.new()
        if result != GstSdp.SDPResult.OK:
            raise RuntimeError("could not allocate SDP message")
        result = GstSdp.sdp_message_parse_buffer(sdp_text.encode(), sdp)
        if result != GstSdp.SDPResult.OK:
            raise RuntimeError("browser sent invalid SDP")
        offer = GstWebRTC.WebRTCSessionDescription.new(GstWebRTC.WebRTCSDPType.OFFER, sdp)
        self.webrtc.emit("set-remote-description", offer, Gst.Promise.new())
        promise = Gst.Promise.new_with_change_func(self._on_answer_created, self.webrtc, None)
        self.webrtc.emit("create-answer", None, promise)
        return False

    def _on_answer_created(self, promise, webrtc, _unused) -> None:
        promise.wait()
        reply = promise.get_reply()
        answer = reply.get_value("answer")
        webrtc.emit("set-local-description", answer, Gst.Promise.new())
        emit({"type": "answer", "sdp": answer.sdp.as_text()})
        self.media_ready.set()

    def _on_ice_candidate(self, _webrtc, mline_index: int, candidate: str) -> None:
        emit({"type": "ice", "candidate": candidate, "sdpMLineIndex": mline_index})

    def _on_data_channel(self, _webrtc, channel) -> None:
        channel.connect("on-message-string", self._on_data_message)
        channel.connect("on-close", lambda *_: self._queue_release_control())
        emit({"type": "input-ready", "label": channel.get_property("label")})

    def _queue_release_control(self) -> None:
        try:
            self.input_events.put_nowait({"type": "release_control"})
        except queue.Full:
            pass

    def _on_data_message(self, _channel, payload: str) -> None:
        try:
            event = json.loads(payload)
            if isinstance(event, dict):
                try:
                    self.input_events.put_nowait(event)
                except queue.Full:
                    if event.get("type") in ("pointer_move", "pointer_delta"):
                        return
                    self.input_events.get_nowait()
                    self.input_events.put_nowait(event)
        except (ValueError, TypeError):
            return

    def _capture_loop(self) -> None:
        frame_duration = Gst.SECOND // self.mode.fps
        frame = 0
        next_frame = time.monotonic()
        while self.running.is_set() and not self.media_ready.wait(0.25):
            pass
        while self.running.is_set():
            try:
                capture = subprocess.run(
                    ["grim", "-c", "-o", self.output, "-t", "ppm", "-"],
                    check=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    timeout=2,
                )
                pixels = parse_ppm(capture.stdout, self.mode)
                buffer = Gst.Buffer.new_allocate(None, len(pixels), None)
                buffer.fill(0, pixels)
                buffer.pts = frame * frame_duration
                buffer.dts = buffer.pts
                buffer.duration = frame_duration
                result = self.source.emit("push-buffer", buffer)
                if result != Gst.FlowReturn.OK:
                    raise RuntimeError(f"appsrc rejected frame: {result}")
                frame += 1
            except Exception as error:
                emit({"type": "capture-warning", "message": str(error)})
                time.sleep(0.5)
            next_frame += 1 / self.mode.fps
            time.sleep(max(0, next_frame - time.monotonic()))

    def _input_loop(self) -> None:
        controller = InputController(self.backend, self.output, self.mode)
        deferred: dict | None = None
        while self.running.is_set():
            try:
                event = deferred or self.input_events.get(timeout=0.5)
                deferred = None
                if event.get("type") in ("pointer_move", "pointer_delta"):
                    controller.update_pointer(event)
                    while True:
                        try:
                            following = self.input_events.get_nowait()
                        except queue.Empty:
                            break
                        if following.get("type") not in ("pointer_move", "pointer_delta"):
                            deferred = following
                            break
                        controller.update_pointer(following)
                    controller.flush_pointer()
                else:
                    controller.inject(event)
            except queue.Empty:
                continue
            except Exception as error:
                emit({"type": "input-warning", "message": str(error)})

    def _bus_loop(self) -> None:
        bus = self.pipeline.get_bus()
        while self.running.is_set():
            message = bus.timed_pop_filtered(
                Gst.SECOND,
                Gst.MessageType.ERROR | Gst.MessageType.EOS | Gst.MessageType.WARNING,
            )
            if message is None:
                continue
            if message.type == Gst.MessageType.ERROR:
                error, debug = message.parse_error()
                emit({"type": "error", "message": str(error), "debug": debug or ""})
                self.stop()
                return
            if message.type == Gst.MessageType.WARNING:
                warning, _debug = message.parse_warning()
                emit({"type": "media-warning", "message": str(warning)})
            if message.type == Gst.MessageType.EOS:
                self.stop()
                return


def parse_ppm(data: bytes, mode: Mode) -> bytes:
    header_end = 0
    tokens: list[bytes] = []
    cursor = 0
    while len(tokens) < 4:
        while cursor < len(data) and chr(data[cursor]).isspace():
            cursor += 1
        if cursor < len(data) and data[cursor] == ord("#"):
            cursor = data.index(b"\n", cursor) + 1
            continue
        end = cursor
        while end < len(data) and not chr(data[end]).isspace():
            end += 1
        tokens.append(data[cursor:end])
        cursor = end
        header_end = cursor
    while header_end < len(data) and chr(data[header_end]).isspace():
        header_end += 1
    if tokens != [b"P6", str(mode.width).encode(), str(mode.height).encode(), b"255"]:
        raise ValueError(f"unexpected grim PPM header: {tokens!r}")
    pixels = data[header_end:]
    expected = mode.width * mode.height * 3
    if len(pixels) != expected:
        raise ValueError(f"grim returned {len(pixels)} RGB bytes; expected {expected}")
    return pixels


def offered_video_payload(sdp: str, codec: str) -> int | None:
    sections = re.split(r"(?=^m=)", sdp.replace("\r\n", "\n"), flags=re.MULTILINE)
    video = next((section for section in sections if section.startswith("m=video ")), None)
    if video is None:
        return None
    match = re.search(rf"^a=rtpmap:(\d+)\s+{re.escape(codec)}/90000\s*$", video, re.IGNORECASE | re.MULTILINE)
    return int(match.group(1)) if match else None


def output_geometry(backend: str, output: str) -> tuple[int, int]:
    command = ["hyprctl", "monitors", "all", "-j"] if backend == "hyprland" else ["swaymsg", "-t", "get_outputs", "-r"]
    result = subprocess.run(
        command,
        check=True,
        capture_output=True,
        text=True,
        timeout=2,
    )
    monitor = next(item for item in json.loads(result.stdout) if item["name"] == output)
    if backend == "hyprland":
        return int(monitor["x"]), int(monitor["y"])
    return int(monitor["rect"]["x"]), int(monitor["rect"]["y"])


class InputController:
    def __init__(self, backend: str, output: str, mode: Mode) -> None:
        self.backend = backend
        self.geometry = output_geometry(backend, output)
        self.mode = mode
        self.return_position = cursor_position() if backend == "hyprland" else (0, 0)
        self.remote_x = mode.width / 2
        self.remote_y = mode.height / 2

    def inject(self, event: dict) -> None:
        kind = event.get("type")
        if kind == "release_control":
            if self.backend == "hyprland":
                run_hyprctl(
                    "dispatch",
                    "movecursor",
                    str(self.return_position[0]),
                    str(self.return_position[1]),
                )
        elif kind == "pointer_move":
            self.update_pointer(event)
            self.flush_pointer()
        elif kind == "pointer_delta":
            self.update_pointer(event)
            self.flush_pointer()
        elif kind == "pointer_button":
            if self.backend == "hyprland" and event.get("state") == "pressed":
                button = {0: 272, 1: 274, 2: 273}.get(int(event.get("button", 0)))
                if button:
                    run_hyprctl("dispatch", "sendshortcut", f",mouse:{button}")
            elif self.backend == "sway":
                button = {0: "button1", 1: "button3", 2: "button2"}.get(int(event.get("button", 0)))
                if button:
                    state = "press" if event.get("state") == "pressed" else "release"
                    run_swaymsg("seat", "seat0", "cursor", state, button)
        elif kind == "wheel":
            if self.backend == "hyprland":
                key = "pagedown" if float(event.get("dy", 0)) > 0 else "pageup"
                run_hyprctl("dispatch", "sendshortcut", f",{key}")
            else:
                button = "button5" if float(event.get("dy", 0)) > 0 else "button4"
                run_swaymsg("seat", "seat0", "cursor", "press", button)
                run_swaymsg("seat", "seat0", "cursor", "release", button)
        elif kind == "key" and event.get("state") == "pressed" and not event.get("repeat"):
            key = browser_key(event.get("code", ""))
            if not key:
                return
            modifiers = []
            if event.get("ctrl"):
                modifiers.append("CTRL")
            if event.get("alt"):
                modifiers.append("ALT")
            if event.get("shift"):
                modifiers.append("SHIFT")
            if event.get("meta"):
                modifiers.append("SUPER")
            if self.backend == "hyprland":
                run_hyprctl("dispatch", "sendshortcut", f"{' '.join(modifiers)},{key}")
            else:
                run_wtype(key, modifiers)

    def update_pointer(self, event: dict) -> None:
        if event.get("type") == "pointer_move":
            self.remote_x = float(event["x"])
            self.remote_y = float(event["y"])
        else:
            self.remote_x += float(event.get("dx", 0))
            self.remote_y += float(event.get("dy", 0))

    def flush_pointer(self) -> None:
        self.remote_x = max(0, min(self.mode.width - 1, self.remote_x))
        self.remote_y = max(0, min(self.mode.height - 1, self.remote_y))
        x = self.geometry[0] + round(self.remote_x)
        y = self.geometry[1] + round(self.remote_y)
        if self.backend == "hyprland":
            run_hyprctl("dispatch", "movecursor", str(x), str(y))
        else:
            run_swaymsg("seat", "seat0", "cursor", "set", str(x), str(y))


def cursor_position() -> tuple[int, int]:
    result = subprocess.run(
        ["hyprctl", "cursorpos"],
        check=True,
        capture_output=True,
        text=True,
        timeout=1,
    )
    x, y = result.stdout.strip().split(",", 1)
    return int(float(x.strip())), int(float(y.strip()))


def browser_key(code: str) -> str | None:
    if code.startswith("Key") and len(code) == 4:
        return code[-1].lower()
    if code.startswith("Digit") and len(code) == 6:
        return code[-1]
    mapping = {
        "Enter": "return",
        "NumpadEnter": "return",
        "Space": "space",
        "Tab": "tab",
        "Escape": "escape",
        "Backspace": "backspace",
        "Delete": "delete",
        "Insert": "insert",
        "Home": "home",
        "End": "end",
        "PageUp": "pageup",
        "PageDown": "pagedown",
        "ArrowUp": "up",
        "ArrowDown": "down",
        "ArrowLeft": "left",
        "ArrowRight": "right",
        "Minus": "minus",
        "Equal": "equal",
        "BracketLeft": "bracketleft",
        "BracketRight": "bracketright",
        "Backslash": "backslash",
        "Semicolon": "semicolon",
        "Quote": "apostrophe",
        "Comma": "comma",
        "Period": "period",
        "Slash": "slash",
        "Backquote": "grave",
    }
    return mapping.get(code)


def run_hyprctl(*args: str) -> None:
    subprocess.run(
        ["hyprctl", *args],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        timeout=1,
    )


def run_swaymsg(*args: str) -> None:
    subprocess.run(
        ["swaymsg", *args],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        timeout=1,
    )


def run_wtype(key: str, modifiers: list[str]) -> None:
    modifier_names = {"CTRL": "ctrl", "ALT": "alt", "SHIFT": "shift", "SUPER": "logo"}
    command = ["wtype"]
    for modifier in modifiers:
        command.extend(["-M", modifier_names[modifier]])
    command.extend(["-k", sway_key(key)])
    for modifier in reversed(modifiers):
        command.extend(["-m", modifier_names[modifier]])
    subprocess.run(command, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=1)


def sway_key(key: str) -> str:
    mapping = {
        "return": "Return", "escape": "Escape", "backspace": "BackSpace",
        "delete": "Delete", "insert": "Insert", "home": "Home", "end": "End",
        "pageup": "Page_Up", "pagedown": "Page_Down", "up": "Up", "down": "Down",
        "left": "Left", "right": "Right", "space": "space", "tab": "Tab",
    }
    return mapping.get(key, key)


def emit(message: dict) -> None:
    print(json.dumps(message, separators=(",", ":")), flush=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--backend", choices=("hyprland", "sway"), default="hyprland")
    parser.add_argument("--output", required=True)
    parser.add_argument("--mode", default="1280x720@30")
    parser.add_argument("--probe", action="store_true")
    args = parser.parse_args()
    mode = parse_mode(args.mode)

    Gst.init(None)
    bridge = DesktopBridge(args.backend, args.output, mode)
    if args.probe:
        print("ok: GStreamer WebRTC/VP8 pipeline is available")
        bridge.pipeline.set_state(Gst.State.NULL)
        return 0

    signal.signal(signal.SIGTERM, lambda *_: bridge.stop())
    signal.signal(signal.SIGINT, lambda *_: bridge.stop())
    bridge.run()
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        emit({"type": "fatal", "message": str(error)})
        raise
