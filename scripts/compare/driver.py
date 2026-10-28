#!/usr/bin/env python3
"""In-terminal benchmark agent for scripts/compare/compare.sh.

Each benchmarked terminal runs `python3 driver.py agent RUNDIR` as the command of
every window/tab, so no shell, rc file, or typed command is involved. The agent
registers RUNDIR/agents/<pid>.json (tty size, start time), then reads one JSON op
per line from its FIFO RUNDIR/agents/<pid>.fifo and answers each op with
RUNDIR/agents/<pid>.<seq>.json. Blocking on the FIFO keeps an idle agent at zero
wakeups. Stdlib only, Python 3.9 compatible.

Ops: ping, throughput, fill, flood, latency, exit (see `OPS`).

Throughput timing follows vtebench: write a pre-generated payload, then DA1
(ESC [ c) and wait for the reply. The reply proves the terminal parsed every
earlier byte; it does not prove the bytes were presented on screen.
"""
import json
import os
import select
import sys
import termios
import time
import tty

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

DA1 = b"\x1b[c"


def now_ns():
    # Same clock as mach_absolute_time in ns; termlat uses it too.
    return time.clock_gettime_ns(time.CLOCK_UPTIME_RAW)


def atomic_json(path, obj):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(obj, f)
    os.replace(tmp, path)


def tty_size():
    sz = os.get_terminal_size(sys.stdout.fileno())
    return {"cols": sz.columns, "rows": sz.lines}


# ---------------------------------------------------------------- payloads

def _ascii_line(i, width):
    body = ("%08d " % i) + "the quick brown fox jumps over the lazy dog 0123456789 " * 4
    return body[: width - 1].encode() + b"\r\n"


def _sgr_line(i, width):
    out = []
    for c in range(width - 1):
        n = (i + c) % 256
        out.append("\x1b[38;5;%dm\x1b[48;5;%dm%c" % (n, 255 - n, 33 + (i + c) % 94))
    return ("".join(out) + "\x1b[0m\r\n").encode()


_UNI_CLUSTERS = [
    # (cluster, cells)
    ("\u6f22", 2), ("\u5b57", 2), ("\u30c6", 2), ("\ud55c", 2),  # wide CJK/Hangul
    ("\U0001F468\u200d\U0001F469\u200d\U0001F467", 2),          # ZWJ family
    ("\U0001F44D\U0001F3FD", 2),                                 # skin-tone modifier
    ("e\u0301", 1), ("a\u0308", 1), ("o\u0323\u0302", 1),         # combining marks
    ("x", 1), ("y", 1),
]


def _unicode_line(i, width):
    out, cells, k = [], 0, i
    while True:
        cl, w = _UNI_CLUSTERS[k % len(_UNI_CLUSTERS)]
        if cells + w > width - 2:
            break
        out.append(cl)
        cells += w
        k += 1
    return ("".join(out) + "\r\n").encode()


def _repeat_lines(gen, width, target):
    lines, size, i = [], 0, 0
    while size < target:
        ln = gen(i, width)
        lines.append(ln)
        size += len(ln)
        i += 1
    return b"".join(lines), i


def _scroll_region(width, rows, target):
    # Scrolling inside DECSTBM margins (status lines stay fixed), like vtebench.
    body, n = _repeat_lines(_ascii_line, width, target)
    head = b"\x1b[2;%dr\x1b[%d;1H" % (max(rows - 1, 3), max(rows - 1, 3))
    return head + body.replace(b"\r\n", b"\n\r") + b"\x1b[r\x1b[%d;1H" % rows, n


def _redraw(width, rows, target):
    # Full-screen cursor-addressed TUI frames on the alternate screen.
    frames, size, f = [b"\x1b[?1049h"], 0, 0
    while size < target:
        parts = []
        for r in range(1, rows + 1):
            n = (f + r) % 216 + 16
            text = ("%06d|%03d " % (f, r) + "#" * width)[:width]
            parts.append("\x1b[%d;1H\x1b[38;5;%dm%s" % (r, n, text))
        frame = ("".join(parts) + "\x1b[0m").encode()
        frames.append(frame)
        size += len(frame)
        f += 1
    frames.append(b"\x1b[?1049l")
    return b"".join(frames), f


