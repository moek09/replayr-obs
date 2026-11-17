#!/usr/bin/env python3
"""Smoke test for replayr-obs.exe: record the first monitor for 3 s, save, probe.

usage: obs-smoke-test.py <path to replayr-obs(.exe)> [--ffprobe ffprobe]

Starts the helper, sends Configure (monitor 0, x264, 1280x720, 30 fps, 5 s
buffer, no audio, no watermark), waits for `ready`, waits 3 s, sends `save`,
expects `saved` with an existing non-empty file, runs ffprobe on it and sends
`quit`. Prints the helper's stderr (OBS's log) when something fails. Exit code
0 on success. Standard library only.
"""
import argparse
import json
import os
import queue
import subprocess
import sys
import tempfile
import threading
import time

READY_TIMEOUT = 60
SAVE_TIMEOUT = 30
QUIT_TIMEOUT = 20

CONFIG = {
    "type": "configure",
    "canvas": [1280, 720],
    "output": [1280, 720],
    "fps": 30,
    "encoder": "x264",
    "bitrate_kbps": 6000,
    "keyint_sec": 1,
    "buffer_seconds": 5,
    "source": {"kind": "monitor", "index": 0},
    "audio": [],
    "watermark": None,
}


def pump(stream, sink):
    for line in iter(stream.readline, ""):
        sink.put(line)


def wait_for(replies, kinds, timeout, proc, log):
    """Next reply whose type is in `kinds`; raises on `error`, exit or timeout."""
    end = time.monotonic() + timeout
    while True:
        left = end - time.monotonic()
        if left <= 0:
            raise RuntimeError(f"timed out waiting for {kinds}")
        try:
            line = replies.get(timeout=min(left, 0.5))
        except queue.Empty:
            if proc.poll() is not None and replies.empty():
                raise RuntimeError(f"helper exited with code {proc.returncode}")
            continue
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            log.append(f"[stdout, not JSON] {line}")
            continue
        kind = msg.get("type")
        print(f"<- {kind}", flush=True)
        if kind == "error":
            raise RuntimeError(f"helper error: {msg.get('data', {}).get('message')}")
        if kind in kinds:
            return msg


def send(proc, msg):
    proc.stdin.write(json.dumps(msg) + "\n")
    proc.stdin.flush()
    print(f"-> {msg['type']}", flush=True)


def run(helper, ffprobe):
    out_dir = tempfile.mkdtemp(prefix="replayr-smoke-")
    proc = subprocess.Popen(
        [helper],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
        cwd=os.path.dirname(os.path.abspath(helper)),
    )
    replies, errs = queue.Queue(), queue.Queue()
    threading.Thread(target=pump, args=(proc.stdout, replies), daemon=True).start()
    threading.Thread(target=pump, args=(proc.stderr, errs), daemon=True).start()
    log = []
    ok = False
    try:
        send(proc, CONFIG)
        wait_for(replies, {"ready"}, READY_TIMEOUT, proc, log)
        time.sleep(3)
        send(proc, {"type": "save", "dir": out_dir})
        saved = wait_for(replies, {"saved"}, SAVE_TIMEOUT, proc, log)
        path = saved["data"]["path"]
        if not os.path.isfile(path):
            raise RuntimeError(f"saved file does not exist: {path}")
        size = os.path.getsize(path)
        if size == 0:
            raise RuntimeError(f"saved file is empty: {path}")
        print(f"clip: {path} ({size} bytes)", flush=True)
        probe = subprocess.run(
            [ffprobe, "-v", "error", "-show_entries", "stream=codec_type,codec_name,width,height",
             "-of", "json", path],
            capture_output=True, text=True, timeout=60,
        )
        if probe.returncode != 0:
            raise RuntimeError(f"ffprobe failed: {probe.stderr.strip()}")
        print(probe.stdout, flush=True)
        streams = json.loads(probe.stdout).get("streams", [])
        if not any(s.get("codec_type") == "video" for s in streams):
            raise RuntimeError("ffprobe found no video stream")
        ok = True
    except Exception as e:  # noqa: BLE001 - report and fail
        print(f"SMOKE TEST FAILED: {e}", file=sys.stderr, flush=True)
    finally:
        try:
            send(proc, {"type": "quit"})
        except Exception:  # helper already gone
            pass
        try:
            proc.wait(timeout=QUIT_TIMEOUT)
        except subprocess.TimeoutExpired:
            print("helper did not quit, killing it", file=sys.stderr, flush=True)
            proc.kill()
            proc.wait()
            ok = False
        time.sleep(0.2)
        if not ok:
            print("--- helper stderr ---", file=sys.stderr)
            while not errs.empty():
                sys.stderr.write(errs.get())
            for line in log:
                print(line, file=sys.stderr)
            print("--- end of helper stderr ---", file=sys.stderr, flush=True)
    return ok


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("helper")
    ap.add_argument("--ffprobe", default="ffprobe")
    args = ap.parse_args()
    return 0 if run(args.helper, args.ffprobe) else 1


if __name__ == "__main__":
    sys.exit(main())
