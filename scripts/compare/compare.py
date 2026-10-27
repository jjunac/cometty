#!/usr/bin/env python3
"""Cross-terminal benchmark orchestrator. Run via scripts/compare/compare.sh.

For each run, terminals are visited in a rotated order, and every suite gets a
fresh instance: launch -> place window -> drive agents -> exit agents -> quit.
Raw results go to OUT/raw/<terminal>.jsonl; report.py renders OUT/summary.md.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import procstat  # noqa: E402
import terminals  # noqa: E402

SUITES = ("startup", "idle", "throughput", "memory", "latency")
WORKLOADS = ("ascii", "sgr", "unicode", "scroll_region", "redraw")


class Context:
    def __init__(self, a):
        self.rundir = os.path.abspath(a.out)
        self.termlat = a.termlat
        self.cometty_app = a.cometty_app
        self.revision = a.revision
        self.geometry = {"x": 80, "y": 80, "w": a.width, "h": a.height}
        self.agent = os.path.join(self.rundir, "bin", "agent")
        self.agents_dir = os.path.join(self.rundir, "agents")
        self.seq = 0

    def activate(self, pid):
        if pid:
            subprocess.run([self.termlat, "activate", "--pid", str(pid)], capture_output=True)


def log(msg):
    print("[%s] %s" % (time.strftime("%H:%M:%S"), msg), flush=True)


# ---------------------------------------------------------------- agents

def agent_ids(ctx):
    return {f[:-5] for f in os.listdir(ctx.agents_dir)
            if f.endswith(".json") and f[:-5].isdigit()}


def wait_agent(ctx, known, timeout=60):
    deadline = time.time() + timeout
    while time.time() < deadline:
        new = agent_ids(ctx) - known
        if new:
            aid = sorted(new)[0]
            with open(os.path.join(ctx.agents_dir, aid + ".json")) as f:
                return json.load(f)
        time.sleep(0.02)
    raise RuntimeError("no agent registered within %ds" % timeout)


def send(ctx, agent, op, wait=True, timeout=900):
    ctx.seq += 1
    op = dict(op, seq=ctx.seq)
    fifo = os.path.join(ctx.agents_dir, "%d.fifo" % agent["pid"])
    fd = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
    os.write(fd, (json.dumps(op) + "\n").encode())
    os.close(fd)
    out = os.path.join(ctx.agents_dir, "%d.%d.json" % (agent["pid"], ctx.seq))
    if not wait:
        return out
    return wait_result(out, timeout)


def wait_result(path, timeout=900):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if os.path.exists(path):
            with open(path) as f:
                r = json.load(f)
            if not r.get("ok"):
                raise RuntimeError("agent op failed: %s" % r.get("error"))
            return r["result"]
        time.sleep(0.05)
    raise RuntimeError("timed out waiting for " + path)


def stop(ctx, agent):
    fifo = os.path.join(ctx.agents_dir, "%d.fifo" % agent["pid"])
    fd = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
    os.write(fd, b'{"op": "stop"}\n')
    os.close(fd)


def exit_agent(ctx, agent):
    try:
        fd = os.open(os.path.join(ctx.agents_dir, "%d.fifo" % agent["pid"]), os.O_WRONLY | os.O_NONBLOCK)
        os.write(fd, b'{"op": "exit"}\n')
        os.close(fd)
    except OSError:
        pass
    try:
        os.unlink(os.path.join(ctx.agents_dir, "%d.json" % agent["pid"]))
    except FileNotFoundError:
        pass


def pid_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        return False


# ---------------------------------------------------------------- instance lifecycle

class Instance:
    """One fresh terminal instance with its first agent."""

    def __init__(self, ctx, term):
        self.ctx, self.term, self.agents = ctx, term, []

    def start(self, place=True):
        known = agent_ids(self.ctx)
        t0 = time.time()
        self.term.launch()
        self.launch_wall = t0
        first = wait_agent(self.ctx, known)
        self.agents.append(first)
        self.pids = self.term.pids()
        self.ui = self.term.ui_pid()
        if place:
            g = self.ctx.geometry
            r = subprocess.run([self.ctx.termlat, "place", "--pid", str(self.ui), "--x", str(g["x"]),
                                "--y", str(g["y"]), "--w", str(g["w"]), "--h", str(g["h"])],
                               capture_output=True, text=True)
            self.window = json.loads(r.stdout).get("window") if r.returncode == 0 else None
            time.sleep(0.5)
            first["tty"] = send(self.ctx, first, {"op": "ping"})["tty"]
        return first

    def new_tab(self):
        known = agent_ids(self.ctx)
        self.term.new_tab()
        a = wait_agent(self.ctx, known)
        self.agents.append(a)
        self.pids = self.term.pids()
        return a

    def close_tab(self, agent):
        exit_agent(self.ctx, agent)
        self.agents.remove(agent)

    def sample(self):
        return procstat.sample(self.term.pids())

    def finish(self):
        for a in list(self.agents):
            self.close_tab(a)
        time.sleep(1.0)
        self.term.quit()
        deadline = time.time() + 15
        while time.time() < deadline and any(pid_alive(p) for p in self.term.pids()):
            time.sleep(0.2)
        left = [p for p in self.term.pids() if pid_alive(p)]
        for p in left:
            os.kill(p, 15)
        time.sleep(1.0)


# ---------------------------------------------------------------- suites

def suite_startup(ctx, term, a):
    cycles = []
    for i in range(a.startup_cycles):
        known = agent_ids(ctx)
        t0 = time.time()
        term.launch()
        exe = os.path.join(term.app, "Contents", "MacOS") + "/"
        w = subprocess.run([ctx.termlat, "wait-window", "--exe-prefix", exe, "--timeout", "60"],
                           capture_output=True, text=True)
        win = json.loads(w.stdout) if w.returncode == 0 else {}
        agent = wait_agent(ctx, known)
        cycles.append({"cycle": i, "window_s": win.get("wall", float("nan")) - t0,
                       "agent_s": agent["start_wall"] - t0})
        inst = Instance(ctx, term)
        inst.agents = [agent]
        inst.finish()
        time.sleep(2)
    return {"cycles": cycles}


def suite_idle(ctx, term, a):
    inst = Instance(ctx, term)
    inst.start()
    try:
        time.sleep(a.settle)
        ctx.activate(inst.ui)
        time.sleep(2)
        before = inst.sample()
        t0 = time.monotonic()
        time.sleep(a.idle_seconds)
        after = inst.sample()
        dt = time.monotonic() - t0
        d = procstat.diff(before, after)
        return {"seconds": dt, "cpu_pct": d["cpu_ns"] / 1e9 / dt * 100,
                "wakeups_s": (d["interrupt_wkups"] + d["pkg_idle_wkups"]) / dt,
                "energy_mj_s": d["energy_nj"] / 1e6 / dt, "delta": d,
                "tty": inst.agents[0]["tty"], "window": inst.window, "pids": inst.pids}
    finally:
        inst.finish()


def suite_throughput(ctx, term, a):
    inst = Instance(ctx, term)
    agent = inst.start()
    try:
        out = {"tty": agent["tty"], "window": inst.window, "workloads": {}}
        for w in a.workloads:
            ctx.activate(inst.ui)
            log("  %s throughput %s" % (term.name, w))
            r = send(ctx, agent, {"op": "throughput", "workload": w, "bytes": a.bytes << 20,
                                  "samples": a.samples, "warmup": 1, "pids": inst.pids})
            out["workloads"][w] = r
        out["peak_footprint"] = inst.sample()["total"]["lifetime_max_phys_footprint"]
        return out
    finally:
        inst.finish()


def suite_memory(ctx, term, a):
    inst = Instance(ctx, term)
    agent = inst.start()
    fpdir = os.path.join(ctx.rundir, "footprint")
    os.makedirs(fpdir, exist_ok=True)
    points = []

    def checkpoint(label):
        time.sleep(a.settle)
        s = inst.sample()
        path = os.path.join(fpdir, "%s-run%d-%s.txt" % (term.name, a.current_run, label))
        with open(path, "w") as f:
            f.write(procstat.footprint_breakdown(inst.term.pids()))
        points.append({"label": label, "tabs": len(inst.agents),
                       "footprint": s["total"]["phys_footprint"],
                       "peak": s["total"]["lifetime_max_phys_footprint"],
                       "rss": s["total"]["resident_size"], "breakdown": os.path.relpath(path, ctx.rundir)})
        log("  %s memory %-14s %.1f MiB" % (term.name, label, s["total"]["phys_footprint"] / 2**20))

    try:
        checkpoint("fresh")
        send(ctx, agent, {"op": "fill", "kind": "ascii", "lines": a.fill_lines})
        checkpoint("ascii_10k")
        send(ctx, agent, {"op": "fill", "kind": "sgr", "lines": a.fill_lines})
        checkpoint("sgr_10k")
        for _ in range(a.tabs - 1):
            t = inst.new_tab()
            send(ctx, t, {"op": "fill", "kind": "sgr", "lines": a.fill_lines})
        checkpoint("tabs_%d_sgr" % a.tabs)
        for t in inst.agents[1:]:
            inst.close_tab(t)
        checkpoint("closed_to_1")
        return {"points": points, "tty": agent["tty"], "window": inst.window}
    finally:
        inst.finish()


def run_latency(ctx, inst, agent, a):
    ready = send(ctx, agent, {"op": "latency"}, wait=False) + ".ready"
    deadline = time.time() + 20
    while not os.path.exists(ready) and time.time() < deadline:
        time.sleep(0.05)
    r = subprocess.run([ctx.termlat, "latency", "--pid", str(inst.ui), "--samples", str(a.latency_samples)],
                       capture_output=True, text=True)
    stop(ctx, agent)
    res = wait_result(ready[: -len(".ready")])
    if r.returncode != 0:
        raise RuntimeError("termlat latency: " + r.stderr.strip())
    lat = json.loads(r.stdout)
    # Calibration keys precede the samples; pair the rest in order.
    arrivals = res["arrival_ns"][lat.get("calibration_keys", 2):]
    if len(arrivals) == len(lat["samples"]):
        for s, t in zip(lat["samples"], arrivals):
            s["input_ms"] = (t - s["post_ns"]) / 1e6
    lat["tty"] = res["tty"]
    return lat


def suite_latency(ctx, term, a):
    inst = Instance(ctx, term)
    first = inst.start()
    try:
        time.sleep(a.settle)
        out = {"window": inst.window, "idle": run_latency(ctx, inst, first, a)}
        # Loaded: the latency agent runs in a new (active) tab while the first tab floods.
        second = inst.new_tab()
        time.sleep(1)
        flood = send(ctx, first, {"op": "flood", "seconds": 600}, wait=False)
        time.sleep(1)
        out["loaded"] = run_latency(ctx, inst, second, a)
        stop(ctx, first)
        wait_result(flood)
        return out
    finally:
        inst.finish()


SUITE_FN = {"startup": suite_startup, "idle": suite_idle, "throughput": suite_throughput,
            "memory": suite_memory, "latency": suite_latency}


# ---------------------------------------------------------------- safety

def host_paths():
    """Executable paths of this process's ancestors (the host terminal among them)."""
    paths, pid = [], os.getppid()
    by_pid = dict(terminals.processes())
    while pid > 1:
        paths.append(by_pid.get(pid, ""))
        r = subprocess.run(["ps", "-o", "ppid=", "-p", str(pid)], capture_output=True, text=True)
        pid = int(r.stdout.strip() or 1)
    return paths


