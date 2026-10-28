#!/usr/bin/env python3
"""Replay compare/driver.py's exact workloads headlessly (no GUI or PTY).

Usage: python3 scripts/bench-throughput.py [NEW_OUTPUT_DIRECTORY]
Environment: COMETTY_BENCH_{COLS,ROWS,CHUNK,SAMPLES,CASE,LANE,PROFILE_SECONDS}.
CASE/LANE accept comma-separated filters. Outputs and source snapshots are kept
under target/perf by default; BENCHMARK.md is never modified.
"""
import argparse
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", nargs="?")
    parser.add_argument("--mib", type=int, default=2, help="MiB per corpus/sample (default: 2)")
    parser.add_argument("--generate-only", action="store_true", help="prepare corpora without running Rust")
    parser.add_argument("--layout", action="store_true", help="measure viewport layout/font fallback instead of parsing")
    args = parser.parse_args()
    if args.mib <= 0:
        parser.error("--mib must be positive")
    root = Path(__file__).resolve().parent.parent
    os.chdir(root)
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = Path(args.output or "target/perf/throughput-" + stamp).resolve()
    output.mkdir(parents=True, exist_ok=False)
    corpus_dir = output / "corpus"
    corpus_dir.mkdir()
    source = output / "source"
    source.mkdir()
    for directory in ("src", "assets", "scripts/compare"):
        shutil.copytree(directory, source / directory)
    if (root / "vendor").exists():
        shutil.copytree(root / "vendor", source / "vendor")
    for file in ("Cargo.toml", "Cargo.lock", "scripts/bench-throughput.py"):
        destination = source / file
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(file, destination)
    env = os.environ.copy()
    env.update(RUST_LOG="off", COMETTY_PERF="0", COMETTY_BENCH_CORPUS=str(corpus_dir))
    cols, rows = int(env.get("COMETTY_BENCH_COLS", 118)), int(env.get("COMETTY_BENCH_ROWS", 37))
    size = args.mib << 20
    sys.dont_write_bytecode = True
    sys.path.insert(0, str(root / "scripts/compare"))
    spec = importlib.util.spec_from_file_location("driver", root / "scripts/compare/driver.py")
    driver = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(driver)
    manifest = {}

    def save(name, payload, units):
        (corpus_dir / (name + ".vt")).write_bytes(payload)
        manifest[name] = {"bytes": len(payload), "units": units,
                          "sha256": hashlib.sha256(payload).hexdigest()}

    for name, (build, _) in driver.WORKLOADS.items():
        save(name, *build(cols, rows, size))
    # Controlled variants isolate scrolling and kinds of Unicode work. These
    # are diagnostics, NOT additional cross-terminal comparison scores.
    line = driver._ascii_line(0, min(cols, 81)).rstrip(b"\r\n") + b"\r"
    save("ascii_overwrite", line * (size // len(line) + 1), 0)
    for name, cluster, width in (("cjk", "漢", 2), ("combining", "e\u0301", 1),
                                 ("zwj", "👨\u200d👩\u200d👧", 2)):
        line = (cluster * ((min(cols, 81) - 2) // width) + "\r\n").encode()
        save(name, line * (size // len(line) + 1), 0)
    (output / "corpus.json").write_text(json.dumps(manifest, indent=2) + "\n")
    metadata = {"utc": stamp, "grid": [cols, rows], "arguments": vars(args),
                "environment": {k: v for k, v in env.items()
                                if k.startswith(("COMETTY_BENCH_", "CARGO_", "RUST"))}}
    for name, command in (("revision", ["git", "rev-parse", "HEAD"]),
                           ("status", ["git", "status", "--short"]),
                           ("rustc", ["rustc", "-Vv"]), ("host", ["uname", "-a"])):
        metadata[name] = subprocess.check_output(command, text=True).strip()
    (output / "environment.json").write_text(json.dumps(metadata, indent=2) + "\n")
    (output / "source.patch").write_bytes(subprocess.check_output(["git", "diff", "--binary"]))
    print("Artifacts:", output, flush=True)
    if args.generate_only:
        return
    test = "term::throughput_bench::" + ("layout" if args.layout else "replay")
    command = ["cargo", "test", "--release", "--locked", test, "--",
               "--ignored", "--exact", "--nocapture", "--test-threads=1"]
    with (output / "replay.log").open("w") as log:
        subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
    for line in (output / "replay.log").read_text().splitlines():
        for marker in ("THROUGHPUT ", "LAYOUT "):
            if marker in line:
                print(line[line.index(marker):])
                break


if __name__ == "__main__":
    main()
