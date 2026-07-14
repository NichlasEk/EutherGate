#!/usr/bin/env python3
"""GStreamer WebRTC bridge for one Hyprland headless output.

Signaling is newline-delimited JSON on stdin/stdout. Desktop input arrives on a
WebRTC DataChannel and is injected through Hyprland IPC in this first slice.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
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
    def __init__(self, output: str, mode: Mode) -> None:
        self.output = output
        self.mode = mode
        self.loop = GLib.MainLoop()
        self.running = threading.Event()
        self.running.set()
        self.input_events: queue.Queue[dict] = queue.Queue(maxsize=256)
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
            rtpvp8pay picture-id-mode=15-bit pt=96 !
            application/x-rtp,media=video,encoding-name=VP8,clock-rate=90000,payload=96 !
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

    def _on_ice_candidate(self, _webrtc, mline_index: int, candidate: str) -> None:
        emit({"type": "ice", "candidate": candidate, "sdpMLineIndex": mline_index})

    def _on_data_channel(self, _webrtc, channel) -> None:
        channel.connect("on-message-string", self._on_data_message)
        emit({"type": "input-ready", "label": channel.get_property("label")})

    def _on_data_message(self, _channel, payload: str) -> None:
        try:
            event = json.loads(payload)
            if isinstance(event, dict):
                try:
                    self.input_events.put_nowait(event)
                except queue.Full:
                    if event.get("type") == "pointer_move":
                        return
                    self.input_events.get_nowait()
                    self.input_events.put_nowait(event)
        except (ValueError, TypeError):
            return

    def _capture_loop(self) -> None:
        frame_duration = Gst.SECOND // self.mode.fps
        frame = 0
        next_frame = time.monotonic()
        while self.running.is_set():
            try:
                capture = subprocess.run(
                    ["grim", "-o", self.output, "-t", "ppm", "-"],
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
        geometry = output_geometry(self.output)
        while self.running.is_set():
            try:
                event = self.input_events.get(timeout=0.5)
                inject_input(event, geometry, self.mode)
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


def output_geometry(output: str) -> tuple[int, int]:
    result = subprocess.run(
        ["hyprctl", "monitors", "all", "-j"],
        check=True,
        capture_output=True,
        text=True,
        timeout=2,
    )
    monitor = next(item for item in json.loads(result.stdout) if item["name"] == output)
    return int(monitor["x"]), int(monitor["y"])


def inject_input(event: dict, geometry: tuple[int, int], mode: Mode) -> None:
    kind = event.get("type")
    if kind == "pointer_move":
        x = geometry[0] + max(0, min(mode.width - 1, round(float(event["x"]))))
        y = geometry[1] + max(0, min(mode.height - 1, round(float(event["y"]))))
        run_hyprctl("dispatch", "movecursor", str(x), str(y))
    elif kind == "pointer_button" and event.get("state") == "pressed":
        button = {0: 272, 1: 274, 2: 273}.get(int(event.get("button", 0)))
        if button:
            run_hyprctl("dispatch", "sendshortcut", f",mouse:{button}")
    elif kind == "wheel":
        key = "pagedown" if float(event.get("dy", 0)) > 0 else "pageup"
        run_hyprctl("dispatch", "sendshortcut", f",{key}")
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
        run_hyprctl("dispatch", "sendshortcut", f"{' '.join(modifiers)},{key}")


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


def emit(message: dict) -> None:
    print(json.dumps(message, separators=(",", ":")), flush=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True)
    parser.add_argument("--mode", default="1280x720@30")
    parser.add_argument("--probe", action="store_true")
    args = parser.parse_args()
    mode = parse_mode(args.mode)

    Gst.init(None)
    bridge = DesktopBridge(args.output, mode)
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