def preflight(ctx, terms, a):
    hosts = host_paths()
    bad = []
    for t in terms:
        if not t.available():
            bad.append("%s: not installed at %s" % (t.name, t.app))
            continue
        if any(p.startswith(t.app + "/") for p in hosts):
            bad.append("%s: it hosts this run; run compare.sh from another terminal" % t.name)
            continue
        running = t.pids()
        if running:
            if not a.quit_running:
                bad.append("%s: already running (pids %s); quit it or pass --quit-running"
                           % (t.name, " ".join(map(str, running))))
                continue
            ans = input("%s is running; quitting it closes its windows and sessions. Quit? [y/N] " % t.name)
            if ans.strip().lower() != "y":
                bad.append("%s: left running" % t.name)
                continue
            t.quit()
            time.sleep(3)
            if t.pids():
                bad.append("%s: still running after quit" % t.name)
    if "latency" in a.suites:
        chk = json.loads(subprocess.run([ctx.termlat, "check"], capture_output=True, text=True).stdout)
        if not chk.get("screen_recording"):
            bad.append("latency: grant Screen Recording to the host terminal (System Settings > "
                       "Privacy & Security), then restart it")
    chk = json.loads(subprocess.run([ctx.termlat, "check"], capture_output=True, text=True).stdout)
    if not chk.get("accessibility"):
        bad.append("grant Accessibility to the host terminal (System Settings > Privacy & Security)")
    if bad:
        sys.exit("preflight failed:\n  " + "\n  ".join(bad))
    return chk


