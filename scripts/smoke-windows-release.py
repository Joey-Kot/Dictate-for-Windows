#!/usr/bin/env python3
"""Exercise a packaged CLI's embedded Opus conversion against a local ASR stub."""

import base64
import json
import pathlib
import queue
import subprocess
import sys
import tempfile
import threading
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class AsrStub(BaseHTTPRequestHandler):
    def log_message(self, _format, *_args):
        pass

    def do_POST(self):
        try:
            body = self.read_body()
            if self.path != "/transcribe" or "multipart/form-data" not in self.headers.get("Content-Type", ""):
                raise ValueError("Expected a multipart transcription request")
            if b"OggS" not in body or b"OpusHead" not in body:
                raise ValueError("Expected an independently decodable Opus upload")
            self.server.results.put(None)
            data = json.dumps({"text": "static-libav-ok"}).encode("utf-8")
            self.send_response(200)
        except (OSError, ValueError) as error:
            self.server.results.put(str(error))
            data = b"invalid smoke test request"
            self.send_response(400)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def read_body(self):
        if self.headers.get("Transfer-Encoding", "").lower() != "chunked":
            size = int(self.headers.get("Content-Length", "0"))
            if not 0 < size <= 4_000_000:
                raise ValueError("Unexpected request size")
            return self.rfile.read(size)
        chunks = []
        total = 0
        while True:
            size = int(self.rfile.readline(1024).split(b";", 1)[0].strip(), 16)
            if not size:
                while self.rfile.readline(8192).strip():
                    pass
                return b"".join(chunks)
            total += size
            if total > 4_000_000:
                raise ValueError("Unexpected request size")
            chunk = self.rfile.read(size)
            if len(chunk) != size or self.rfile.read(2) != b"\r\n":
                raise ValueError("Incomplete chunked request")
            chunks.append(chunk)


def main():
    if len(sys.argv) != 2:
        raise SystemExit("Usage: smoke-windows-release.py PATH_TO_CLI")
    executable = pathlib.Path(sys.argv[1]).resolve(strict=True)
    fixture = pathlib.Path(__file__).resolve().with_name("connectivity_test.pcm.b64")
    pcm = base64.b64decode(fixture.read_bytes())
    server = ThreadingHTTPServer(("127.0.0.1", 0), AsrStub)
    server.daemon_threads = True
    server.results = queue.Queue()
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        with tempfile.TemporaryDirectory(prefix="dictate-release-smoke-") as directory:
            root = pathlib.Path(directory)
            source = root / "input.wav"
            output = root / "output.txt"
            with wave.open(str(source), "wb") as wav:
                wav.setnchannels(1)
                wav.setsampwidth(2)
                wav.setframerate(16_000)
                wav.writeframes(pcm)
            command = [
                str(executable),
                "--file", str(source),
                "--output", str(output),
                "--api-endpoint", f"http://127.0.0.1:{server.server_port}/transcribe",
                "--cache-dir", str(root / "cache"),
                "--codecs", "opus",
                "--container", "opus",
                "--max-retry", "1",
                "--request-timeout", "20",
            ]
            result = subprocess.run(command, cwd=root, capture_output=True, timeout=60, check=False)
            if result.returncode:
                raise RuntimeError(
                    f"CLI exited with {result.returncode}:\n"
                    + result.stdout.decode("utf-8", errors="replace")
                    + result.stderr.decode("utf-8", errors="replace")
                )
            try:
                error = server.results.get(timeout=2)
            except queue.Empty as exc:
                raise RuntimeError("CLI did not send an ASR request") from exc
            if error:
                raise RuntimeError(error)
            if output.read_text(encoding="utf-8") != "static-libav-ok":
                raise RuntimeError("Unexpected transcript")
            print(f"{executable.name}: embedded Opus conversion and transcription passed")
    finally:
        server.shutdown()
        server.server_close()
        worker.join()


if __name__ == "__main__":
    main()