WORKLOADS = {
    # name: (builder(width, rows, target_bytes) -> (payload, units), unit)
    "ascii": (lambda w, r, t: _repeat_lines(_ascii_line, min(w, 81), t), "lines"),
    "sgr": (lambda w, r, t: _repeat_lines(_sgr_line, min(w, 81), t), "lines"),
    "unicode": (lambda w, r, t: _repeat_lines(_unicode_line, min(w, 81), t), "lines"),
    "scroll_region": (lambda w, r, t: _scroll_region(min(w, 81), r, t), "lines"),
    "redraw": (_redraw, "frames"),
}


# ---------------------------------------------------------------- tty helpers

class Raw:
    """Raw, no-echo tty for the duration of an op; always restored."""

    def __enter__(self):
        self.fd = sys.stdin.fileno()
        self.saved = termios.tcgetattr(self.fd)
        tty.setraw(self.fd)
        return self

    def __exit__(self, *exc):
        termios.tcsetattr(self.fd, termios.TCSAFLUSH, self.saved)


def write_all(data):
    fd, view = sys.stdout.fileno(), memoryview(data)
    while view:
        n = os.write(fd, view)
        view = view[n:]


def da1_sync(timeout=300.0):
    """Send DA1 and wait for its reply (ESC [ ? ... c). Returns False on timeout."""
    write_all(DA1)
    fd, buf, deadline = sys.stdin.fileno(), b"", time.monotonic() + timeout
    while time.monotonic() < deadline:
        r, _, _ = select.select([fd], [], [], max(0.0, deadline - time.monotonic()))
        if not r:
            break
        buf += os.read(fd, 4096)
        i = buf.find(b"\x1b[?")
        if i >= 0 and buf.find(b"c", i) >= 0:
            return True
    return False


# ---------------------------------------------------------------- ops

def op_ping(op, ctx):
    return {"tty": tty_size()}


def op_throughput(op, ctx):
    # Keep payload generation importable on non-macOS headless benchmark hosts.
    import procstat

    name = op["workload"]
    build, unit = WORKLOADS[name]
    size = tty_size()
    payload, units = build(size["cols"], size["rows"], int(op.get("bytes", 16 << 20)))
    pids = op.get("pids", [])
    samples = []
    with Raw():
        write_all(b"\x1b[?25l\x1b[2J\x1b[H")
        for i in range(int(op.get("warmup", 1)) + int(op.get("samples", 5))):
            before = procstat.sample(pids)
            t0 = time.perf_counter()
            write_all(payload)
            ok = da1_sync()
            dt = time.perf_counter() - t0
            after = procstat.sample(pids)
            if i < int(op.get("warmup", 1)):
                continue
            samples.append({
                "ok": ok, "seconds": dt, "bytes": len(payload), unit: units,
                "mib_s": len(payload) / dt / (1 << 20),
                "units_s": units / dt,
                "terminal": procstat.diff(before, after),
            })
        write_all(b"\x1b[0m\x1b[?25h\x1b[2J\x1b[H")
    return {"workload": name, "unit": unit, "tty": size, "samples": samples}


def op_fill(op, ctx):
    gen = {"ascii": _ascii_line, "sgr": _sgr_line, "unicode": _unicode_line}[op["kind"]]
    width = min(tty_size()["cols"], 81)
    payload = b"".join(gen(i, width) for i in range(int(op.get("lines", 10000))))
    with Raw():
        t0 = time.perf_counter()
        write_all(payload)
        ok = da1_sync()
    return {"ok": ok, "seconds": time.perf_counter() - t0, "bytes": len(payload)}


def op_flood(op, ctx):
    """Background ASCII output until `seconds` elapse or a `stop` line arrives."""
    width = min(tty_size()["cols"], 81)
    chunk, _ = _repeat_lines(_ascii_line, width, 64 << 10)
    end, total = time.monotonic() + float(op.get("seconds", 10)), 0
    while time.monotonic() < end:
        write_all(chunk)
        total += len(chunk)
        line = ctx["reader"].readline_nonblock()
        if line is not None and json.loads(line).get("op") == "stop":
            break
    return {"bytes": total}


