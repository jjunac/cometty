#!/usr/bin/env python3
"""Render OUT/summary.md (and summary.csv) from OUT/raw/*.jsonl.

Medians across runs/samples; spreads are min-max. Usage: report.py OUT
"""
import csv
import glob
import json
import math
import os
import statistics
import sys


def med(xs):
    xs = [x for x in xs if x is not None and not (isinstance(x, float) and math.isnan(x))]
    return statistics.median(xs) if xs else None


def pct(xs, p):
    xs = sorted(xs)
    if not xs:
        return None
    k = (len(xs) - 1) * p / 100
    lo, hi = int(math.floor(k)), int(math.ceil(k))
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def fmt(v, digits=1):
    if v is None:
        return "-"
    return ("%%.%df" % digits) % v


def load(out):
    recs = {}
    for path in sorted(glob.glob(os.path.join(out, "raw", "*.jsonl"))):
        with open(path) as f:
            for line in f:
                r = json.loads(line)
                recs.setdefault(r["terminal"], []).append(r)
    return recs


def environment(out):
    """Host facts from compare.sh's environment.txt (missing for hand-made dirs)."""
    path = os.path.join(out, "environment.txt")
    if not os.path.exists(path):
        return {}
    env = {}
    with open(path) as f:
        for line in f:
            line = line.strip()
            key, sep, value = line.partition("=")
            if sep and key in ("utc", "revision"):
                env[key] = value
            elif line.startswith("ProductVersion:"):
                env["macos"] = line.split(":", 1)[1].strip()
            elif line.startswith("Apple ") or line.startswith("Intel"):
                env.setdefault("cpu", line)
            elif line.startswith("Now drawing from"):
                env["power"] = line.replace("Now drawing from", "").strip(" '")
    return env


def ok(recs, term, suite):
    return [r["data"] for r in recs.get(term, []) if r["suite"] == suite and r["ok"]]


def table(head, rows):
    out = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    out += ["| " + " | ".join(str(c) for c in row) + " |" for row in rows]
    return "\n".join(out) + "\n"


