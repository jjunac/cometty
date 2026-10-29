#!/usr/bin/env python3
"""Measure Cometty's idle (and near-idle) CPU, wakeups and energy (macOS).

Build a release binary first. Every scenario launches a fresh, privately
configured process whose tabs run driver.py's agent instead of a shell: the
agent blocks on a FIFO, so the terminal's own wakeups are all that remain.
Nothing else is touched (isolated HOME/XDG dirs, owned PIDs only, no posted
input). Needs a display/GPU. The window opens unfocused or focused depending
on macOS activation policy; Cometty's idle schedule does not depend on focus.

Per scenario: settle, then diff `proc_pid_rusage` over the window (process CPU,
package-idle and interrupt wakeups, energy) and, with --perf, aggregate the
COMETTY_PERF=1 reports that fall inside the steady state (wake causes, redraw
reasons, frame phases, supervisor polls). --sample-seconds adds a native
`sample` call graph after the measured window (sampling perturbs the target,
so it never overlaps the counters).
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))
import procstat  # noqa: E402

# name: (description, config overrides, tab count, agent ops run in order).
# Every op but a final `tick` must finish before the measured window starts.
STEADY = "[cursor]\ndefault_blinking = false\n"
FILL = {"op": "fill", "kind": "ascii", "lines": 200}
SCENARIOS = {
    "blink": ("Default blinking cursor, one tab, near-empty screen", "", 1, []),
    "steady": ("Steady cursor (cursor.default_blinking = false), one tab", STEADY, 1, []),
    "blink-full": ("Blinking cursor below a screen of plain text (shell after output)",
                   "", 1, [FILL]),
    "steady-full": ("Steady cursor below a screen of plain text", STEADY, 1, [FILL]),
    "blink-5tabs": ("Blinking cursor, five tabs", "", 5, []),
    "steady-5tabs": ("Steady cursor, five tabs", STEADY, 5, []),
    "tick-1hz": ("Hidden cursor; a 15-cell status rewritten every 1 s on an empty screen",
                 STEADY, 1, [{"op": "tick", "interval": 1.0}]),
    "tick-1hz-full": ("Hidden cursor; full screen of text, status rewritten every 1 s (TUI clock)",
                      STEADY, 1, [{"op": "tick", "interval": 1.0, "fill": "ascii"}]),
    "tick-10hz": ("Hidden cursor; status rewritten every 100 ms on an empty screen",
                  STEADY, 1, [{"op": "tick", "interval": 0.1}]),
    "tick-10hz-full": ("Hidden cursor; full screen of text, status every 100 ms (spinner)",
                       STEADY, 1, [{"op": "tick", "interval": 0.1, "fill": "ascii"}]),
    "tick-30hz": ("Hidden cursor; status rewritten every 33 ms on an empty screen",
                  STEADY, 1, [{"op": "tick", "interval": 1 / 30}]),
    # Manual: needs an operator, so it is never in the default list.
    "hover": ("MANUAL: keep moving the pointer over the focused window during the window",
              STEADY, 1, [FILL]),
}
MANUAL = {"hover"}

PERF_LINE = re.compile(r"^cometty-perf \(.*?\) interval_us=(\d+) at_ms=(\d+):(.*)$")
PERF_FIELD = re.compile(r" (\w+)=\{([^}]*)\}")


def wait_for(predicate, process, seconds, what):
    deadline = time.monotonic() + seconds
    while True:
        result = predicate()
        if result:
            return result
        if process.poll() is not None:
            raise RuntimeError("Cometty exited early; inspect stderr.log")
        if time.monotonic() >= deadline:
            raise RuntimeError("Timed out waiting for " + what)
        time.sleep(0.05)


def parse_perf(text):
    """[(interval_s, end_s, {metric: {field: number}})] from COMETTY_PERF reports."""
    reports = []
    for line in text.splitlines():
        m = PERF_LINE.match(line)
        if not m:
            continue
        fields = {}
        for name, body in PERF_FIELD.findall(m.group(3)):
            values = {}
            for item in body.split(","):
                key, _, value = item.partition(":")
                try:
                    values[key] = float(value)
                except ValueError:
                    pass
            fields[name] = values
        reports.append((int(m.group(1)) / 1e6, int(m.group(2)) / 1e3, fields))
    return reports


def steady_rates(reports, start_s, end_s):
    """Per-second rates over the periodic reports lying wholly in [start, end].

    The partial exit report and the startup report are excluded, so neither
    shutdown nor launch activity leaks into the idle window."""
    chosen = [r for r in reports
              if r[0] >= 9.0 and r[1] - r[0] >= start_s - 0.25 and r[1] <= end_s + 0.25]
    seconds = sum(r[0] for r in chosen)
    if not chosen or seconds <= 0:
        return None
    totals = {}
    for _, _, fields in chosen:
        for name, values in fields.items():
            t = totals.setdefault(name, {"n": 0.0, "us": 0.0, "max_us": 0.0, "max_value": 0.0})
            t["n"] += values.get("n", 0.0)
            t["us"] += values.get("us", 0.0)
            t["max_us"] = max(t["max_us"], values.get("max_us", 0.0))
            t["max_value"] = max(t["max_value"], values.get("max_value", 0.0))
            for key in ("cpu_us", "vcsw", "ivcsw", "idle_wakeups", "interrupt_wakeups",
                        "instructions"):
                if key in values:
                    t[key] = t.get(key, 0.0) + values[key]
    return {"seconds": seconds, "reports": len(chosen), "totals": totals}


def write_session(state_home, tabs):
    directory = state_home / "cometty"
    directory.mkdir(parents=True)
    body = "version = 1\nactive = 0\n\n[window]\nwidth = 1000.0\nheight = 700.0\n"
    body += "\n[[tabs]]\n" * tabs
    (directory / "session.toml").write_text(body)


def run_scenario(args, output, name, repeat):
    description, overrides, tabs, ops = SCENARIOS[name]
    run = output / ("%s-%d" % (name, repeat))
    run.mkdir()
    home = run / "home"
    home.mkdir()
    command = "/usr/bin/python3 -I -B '%s' agent '%s'" % (HERE / "driver.py", run)
    config = run / "config.toml"
    # Restoring a written session is how several tabs open without posting
    # keystrokes; one tab uses the same path so every scenario matches.
    config.write_text('''[font]
family = "monospace"
size = 14.0
[window]
width = 1000
height = 700
[terminal]
scrollback_lines = 10000
[shell]
shell = %s
[session]
restore = "tabs"
[log]
filter = "off"
%s''' % (json.dumps(command), overrides))
    write_session(home / "state", tabs)
    env = os.environ.copy()
    env.update(HOME=str(home), XDG_STATE_HOME=str(home / "state"),
               XDG_CONFIG_HOME=str(home / "config"), RUST_LOG="off",
               COMETTY_PERF="1" if args.perf else "0")
    result = {"scenario": name, "description": description, "tabs": tabs, "repeat": repeat,
              "ops": ops, "settle_s": args.settle, "window_s": args.seconds}
    ticking = bool(ops) and ops[-1]["op"] == "tick"
    with (run / "stderr.log").open("w") as log:
        launched = time.monotonic()
        app = subprocess.Popen([str(args.binary), "--config", str(config)], env=env,
                               stdout=log, stderr=log)
        agents = []
        try:
            agents_dir = run / "agents"
            infos = wait_for(lambda: (lambda found: found if len(found) >= tabs else None)(
                sorted(agents_dir.glob("*.json")) if agents_dir.exists() else []),
                app, 30, "%d agents" % tabs)
            agents = [json.loads(p.read_text()) for p in infos if p.name.count(".") == 1]
            result["grid"] = agents[0]["tty"]
            # The restored active tab is the first one spawned.
            first = min(agents, key=lambda a: a["start_ns"])
            fifo = agents_dir / (str(first["pid"]) + ".fifo")
            for seq, op in enumerate(ops, 1):
                payload = dict(op, seq=seq)
                if op["op"] == "tick":
                    payload["seconds"] = args.settle + args.seconds + 5
                with fifo.open("w") as control:
                    control.write(json.dumps(payload) + "\n")
                if op["op"] != "tick":
                    done = agents_dir / ("%d.%d.json" % (first["pid"], seq))
                    wait_for(done.exists, app, 60, "op %s" % op["op"])
            time.sleep(args.settle)
            if name in MANUAL:
                print("  >>> %s: start now, for %.0f s" % (description, args.seconds), flush=True)
            before = procstat.sample([app.pid])
            t0 = time.monotonic()
            time.sleep(args.seconds)
            after = procstat.sample([app.pid])
            t1 = time.monotonic()
            if name in MANUAL:
                print("  >>> done, you can stop", flush=True)
            delta = procstat.diff(before, after)
            seconds = t1 - t0
            result["window"] = {"start_s": t0 - launched, "end_s": t1 - launched}
            result["process"] = {
                "seconds": seconds,
                "cpu_pct": 100.0 * delta["cpu_ns"] / 1e9 / seconds,
                "minstr_s": delta["instructions"] / 1e6 / seconds,
                "idle_wakeups_s": delta["pkg_idle_wkups"] / seconds,
                "interrupt_wakeups_s": delta["interrupt_wkups"] / seconds,
                "energy_mw": delta["energy_nj"] / 1e6 / seconds,
                "phys_footprint_mib": delta["phys_footprint"] / (1 << 20),
                "raw": delta,
            }
            if args.sample_seconds:
                subprocess.run(["sample", str(app.pid), str(args.sample_seconds), "1",
                                "-file", str(run / "cpu.sample.txt")], check=True,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            threads = subprocess.run(["ps", "-M", "-p", str(app.pid)], capture_output=True,
                                     text=True).stdout
            (run / "threads.txt").write_text(threads)
        finally:
            for agent in agents:
                fifo = run / "agents" / (str(agent["pid"]) + ".fifo")
                try:
                    fd = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
                    try:
                        if ticking:
                            os.write(fd, b'{"op":"stop"}\n')
                        os.write(fd, b'{"op":"exit"}\n')
                    finally:
                        os.close(fd)
                except OSError:
                    pass
            try:
                app.wait(timeout=15)
            except subprocess.TimeoutExpired:
                app.terminate()
                app.wait(timeout=10)
    if args.perf and "window" in result:
        reports = parse_perf((run / "stderr.log").read_text(errors="replace"))
        result["perf"] = steady_rates(reports, result["window"]["start_s"],
                                      result["window"]["end_s"])
    (run / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def rate(perf, name, key="n"):
    if not perf:
        return None
    return perf["totals"].get(name, {}).get(key, 0.0) / perf["seconds"]


def per_frame_ms(perf, name):
    if not perf:
        return None
    presented = perf["totals"].get("presented", {}).get("n", 0.0)
    total = perf["totals"].get(name, {}).get("us", 0.0)
    return total / presented / 1000 if presented else None


def fmt(value, digits=2):
    return "–" if value is None else ("%.*f" % (digits, value))


def label(r):
    return r["scenario"] + ("" if r["repeat"] == 0 else " #%d" % r["repeat"])


def summarize(results):
    """Headline table from external proc_pid_rusage, then in-app counters.

    Compare revisions by retired instructions: CPU time and CPU energy both
    swing severalfold with performance/efficiency-core placement."""
    lines = ["| Scenario | M instr/s | CPU % | interrupt wakeups/s | idle wakeups/s | energy mW |",
             "|---|---:|---:|---:|---:|---:|"]
    for r in results:
        p = r.get("process")
        lines.append("| %s | %s | %s | %s | %s | %s |" % (
            label(r), fmt(p and p["minstr_s"], 2), fmt(p and p["cpu_pct"], 3),
            fmt(p and p["interrupt_wakeups_s"], 1), fmt(p and p["idle_wakeups_s"], 1),
            fmt(p and p["energy_mw"], 2)))
    if any(r.get("perf") for r in results):
        lines += ["", "In-app counters (COMETTY_PERF=1 periodic reports inside the window). "
                  "M instr/frame is whole-process instructions per presented frame, idle floor "
                  "included; the other per-frame columns are main-thread elapsed ms.", "",
                  "| Scenario | counted s | frames/s | loop wakes/s timer+event | supervisor polls/s | "
                  "M instr/frame | redraw_total | overlay | background | encode_submit | "
                  "acquire | present | rebuild | prepare |",
                  "|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"]
        for r in results:
            perf = r.get("perf")
            if not perf:
                lines.append("| %s | – |" % label(r))
                continue
            frames = rate(perf, "presented")
            instructions = rate(perf, "process", "instructions")
            lines.append("| %s | %.0f | %s | %s+%s | %s | %s | %s |" % (
                label(r), perf["seconds"], fmt(frames, 2), fmt(rate(perf, "wake_timer"), 1),
                fmt(rate(perf, "wake_event"), 1), fmt(rate(perf, "supervisor_poll"), 1),
                fmt(instructions / 1e6 / frames if frames and instructions else None, 2),
                " | ".join(fmt(per_frame_ms(perf, n), 3) for n in (
                    "redraw_total", "overlay", "background", "encode_submit", "acquire",
                    "present", "rebuild", "prepare"))))
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("output", nargs="?")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cometty")
    parser.add_argument("--scenarios",
                        default="blink,steady,blink-full,steady-full,blink-5tabs,tick-1hz-full,"
                                "tick-10hz-full",
                        help="comma-separated: " + ",".join(SCENARIOS))
    parser.add_argument("--seconds", type=float, default=30.0, help="measured window")
    parser.add_argument("--settle", type=float, default=6.0, help="wait before measuring")
    parser.add_argument("--repeats", type=int, default=1)
    parser.add_argument("--perf", action="store_true", help="also collect COMETTY_PERF=1 reports")
    parser.add_argument("--sample-seconds", type=int, default=0)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("macOS only (proc_pid_rusage, sample)")
    names = args.scenarios.split(",")
    unknown = [n for n in names if n not in SCENARIOS]
    if unknown or len(set(names)) != len(names):
        parser.error("choose distinct scenarios from " + ",".join(SCENARIOS))
    if args.seconds <= 0 or args.settle < 0 or args.repeats < 1 or args.sample_seconds < 0:
        parser.error("seconds/repeats must be positive; settle/sample-seconds non-negative")
    args.binary = args.binary.resolve()
    if not args.binary.is_file():
        parser.error("build the release binary first: cargo build --release --locked")
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = Path(args.output or ROOT / ("target/perf/idle-" + stamp)).resolve()
    if "'" in str(HERE) or "'" in str(output):
        parser.error("repository/output paths must not contain single quotes")
    output.mkdir(parents=True, exist_ok=False)
    source = output / "source"
    source.mkdir()
    for directory in ("src", "scripts/compare"):
        shutil.copytree(ROOT / directory, source / directory)
    for file in ("Cargo.toml", "Cargo.lock"):
        shutil.copy2(ROOT / file, source / file)
    metadata = {"utc": stamp, "arguments": {k: str(v) if isinstance(v, Path) else v
                                           for k, v in vars(args).items()},
                "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest()}
    for key, command in (("revision", ["git", "rev-parse", "HEAD"]),
                         ("status", ["git", "status", "--short"]),
                         ("host", ["sysctl", "-n", "machdep.cpu.brand_string"]),
                         ("os", ["sw_vers"]), ("power", ["pmset", "-g", "batt"])):
        metadata[key] = subprocess.run(command, cwd=ROOT, capture_output=True,
                                       text=True).stdout.strip()
    (output / "environment.json").write_text(json.dumps(metadata, indent=2) + "\n")
    (output / "source.patch").write_bytes(
        subprocess.run(["git", "diff", "--binary"], cwd=ROOT, capture_output=True).stdout)
    print("Artifacts:", output, flush=True)
    results = []
    for repeat in range(args.repeats):
        for name in names:
            print("Running", name, "repeat", repeat, flush=True)
            results.append(run_scenario(args, output, name, repeat))
            print(summarize(results[-1:]).splitlines()[2], flush=True)
    summary = summarize(results)
    (output / "summary.md").write_text(summary)
    (output / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    print(summary)


if __name__ == "__main__":
    main()
