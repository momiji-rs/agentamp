#!/usr/bin/env python3
"""Times the window's start in real terminals, on a Hyprland desktop.

    scripts/terminal-bench.py [--runs 10] [--terminals ghostty,foot,alacritty] [BINARY]

Each run opens a terminal running the window on a headless output, out
of the user's sight (workspace 6, silently), and reads its AGENTAMP_TRACE
marks. Times are milliseconds from asking Hyprland to launch the terminal:
the median and the slowest run. The terminal's own start counts, since a
user waits for it too; what it takes the terminal to put the last frame on
glass is not measured.
"""

import argparse
import os
import shlex
import statistics
import subprocess
import time
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = "agentamp-shot"
CLASS = "agentamp.shot"

TERMINALS = {
    "ghostty": "ghostty --class={cls} -e {cmd}",
    "foot": "foot --app-id={cls} {cmd}",
    "alacritty": "alacritty --class {cls} -e {cmd}",
}


def hypr(*args):
    return subprocess.run(["hyprctl", *args], capture_output=True, text=True, check=True).stdout


def lua(code):
    return hypr("eval", code)


def now_us():
    return time.time_ns() // 1000


def setup():
    if OUTPUT not in hypr("monitors", "all"):
        hypr("output", "create", "headless", OUTPUT)
    lua(f"hl.window_rule({{ match = {{ class = '{CLASS.replace('.', '[.]')}' }}, workspace = '6 silent' }})")


def close():
    lua(f"hl.dispatch(hl.dsp.window.close({{ window = 'class:{CLASS}' }}))")
    for _ in range(100):
        if f"class: {CLASS}" not in hypr("clients"):
            return
        time.sleep(0.02)


def done(lines):
    """The cover has been drawn: a frame after the cover was applied."""
    applied = next((i for i, line in enumerate(lines) if line.endswith("applied art")), None)
    return applied is not None and any(" frame " in f" {line}" for line in lines[applied:])


def run_once(binary, terminal, trace):
    trace.unlink(missing_ok=True)
    # Hyprland starts the terminal with its own environment, not ours.
    env = f"AGENTAMP_TRACE={shlex.quote(str(trace))}"
    if "AGENTAMP_HOME" in os.environ:
        env += f" AGENTAMP_HOME={shlex.quote(os.path.abspath(os.environ['AGENTAMP_HOME']))}"
    command = f"{env} " + TERMINALS[terminal].format(cls=CLASS, cmd=shlex.quote(binary))
    launched = now_us()
    lua(f"hl.exec_cmd({command!r})".replace("\\'", "'"))
    lines = []
    deadline = time.time() + 6
    while time.time() < deadline:
        lines = trace.read_text().splitlines() if trace.exists() else []
        if done(lines):
            break
        time.sleep(0.005)
    close()
    times = {}
    for line in lines:
        at, step = line.split(" ", 1)
        words = step.split(" ")
        if words[0] == "frame":
            step = f"frame {words[1]} {words[3]}"
            times[step + " draw"] = int(words[2][:-2]) / 1000
        elif words[0] == "image":
            step = f"image {words[2]}"
            times[step + " cost"] = int(words[3][:-2]) / 1000
        times.setdefault(step, (int(at) - launched) / 1000)
    times["cover drawn"] = (int(lines[-1].split(" ")[0]) - launched) / 1000 if done(lines) else None
    return times


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", nargs="?", default=str(ROOT / "target/release/agentamp"))
    parser.add_argument("--runs", type=int, default=10)
    parser.add_argument("--terminals", default="ghostty,foot,alacritty")
    args = parser.parse_args()
    trace = ROOT / "target/terminal-bench.trace"
    setup()
    for terminal in args.terminals.split(","):
        runs, order = defaultdict(list), []
        for _ in range(args.runs):
            for key, value in run_once(args.binary, terminal, trace).items():
                if key not in order:
                    order.append(key)
                if value is not None:
                    runs[key].append(value)
            time.sleep(0.3)
        print(f"{terminal}, {args.runs} runs: ms from launch, median / slowest")
        for key in order:
            values = runs[key]
            if not values:
                print(f"  {key:<28} never")
                continue
            print(f"  {key:<28} {statistics.median(values):>9.2f} {max(values):>9.2f}  ({len(values)})")
    trace.unlink(missing_ok=True)


if __name__ == "__main__":
    os.environ.setdefault("WAYLAND_DISPLAY", "wayland-1")
    if "HYPRLAND_INSTANCE_SIGNATURE" not in os.environ:
        runtime = Path(f"/run/user/{os.getuid()}/hypr")
        os.environ["HYPRLAND_INSTANCE_SIGNATURE"] = sorted(runtime.iterdir())[0].name
    main()