def main(out):
    recs = load(out)
    meta = {}
    if os.path.exists(os.path.join(out, "meta.json")):
        with open(os.path.join(out, "meta.json")) as f:
            meta = json.load(f)
    terms = [t for t in meta.get("args", {}).get("terminals", "").split(",") if t in recs] or sorted(recs)
    md, flat = [], []

    def add(term, suite, metric, value):
        if value is not None:
            flat.append({"terminal": term, "suite": suite, "metric": metric, "value": value})

    md.append("# Terminal comparison\n")
    env = environment(out)
    if env:
        md.append("Measured %s on %s, macOS %s, %s; cometty revision `%s`. Raw data: `%s`.\n" % (
            env.get("utc", "?"), env.get("cpu", "?"), env.get("macos", "?"), env.get("power", "?"),
            env.get("revision", "?")[:7], os.path.relpath(out)))
        if "Battery" in env.get("power", ""):
            md.append("> **Warning:** measured on battery power; CPU and GPU may have been throttled.\n")
    d = meta.get("display", {})
    md.append("Display %s Hz (max %s fps), backing scale %s; font %s; window %s; runs %s.\n" % (
        d.get("mode_refresh_hz"), d.get("max_fps"), d.get("backing_scale"), meta.get("font"),
        meta.get("geometry"), meta.get("args", {}).get("runs")))
    md.append("Host-specific measurements; see docs/performance.md for what each number does and "
              "does not include. Medians; ranges are min-max across runs.\n")
    md.append(table(["Terminal", "Version"], [[t, meta.get("versions", {}).get(t, "?")] for t in terms]))

    # Geometry actually obtained.
    geo = []
    for t in terms:
        sizes = set()
        for r in recs[t]:
            if r["ok"]:
                tty = r["data"].get("tty")
                if tty:
                    sizes.add("%dx%d" % (tty["cols"], tty["rows"]))
        geo.append([t, ", ".join(sorted(sizes)) or "-"])
    md.append("## Grid size\n\nThroughput, latency and memory depend on cols×rows; differences "
              "come from each terminal's cell metrics, padding and chrome at the same window size.\n")
    md.append(table(["Terminal", "cols×rows"], geo))

    rows = []
    for t in terms:
        cyc = [c for s in ok(recs, t, "startup") for c in s["cycles"]]
        if not cyc:
            continue
        w, ag = [c["window_s"] * 1000 for c in cyc], [c["agent_s"] * 1000 for c in cyc]
        rows.append([t, fmt(med(w), 0), fmt(med(ag), 0), "%s-%s" % (fmt(min(w), 0), fmt(max(w), 0))])
        add(t, "startup", "window_ms", med(w))
        add(t, "startup", "agent_ms", med(ag))
    if rows:
        md.append("## Startup\n\nLaunch command → first on-screen window / → agent process start.\n")
        md.append(table(["Terminal", "window ms", "child ms", "window range"], rows))

    rows = []
    for t in terms:
        s = ok(recs, t, "idle")
        if not s:
            continue
        cpu, wk, en = [x["cpu_pct"] for x in s], [x["wakeups_s"] for x in s], [x["energy_mj_s"] for x in s]
        rows.append([t, fmt(med(cpu), 2), "%s-%s" % (fmt(min(cpu), 2), fmt(max(cpu), 2)),
                     fmt(med(wk), 0), fmt(med(en), 2)])
        add(t, "idle", "cpu_pct", med(cpu))
        add(t, "idle", "wakeups_s", med(wk))
        add(t, "idle", "energy_mw", med(en))
    if rows:
        md.append("## Idle (one window, cursor blink off)\n")
        md.append(table(["Terminal", "CPU %", "CPU range", "wakeups/s", "energy mW"], rows))

    wls = []
    for t in terms:
        for s in ok(recs, t, "throughput"):
            for w in s["workloads"]:
                if w not in wls:
                    wls.append(w)
    if wls:
        md.append("## Throughput (DA1-synchronized; parse, not presentation)\n")
        md.append("MiB/s, median of all samples (higher is better); terminal CPU seconds per MiB in "
                  "parentheses.\n")
        rows = []
        for t in terms:
            row = [t]
            for w in wls:
                samples = [x for s in ok(recs, t, "throughput") for x in s["workloads"].get(w, {}).get("samples", [])]
                rate = med([x["mib_s"] for x in samples if x["ok"]])
                cpu = med([x["terminal"]["cpu_ns"] / 1e9 / (x["bytes"] / 2**20) for x in samples if x["ok"]])
                bad = sum(1 for x in samples if not x["ok"])
                row.append("%s (%s)%s" % (fmt(rate), fmt(cpu, 3), " ⚠%d timeouts" % bad if bad else ""))
                add(t, "throughput", w + "_mib_s", rate)
                add(t, "throughput", w + "_cpu_s_per_mib", cpu)
            rows.append(row)
        md.append(table(["Terminal"] + wls, rows))

    labels = []
    for t in terms:
        for s in ok(recs, t, "memory"):
            for p in s["points"]:
                if p["label"] not in labels:
                    labels.append(p["label"])
    if labels:
        md.append("## Memory (phys_footprint, MiB, all terminal-owned processes)\n")
        md.append("Includes GPU/IOSurface memory attributed to the process. `footprint/` has the "
                  "per-category breakdown for every checkpoint.\n")
        rows = []
        for t in terms:
            row = [t]
            for lb in labels:
                v = med([p["footprint"] / 2**20 for s in ok(recs, t, "memory") for p in s["points"] if p["label"] == lb])
                row.append(fmt(v))
                add(t, "memory", lb + "_mib", v)
            peak = med([max(p["peak"] for p in s["points"]) / 2**20 for s in ok(recs, t, "memory")])
            row.append(fmt(peak))
            add(t, "memory", "peak_mib", peak)
            rows.append(row)
        md.append(table(["Terminal"] + labels + ["peak"], rows))

    rows = []
    for t in terms:
        for mode in ("idle", "loaded"):
            runs = [s[mode] for s in ok(recs, t, "latency") if mode in s]
            if not runs:
                continue
            ms = [x["ms"] for r in runs for x in r["samples"] if "ms" in x]
            inp = [x["input_ms"] for r in runs for x in r["samples"] if "input_ms" in x]
            misses = sum(r["misses"] for r in runs)
            rows.append([t, mode, fmt(pct(ms, 50)), fmt(pct(ms, 95)), fmt(pct(ms, 99)),
                         fmt(max(ms) if ms else None), fmt(pct(inp, 50), 2), misses, len(ms)])
            for p in (50, 95, 99):
                add(t, "latency_" + mode, "p%d_ms" % p, pct(ms, p))
    if rows:
        md.append("## Key → pixel latency (ms)\n")
        md.append("Post of a synthetic key event → display time of the first captured frame showing "
                  "the change. Includes capture quantization at the display rate (common to all). "
                  "`loaded`: another tab floods ASCII output meanwhile. `input` is key post → bytes "
                  "read by the child.\n")
        md.append(table(["Terminal", "mode", "p50", "p95", "p99", "max", "input p50", "misses", "n"], rows))

    fails = [(t, r["run"], r["suite"], r["error"]) for t in terms for r in recs[t] if not r["ok"]]
    if fails:
        md.append("## Failures\n")
        md.append(table(["Terminal", "run", "suite", "error"], [list(f) for f in fails]))

    with open(os.path.join(out, "summary.md"), "w") as f:
        f.write("\n".join(md))
    with open(os.path.join(out, "summary.csv"), "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=["terminal", "suite", "metric", "value"])
        w.writeheader()
        w.writerows(flat)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
