#!/usr/bin/env python3
"""Per-process CPU, wakeup, energy and footprint counters via proc_pid_rusage.

Usage:
  procstat.py PID [PID...]          one JSON object: per-pid counters and their sum
  procstat.py --footprint PID...    also attach `footprint` category breakdown

Also importable: `sample(pids)`. Stdlib only, Python 3.9 compatible (macOS
/usr/bin/python3). Counters are cumulative since process start; callers diff
two samples. CPU/energy fields cover the process itself, never its children.
"""
import ctypes
import ctypes.util
import json
import subprocess
import sys

RUSAGE_INFO_V6 = 6
# Field order of struct rusage_info_v6 (sys/resource.h) after ri_uuid[16].
_FIELDS = (
    "user_time system_time pkg_idle_wkups interrupt_wkups pageins wired_size "
    "resident_size phys_footprint proc_start_abstime proc_exit_abstime "
    "child_user_time child_system_time child_pkg_idle_wkups child_interrupt_wkups "
    "child_pageins child_elapsed_abstime diskio_bytesread diskio_byteswritten "
    "cpu_time_qos_default cpu_time_qos_maintenance cpu_time_qos_background "
    "cpu_time_qos_utility cpu_time_qos_legacy cpu_time_qos_user_initiated "
    "cpu_time_qos_user_interactive billed_system_time serviced_system_time "
    "logical_writes lifetime_max_phys_footprint instructions cycles billed_energy "
    "serviced_energy interval_max_phys_footprint runnable_time flags user_ptime "
    "system_ptime pinstructions pcycles energy_nj penergy_nj"
).split()


class _RusageV6(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(n, ctypes.c_uint64) for n in _FIELDS] + [
        # Remaining v6 fields plus reserved words; oversized on purpose.
        ("_tail", ctypes.c_uint64 * 32)
    ]


class _Timebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


_libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
_libc.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
_libc.proc_pid_rusage.restype = ctypes.c_int
_tb = _Timebase()
_libc.mach_timebase_info(ctypes.byref(_tb))


def _abs_to_ns(v):
    # On Apple Silicon rusage CPU times are mach absolute units, not ns.
    return v * _tb.numer // _tb.denom


def one(pid):
    info = _RusageV6()
    if _libc.proc_pid_rusage(int(pid), RUSAGE_INFO_V6, ctypes.byref(info)) != 0:
        return None
    return {
        "pid": int(pid),
        "cpu_ns": _abs_to_ns(info.user_time + info.system_time),
        "pkg_idle_wkups": info.pkg_idle_wkups,
        "interrupt_wkups": info.interrupt_wkups,
        "phys_footprint": info.phys_footprint,
        "lifetime_max_phys_footprint": info.lifetime_max_phys_footprint,
        "resident_size": info.resident_size,
        "energy_nj": info.energy_nj,
        "instructions": info.instructions,
    }


SUMMED = ("cpu_ns", "pkg_idle_wkups", "interrupt_wkups", "phys_footprint",
          "lifetime_max_phys_footprint", "resident_size", "energy_nj", "instructions")


def sample(pids):
    procs = [p for p in (one(pid) for pid in pids) if p is not None]
    total = {k: sum(p[k] for p in procs) for k in SUMMED}
    return {"procs": procs, "total": total}


def diff(a, b):
    """Counter deltas b - a for the summed totals (footprints are levels, kept from b)."""
    out = {}
    for k in SUMMED:
        if "footprint" in k or k == "resident_size":
            out[k] = b["total"][k]
        else:
            out[k] = b["total"][k] - a["total"][k]
    return out


def footprint_breakdown(pids):
    """Raw `footprint` text (graphics / IOSurface / malloc categories)."""
    try:
        return subprocess.run(["footprint", *map(str, pids)], capture_output=True,
                              text=True, timeout=60).stdout
    except (OSError, subprocess.TimeoutExpired) as e:
        return "footprint failed: %s" % e


def main(argv):
    want_fp = "--footprint" in argv
    pids = [int(a) for a in argv if a != "--footprint"]
    if not pids:
        sys.exit(__doc__)
    out = sample(pids)
    if want_fp:
        out["footprint"] = footprint_breakdown(pids)
    json.dump(out, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main(sys.argv[1:])