def op_latency(op, ctx):
    """Toggle a reverse-video block on every key; termlat watches the pixels.

    Each read toggles drawn/erased, regardless of which key arrived, so the
    keyboard layout does not matter. Ends on a `stop` line on the control FIFO.
    """
    size = tty_size()
    cols, rows = int(op.get("block_cols", 24)), int(op.get("block_rows", 6))
    cols, rows = min(cols, size["cols"] - 4), min(rows, size["rows"] - 3)
    draw = b"".join(b"\x1b[%d;3H\x1b[7m%s" % (2 + r, b" " * cols) for r in range(rows)) + b"\x1b[0m"
    erase = b"".join(b"\x1b[%d;3H%s" % (2 + r, b" " * cols) for r in range(rows))
    arrivals, drawn = [], False
    fd, ctl = sys.stdin.fileno(), ctx["ctl"]
    with Raw():
        write_all(b"\x1b[?25l\x1b[0m\x1b[2J\x1b[H")
        atomic_json(ctx["ready_path"], {"tty": size, "block": [cols, rows]})
        while True:
            r, _, _ = select.select([fd, ctl], [], [])
            if fd in r:
                os.read(fd, 1024)
                t = now_ns()
                drawn = not drawn
                write_all(draw if drawn else erase)
                arrivals.append(t)
            if ctl in r:
                line = ctx["reader"].readline_nonblock()
                if line is not None and json.loads(line).get("op") == "stop":
                    break
        write_all(b"\x1b[0m\x1b[?25h\x1b[2J\x1b[H")
    return {"tty": size, "arrival_ns": arrivals}


OPS = {"ping": op_ping, "throughput": op_throughput, "fill": op_fill,
       "flood": op_flood, "latency": op_latency}


class LineReader:
    def __init__(self, fd):
        self.fd, self.buf = fd, b""

    def _pop(self):
        i = self.buf.find(b"\n")
        if i < 0:
            return None
        line, self.buf = self.buf[:i], self.buf[i + 1:]
        return line.decode()

    def readline(self):
        while True:
            line = self._pop()
            if line is not None:
                return line
            select.select([self.fd], [], [])
            self.buf += os.read(self.fd, 65536)

    def readline_nonblock(self):
        r, _, _ = select.select([self.fd], [], [], 0)
        if r:
            self.buf += os.read(self.fd, 65536)
        return self._pop()


def agent(rundir):
    start_ns, start_wall = now_ns(), time.time()
    adir = os.path.join(rundir, "agents")
    os.makedirs(adir, exist_ok=True)
    me = str(os.getpid())
    fifo = os.path.join(adir, me + ".fifo")
    if not os.path.exists(fifo):
        os.mkfifo(fifo, 0o600)
    # O_RDWR: never sees EOF when the orchestrator closes, and open never blocks.
    ctl = os.open(fifo, os.O_RDWR)
    reader = LineReader(ctl)
    sys.stdout.write("\x1b]2;cometty-bench agent %s\x07" % me)
    sys.stdout.write("cometty bench agent %s waiting for ops\r\n" % me)
    sys.stdout.flush()
    atomic_json(os.path.join(adir, me + ".json"), {
        "pid": os.getpid(), "ppid": os.getppid(), "tty": tty_size(),
        "start_ns": start_ns, "start_wall": start_wall,
        "term": os.environ.get("TERM", ""), "term_program": os.environ.get("TERM_PROGRAM", ""),
    })
    while True:
        op = json.loads(reader.readline())
        if op.get("op") == "exit":
            os.close(ctl)
            os.unlink(fifo)
            return
        seq = op.get("seq", 0)
        out_path = os.path.join(adir, "%s.%s.json" % (me, seq))
        ctx = {"ctl": ctl, "reader": reader, "ready_path": out_path + ".ready"}
        try:
            result = {"ok": True, "op": op["op"], "result": OPS[op["op"]](op, ctx)}
        except Exception as e:  # report instead of killing the tab
            result = {"ok": False, "op": op.get("op"), "error": repr(e)}
        atomic_json(out_path, result)


def main(argv):
    if len(argv) == 2 and argv[0] == "agent":
        try:
            agent(argv[1])
        except Exception as e:
            # Keep the window open so a broken setup is visible.
            sys.stdout.write("\r\nagent failed: %r\r\n" % e)
            sys.stdout.flush()
            time.sleep(3600)
        return
    sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