# ---------------------------------------------------------------- main

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--out", required=True)
    p.add_argument("--termlat", required=True)
    p.add_argument("--cometty-app", required=True)
    p.add_argument("--revision", default="?")
    p.add_argument("--terminals", default="cometty,terminal,iterm2,ghostty,kitty")
    p.add_argument("--suites", default=",".join(SUITES))
    p.add_argument("--workloads", default=",".join(WORKLOADS))
    p.add_argument("--runs", type=int, default=3)
    p.add_argument("--bytes", type=int, default=32, help="MiB per throughput sample")
    p.add_argument("--samples", type=int, default=5)
    p.add_argument("--startup-cycles", type=int, default=3)
    p.add_argument("--idle-seconds", type=float, default=60)
    p.add_argument("--settle", type=float, default=5)
    p.add_argument("--fill-lines", type=int, default=10000)
    p.add_argument("--tabs", type=int, default=10)
    p.add_argument("--latency-samples", type=int, default=200)
    p.add_argument("--cooldown", type=float, default=10)
    p.add_argument("--width", type=int, default=1000)
    p.add_argument("--height", type=int, default=700)
    p.add_argument("--quit-running", action="store_true")
    a = p.parse_args()
    a.suites = [s for s in a.suites.split(",") if s]
    a.workloads = [w for w in a.workloads.split(",") if w]
    for s in a.suites:
        if s not in SUITES:
            sys.exit("unknown suite %s (have %s)" % (s, ", ".join(SUITES)))

    ctx = Context(a)
    os.makedirs(ctx.agents_dir, exist_ok=True)
    os.makedirs(os.path.join(ctx.rundir, "raw"), exist_ok=True)
    os.makedirs(os.path.dirname(ctx.agent), exist_ok=True)
    with open(ctx.agent, "w") as f:
        f.write("#!/bin/sh\nexec /usr/bin/python3 -I %s agent %s\n"
                % (os.path.join(HERE, "driver.py"), ctx.rundir))
    os.chmod(ctx.agent, 0o755)

    names = [n for n in a.terminals.split(",") if n]
    for n in names:
        if n not in terminals.ADAPTERS:
            sys.exit("unknown terminal %s (have %s)" % (n, ", ".join(terminals.ADAPTERS)))
    terms = [terminals.ADAPTERS[n](ctx) for n in names]
    display = preflight(ctx, terms, a)

    meta = {"display": display, "args": vars(a), "geometry": ctx.geometry,
            "font": "%s %d" % (terminals.FONT_FAMILY, terminals.FONT_SIZE),
            "versions": {t.name: t.version() for t in terms}}
    with open(os.path.join(ctx.rundir, "meta.json"), "w") as f:
        json.dump(meta, f, indent=2)

    prepared = []
    try:
        for t in terms:
            t.prepare()
            prepared.append(t)
        for run in range(a.runs):
            a.current_run = run
            order = terms[run % len(terms):] + terms[: run % len(terms)]
            for t in order:
                for s in a.suites:
                    log("run %d/%d %s %s" % (run + 1, a.runs, t.name, s))
                    try:
                        data = SUITE_FN[s](ctx, t, a)
                        rec = {"terminal": t.name, "run": run, "suite": s, "ok": True, "data": data}
                    except Exception as e:  # keep going; the report shows the failure
                        log("  FAILED: %r" % e)
                        rec = {"terminal": t.name, "run": run, "suite": s, "ok": False, "error": repr(e)}
                        Instance(ctx, t).finish()
                    with open(os.path.join(ctx.rundir, "raw", t.name + ".jsonl"), "a") as f:
                        f.write(json.dumps(rec) + "\n")
                    time.sleep(a.cooldown)
    finally:
        for t in prepared:
            try:
                t.cleanup()
            except Exception as e:
                log("cleanup %s failed: %r" % (t.name, e))
        shutil.rmtree(ctx.agents_dir, ignore_errors=True)


if __name__ == "__main__":
    main()
