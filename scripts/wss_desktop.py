#!/usr/bin/env python3
"""JPEG desktop frames and input over an authenticated EutherGate WebSocket.

The Rust gateway owns the WebSocket. This helper uses newline-delimited JSON on
stdin for input and a small framed binary protocol on stdout:

    1 byte packet type + 4 byte big-endian payload length + payload

Packet type 1 is a JPEG frame and packet type 2 is a UTF-8 JSON status message.
"""

from __future__ import annotations

import argparse
import json
import queue
import shutil
import struct
import subprocess
import sys
import threading
import time

from webrtc_desktop import InputController, Mode, parse_mode


PACKET_JPEG = 1
PACKET_JSON = 2
MAX_FRAME_BYTES = 8 * 1024 * 1024


class WssDesktopBridge:
    def __init__(
        self,
        backend: str,
        output: str,
        mode: Mode,
        fps: int,
        quality: int,
        hide_cursor: bool,
    ) -> None:
        self.backend = backend
        self.output = output
        self.mode = mode
        self.fps = min(mode.fps, fps)
        self.quality = quality
        self.hide_cursor = hide_cursor
        self.running = threading.Event()
        self.running.set()
        self.input_events: queue.Queue[dict] = queue.Queue(maxsize=256)
        self.output_lock = threading.Lock()

    def run(self) -> None:
        threads = [
            threading.Thread(target=self._command_loop, name="commands", daemon=True),
            threading.Thread(target=self._capture_loop, name="capture", daemon=True),
            threading.Thread(target=self._input_loop, name="input", daemon=True),
        ]
        for thread in threads:
            thread.start()
        self.emit_json(
            {
                "type": "ready",
                "output": self.output,
                "codec": f"JPEG/{self.fps}FPS",
                "input": "websocket",
            }
        )
        threads[0].join()
        self.running.clear()

    def _command_loop(self) -> None:
        for line in sys.stdin:
            if not self.running.is_set():
                return
            try:
                message = json.loads(line)
                if not isinstance(message, dict):
                    continue
                if message.get("type") == "stop":
                    return
                try:
                    self.input_events.put_nowait(message)
                except queue.Full:
                    if message.get("type") in ("pointer_move", "pointer_delta"):
                        continue
                    self.input_events.get_nowait()
                    self.input_events.put_nowait(message)
            except (ValueError, TypeError):
                continue

    def _capture_loop(self) -> None:
        next_frame = time.monotonic()
        while self.running.is_set():
            try:
                command = [
                    "grim",
                    "-o",
                    self.output,
                    "-t",
                    "jpeg",
                    "-q",
                    str(self.quality),
                    "-",
                ]
                if not self.hide_cursor:
                    command.insert(1, "-c")
                capture = subprocess.run(
                    command,
                    check=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    timeout=3,
                )
                if not capture.stdout.startswith(b"\xff\xd8"):
                    raise ValueError("grim returned a non-JPEG frame")
                if len(capture.stdout) > MAX_FRAME_BYTES:
                    raise ValueError(f"JPEG frame exceeds {MAX_FRAME_BYTES} bytes")
                self._emit_packet(PACKET_JPEG, capture.stdout)
            except Exception as error:
                self.emit_json({"type": "capture-warning", "message": str(error)})
                time.sleep(0.5)
            next_frame += 1 / self.fps
            time.sleep(max(0, next_frame - time.monotonic()))

    def _input_loop(self) -> None:
        try:
            controller = InputController(self.backend, self.output, self.mode)
        except Exception as error:
            self.emit_json({"type": "input-warning", "message": str(error)})
            return
        deferred: dict | None = None
        try:
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
                        if event.get("type") == "text" or (
                            event.get("type") == "key"
                            and event.get("state") == "pressed"
                            and not event.get("repeat")
                        ):
                            self.emit_json(
                                {"type": "input-ack", "input": event.get("type")}
                            )
                except queue.Empty:
                    continue
                except Exception as error:
                    self.emit_json({"type": "input-warning", "message": str(error)})
        finally:
            controller.close()

    def emit_json(self, message: dict) -> None:
        payload = json.dumps(message, separators=(",", ":")).encode()
        self._emit_packet(PACKET_JSON, payload)

    def _emit_packet(self, kind: int, payload: bytes) -> None:
        header = struct.pack(">BI", kind, len(payload))
        with self.output_lock:
            sys.stdout.buffer.write(header)
            sys.stdout.buffer.write(payload)
            sys.stdout.buffer.flush()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--backend", choices=("hyprland", "sway"), default="hyprland")
    parser.add_argument("--output", required=True)
    parser.add_argument("--mode", default="1280x720@30")
    parser.add_argument("--fps", type=int, default=12)
    parser.add_argument("--quality", type=int, default=70)
    parser.add_argument("--hide-cursor", action="store_true")
    parser.add_argument("--probe", action="store_true")
    args = parser.parse_args()
    mode = parse_mode(args.mode)
    if not 1 <= args.fps <= 30:
        raise ValueError("fallback frame rate must be between 1 and 30")
    if not 20 <= args.quality <= 95:
        raise ValueError("JPEG quality must be between 20 and 95")
    if shutil.which("grim") is None:
        raise RuntimeError("grim is required for HTTPS/WSS desktop capture")
    if args.probe:
        print("ok: HTTPS/WSS JPEG desktop helper is available")
        return 0
    WssDesktopBridge(
        args.backend,
        args.output,
        mode,
        args.fps,
        args.quality,
        args.hide_cursor,
    ).run()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
