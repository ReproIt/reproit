"""A minimal Dart VM service client for the Flutter Android field campaign.

The service speaks JSON-RPC over a WebSocket and the worker image carries no
WebSocket library, so the handshake and framing are done here rather than
pulled in as a dependency. Only text frames and one outstanding request at a
time are needed.

The VM service listens inside the emulator, so its port has to be forwarded
with adb before any of this can connect at all.
"""

from __future__ import annotations

import base64
import json
import os
import socket
import struct
import urllib.parse

MAX_FRAME_BYTES = 64 * 1024 * 1024


class VmService:
    def __init__(self, uri: str, timeout: int = 120) -> None:
        parsed = urllib.parse.urlparse(uri)
        self.host = parsed.hostname or "127.0.0.1"
        self.port = parsed.port or 80
        path = parsed.path if parsed.path.endswith("/") else parsed.path + "/"
        self.path = path + "ws"
        self.socket = socket.create_connection((self.host, self.port), timeout)
        self.socket.settimeout(timeout)
        self._buffer = b""
        self._identifier = 0
        self._handshake()

    def _handshake(self) -> None:
        key = base64.b64encode(os.urandom(16)).decode()
        self.socket.sendall(
            (
                f"GET {self.path} HTTP/1.1\r\n"
                f"Host: {self.host}:{self.port}\r\n"
                "Upgrade: websocket\r\n"
                "Connection: Upgrade\r\n"
                f"Sec-WebSocket-Key: {key}\r\n"
                "Sec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        while b"\r\n\r\n" not in self._buffer:
            chunk = self.socket.recv(4096)
            if not chunk:
                raise RuntimeError("VM service closed during handshake")
            self._buffer += chunk
        head, _, rest = self._buffer.partition(b"\r\n\r\n")
        if b"101" not in head.split(b"\r\n")[0]:
            raise RuntimeError(f"VM service refused upgrade: {head[:200]!r}")
        self._buffer = rest

    def _send(self, payload: bytes) -> None:
        header = bytearray([0x81])
        mask = os.urandom(4)
        length = len(payload)
        if length < 126:
            header.append(0x80 | length)
        elif length < (1 << 16):
            header.append(0x80 | 126)
            header += struct.pack("!H", length)
        else:
            header.append(0x80 | 127)
            header += struct.pack("!Q", length)
        header += mask
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        self.socket.sendall(bytes(header) + masked)

    def _fill(self, count: int) -> bytes:
        if count > MAX_FRAME_BYTES:
            raise RuntimeError(f"VM service frame exceeds its bound: {count}")
        while len(self._buffer) < count:
            chunk = self.socket.recv(65536)
            if not chunk:
                raise RuntimeError("VM service closed while reading")
            self._buffer += chunk
        value, self._buffer = self._buffer[:count], self._buffer[count:]
        return value

    def _receive(self) -> str:
        while True:
            first, second = self._fill(2)
            opcode = first & 0x0F
            length = second & 0x7F
            if length == 126:
                length = struct.unpack("!H", self._fill(2))[0]
            elif length == 127:
                length = struct.unpack("!Q", self._fill(8))[0]
            payload = self._fill(length)
            if opcode == 0x8:
                raise RuntimeError("VM service closed the connection")
            if opcode in (0x1, 0x2):
                return payload.decode("utf-8", "replace")

    def call(self, method: str, params: dict | None = None) -> dict:
        self._identifier += 1
        identifier = str(self._identifier)
        self._send(
            json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": identifier,
                    "method": method,
                    "params": params or {},
                }
            ).encode()
        )
        while True:
            message = json.loads(self._receive())
            if message.get("id") == identifier:
                if "error" in message:
                    raise RuntimeError(f"{method}: {message['error']}")
                return message.get("result", {})

    def close(self) -> None:
        try:
            self.socket.close()
        except OSError:
            pass
