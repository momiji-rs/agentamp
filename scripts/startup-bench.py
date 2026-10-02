#!/usr/bin/env python3
"""Times the window's start, step by step, in a pseudo-terminal.

    scripts/startup-bench.py [--runs 20] [--terminal kitty|plain|silent] [--size 140x40] [BINARY]

The harness plays the terminal: it answers the image query the way kitty
(images), a plain 24-bit terminal (half blocks) or a terminal that answers
nothing would. It reads the window's AGENTAMP_TRACE marks, presses `/` as
soon as the first frame has arrived and times how long the prompt takes to
show, then quits with `q`. Times are milliseconds from the spawn; each row
is the median and the slowest run.

The daemon is whatever AGENTAMP_HOME points at: run it with and without a
playing track to see both starts.
"""

import argparse
import fcntl
import os
import pty
import select
import statistics
import struct
import subprocess
import sys
import termios
import time
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

ANSWERS = {
    # Kitty graphics OK, primary attributes, 10x20 cells, then the status.
    "kitty": b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c\x1b[6;20;10t\x1b[0n",
    "plain": b"\x1b[?62;22c\x1b[6;20;10t\x1b[0n",
    "silent": b"",
}


def now_us():
    return time.time_ns() // 1000


def run_once(binary, terminal, cols, rows, trace):
    trace.unlink(missing_ok=True)
    env = dict(os.environ, AGENTAMP_TRACE=str(trace), TERM="xterm-256color")
    # Inside tmux the image query is wrapped for tmux; this harness is the terminal.
    env.pop("TMUX", None)
    main, child = pty.openpty()
    fcntl.ioctl(child, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, cols * 10, rows * 20))
    spawned = now_us()
    process = subprocess.Popen([binary], stdin=child, stdout=child, stderr=child, env=env, start_new_session=True)
    os.close(child)

    chunks = []  # (µs, bytes)
    output = b""
    answered = pressed = prompted = quit_sent = False
    deadline = time.time() + 10
    while time.time() < deadline:
        ready, _, _ = select.select([main], [], [], 0.05)
        if ready:
            try:
                data = os.read(main, 1 << 16)
            except OSError:
                break
            if not data:
                break
            at = now_us()
            chunks.append((at, len(data)))
            output += data
        if not answered and b"\x1b[5n" in output:
            os.write(main, ANSWERS[terminal])
            answered = True
        # The first frame is in once the footer's last key hint has arrived.
        if not pressed and b"quit" in output:
            first_frame = now_us()
            os.write(main, b"/")
            pressed = True
            mark = len(output)
        if pressed and not prompted and "Play ▸".encode() in output[mark:]:
            prompt_at = now_us()
            prompted = True
            # Let the snapshot and the cover arrive, then leave. Keep
            # reading meanwhile: a full pty would stall the window.
            pending = [(prompt_at + 600_000, b"\x1b"), (prompt_at + 700_000, b"q")]
        while prompted and pending and now_us() >= pending[0][0]:
            os.write(main, pending.pop(0)[1])
            quit_sent = not pending
        if quit_sent and process.poll() is not None:
            break
    process.wait(timeout=5)
    os.close(main)

    times = {}
    for line in trace.read_text().splitlines():
        at, step = line.split(" ", 1)
        key = step.split(" ")[0] if step.startswith(("frame", "image")) else step
        if step.startswith("frame"):
            key = "frame " + step.split(" ")[1] + " " + step.split(" ")[3]
            times[key + " draw"] = int(step.split(" ")[2][:-2]) / 1000
        if step.startswith("image encoded"):
            key = "image encoded " + step.split(" ")[2]
            times[key + " cost"] = int(step.split(" ")[3][:-2]) / 1000
        times.setdefault(key, (int(at) - spawned) / 1000)
    times["terminal: first byte"] = (chunks[0][0] - spawned) / 1000 if chunks else None
    times["terminal: first frame in"] = (first_frame - spawned) / 1000 if pressed else None
    times["terminal: key to prompt"] = (prompt_at - first_frame) / 1000 if prompted else None
    times["bytes to first frame"] = mark if pressed else None
    times["bytes in total"] = len(output)
    return times


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", nargs="?", default=str(ROOT / "target/release/agentamp"))
    parser.add_argument("--runs", type=int, default=20)
    parser.add_argument("--terminal", choices=ANSWERS, default="kitty")
    parser.add_argument("--size", default="140x40")
    args = parser.parse_args()
    cols, rows = map(int, args.size.split("x"))
    trace = ROOT / "target/startup-bench.trace"

    runs = defaultdict(list)
    order = []
    for _ in range(args.runs):
        for key, value in run_once(args.binary, args.terminal, cols, rows, trace).items():
            if key not in order:
                order.append(key)
            if value is not None:
                runs[key].append(value)
    trace.unlink(missing_ok=True)
    print(f"{args.runs} runs, {args.terminal} terminal, {args.size}: median / slowest")
    for key in order:
        values = runs[key]
        if key.startswith("bytes"):
            print(f"  {key:<34} {statistics.median(values):>9.0f} {max(values):>9.0f}")
        else:
            print(f"  {key:<34} {statistics.median(values):>9.2f} {max(values):>9.2f}  ms")


if __name__ == "__main__":
    sys.exit(main())
