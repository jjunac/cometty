#!/usr/bin/env python3
"""Profile Cometty's real PTY/DA1 throughput, visible or minimized (macOS).

Build a release binary first. Every workload gets a fresh, privately configured
process running driver.py's agent, not a shell. No other apps/preferences are
touched. --minimize needs Accessibility; all runs need a working display/GPU.
Use --perf and/or --sample-seconds 8 for diagnosis, not headline comparisons.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[2]
WORKLOADS = ("ascii", "sgr", "unicode", "scroll_region", "redraw")


def wait_for(predicate, process, seconds):
    deadline = time.monotonic() + seconds
    while True:
        result = predicate()
        if result:
            return result
        if process.poll() is not None:
            raise RuntimeError("Cometty exited early; inspect stderr.log")
        if time.monotonic() >= deadline:
            raise RuntimeError("Timed out waiting for benchmark agent")
        time.sleep(0.05)


def run_case(args, output, name):
    run = output / name
    run.mkdir()
    home = run / "home"
    home.mkdir()
    command = "/usr/bin/python3 -I -B '%s' agent '%s'" % (ROOT / "scripts/compare/driver.py", run)
    config = run / "config.toml"
    config.write_text('''[font]
family = "monospace"
size = 14.0
[window]
width = 1000
height = 700
[cursor]
default_blinking = false
[terminal]
scrollback_lines = 10000
[shell]
shell = %s
[session]
restore = "off"
[log]
filter = "off"
''' % json.dumps(command))
    env = os.environ.copy()
    env.update(HOME=str(home), XDG_STATE_HOME=str(home / "state"),
               XDG_CONFIG_HOME=str(home / "config"), RUST_LOG="off",
               COMETTY_PERF="1" if args.perf else "0")
    with (run / "stderr.log").open("w") as log:
        app = subprocess.Popen([str(args.binary), "--config", str(config)], env=env,
                               stdout=log, stderr=log)
        fifo = None
        try:
            agents = wait_for(lambda: list((run / "agents").glob("*.json")), app, 30)
            info = json.loads(agents[0].read_text())
            fifo = run / "agents" / (str(info["pid"]) + ".fifo")
            if args.minimize:
                subprocess.run(["osascript", "-e",
                                'tell application "System Events" to tell (first process whose unix id is %d) '
                                'to set value of attribute "AXMinimized" of window 1 to true' % app.pid],
                               check=True)
            op = {"op": "throughput", "workload": name, "bytes": args.mib << 20,
                  "warmup": 1, "samples": args.samples, "pids": [app.pid], "seq": 1}
            (run / "op.json").write_text(json.dumps(op, indent=2) + "\n")
            print("Running", name, "pid", app.pid, "grid", info["tty"], flush=True)
            with fifo.open("w") as control:
                control.write(json.dumps(op) + "\n")
            if args.sample_seconds:
                subprocess.run(["sample", str(app.pid), str(args.sample_seconds), "1", "-file",
                                str(run / "cpu.sample.txt")], check=True,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            result_path = run / "agents" / (str(info["pid"]) + ".1.json")
            wait_for(result_path.exists, app, args.timeout)
            result = json.loads(result_path.read_text())
            (run / "result.json").write_text(json.dumps(result, indent=2) + "\n")
            if not result["ok"] or not all(s["ok"] for s in result["result"]["samples"]):
                raise RuntimeError("Agent/DA1 failure: " + str(result))
            samples = result["result"]["samples"]
            print(name, "MiB/s", [round(s["mib_s"], 3) for s in samples], "CPU s/MiB",
                  [round(s["terminal"]["cpu_ns"] / 1e9 / (s["bytes"] / (1 << 20)), 4)
                   for s in samples], flush=True)
        finally:
            if app.poll() is None:
                if fifo is not None and fifo.exists():
                    # Only our own agent/process, never quit by application name.
                    try:
                        fd = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
                        try:
                            os.write(fd, b'{"op":"exit"}\n')
                        finally:
                            os.close(fd)
                    except OSError:
                        # The child may already have exited; still reap the app.
                        pass
                try:
                    app.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    app.terminate()
                    app.wait(timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", nargs="?")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cometty")
    parser.add_argument("--workloads", default=",".join(WORKLOADS))
    parser.add_argument("--mib", type=int, default=8)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=600)
    parser.add_argument("--minimize", action="store_true")
    parser.add_argument("--perf", action="store_true")
    parser.add_argument("--sample-seconds", type=int, default=0)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("macOS only; use scripts/bench-throughput.py for headless replay")
    if args.mib < 1 or args.samples < 1 or args.sample_seconds < 0 or args.timeout <= 0:
        parser.error("sizes, samples and timeout must be positive; sample-seconds cannot be negative")
    workloads = args.workloads.split(",")
    if any(name not in WORKLOADS for name in workloads) or len(set(workloads)) != len(workloads):
        parser.error("choose distinct workloads from " + ",".join(WORKLOADS))
    args.binary = args.binary.resolve()
    if not args.binary.is_file():
        parser.error("build the release binary first: cargo build --release --locked")
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = Path(args.output or ROOT / ("target/perf/gui-throughput-" + stamp)).resolve()
    # Shell command parsing quotes paths with single quotes.
    if "'" in str(ROOT) or "'" in str(output):
        parser.error("repository/output paths must not contain single quotes")
    output.mkdir(parents=True, exist_ok=False)
    source = output / "source"
    source.mkdir()
    for directory in ("src", "assets", "scripts/compare"):
        shutil.copytree(ROOT / directory, source / directory)
    if (ROOT / "vendor").exists():
        shutil.copytree(ROOT / "vendor", source / "vendor")
    for file in ("Cargo.toml", "Cargo.lock"):
        shutil.copy2(ROOT / file, source / file)
    metadata = {"utc": stamp, "arguments": {k: str(v) if isinstance(v, Path) else v
                                           for k, v in vars(args).items()},
                "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
                "lock_sha256": hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest()}
    for name, command in (("revision", ["git", "rev-parse", "HEAD"]),
                           ("status", ["git", "status", "--short"]),
                           ("host", ["sysctl", "-n", "machdep.cpu.brand_string"]),
                           ("os", ["sw_vers"]), ("rustc", ["rustc", "-Vv"]),
                           ("displays", ["system_profiler", "SPDisplaysDataType"]),
                           ("power", ["pmset", "-g", "batt"])):
        metadata[name] = subprocess.check_output(command, cwd=ROOT, text=True).strip()
    (output / "environment.json").write_text(json.dumps(metadata, indent=2) + "\n")
    (output / "source.patch").write_bytes(subprocess.check_output(["git", "diff", "--binary"], cwd=ROOT))
    print("Artifacts:", output, flush=True)
    for name in workloads:
        run_case(args, output, name)


if __name__ == "__main__":
    main()
