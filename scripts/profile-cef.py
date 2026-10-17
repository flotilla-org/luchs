#!/usr/bin/env python3
"""Measure the CEF GPU producer through a finite, private macOS launchd job."""
import argparse
import csv
import json
import os
import pathlib
import platform
import plistlib
import statistics
import subprocess
import time
import uuid


def distribution(values):
    values = sorted(values)
    return {
        "p50_ms": statistics.median(values),
        "p95_ms": values[int(.95 * (len(values) - 1))],
        "max_ms": max(values),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("page", type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--frames", type=int, default=600)
    parser.add_argument("--warmup", type=int, default=10)
    parser.add_argument("--process-type", choices=("Interactive", "Standard"), default="Interactive")
    args = parser.parse_args()
    if platform.system() != "Darwin":
        parser.error("This probe requires macOS")
    if not 2 <= args.frames <= 600 or not 0 <= args.warmup < args.frames - 1:
        parser.error("Require 2–600 frames and at least two frames after warmup")
    binary = args.binary.resolve(strict=True)
    page = args.page.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    tag = "work.flotilla.luchs.cef-profile." + uuid.uuid4().hex
    # The Unix socket also includes macOS's long per-user temporary directory.
    endpoint = "cef-perf-" + uuid.uuid4().hex[:12]
    target = f"gui/{os.getuid()}/{tag}"
    trace = output / "frames.csv"
    log = output / "source.log"
    job = output / "job.plist"
    job.write_bytes(plistlib.dumps({
        "Label": tag,
        # Standard jobs can have timer/CPU/IO throttling even on an idle Mac.
        "ProcessType": args.process_type,
        "ProgramArguments": [str(binary), "--renderer", "cef", "--fps", "60",
                             "--size", "680x430", "--frames", str(args.frames),
                             "--endpoint", endpoint, str(page)],
        "EnvironmentVariables": {"LUCHS_CEF_GPU": "1", "LUCHS_CEF_TRACE": str(trace)},
        "RunAtLoad": True,
        "StandardOutPath": str(log),
        "StandardErrorPath": str(log),
    }))
    subprocess.run(["launchctl", "bootstrap", f"gui/{os.getuid()}", str(job)], check=True)
    try:
        deadline = time.monotonic() + 90
        next_status = time.monotonic() + 1
        finished = False
        while time.monotonic() < deadline:
            if log.exists() and "elapsed_ms=" in log.read_text():
                finished = True
                break
            if time.monotonic() >= next_status:
                status = subprocess.run(["launchctl", "print", target],
                                        capture_output=True, text=True, check=True)
                if "last exit code =" in status.stdout and "pid =" not in status.stdout:
                    raise RuntimeError(f"Producer exited early; inspect {log}")
                next_status = time.monotonic() + 1
            time.sleep(.2)
        if not trace.exists():
            raise RuntimeError(f"No frames recorded; inspect {log}")
        with trace.open() as stream:
            rows = [list(map(int, row)) for row in csv.reader(stream)]
        frames = rows[args.warmup:]
        if len(frames) < 2:
            raise RuntimeError(f"Insufficient frames; inspect {log}")
        gaps = [(b[0] - a[0]) / 1e6 for a, b in zip(frames, frames[1:])]
        fps = (len(frames) - 1) * 1e9 / (frames[-1][0] - frames[0][0])
        long_gaps = sum(gap > 33.4 for gap in gaps)
        # p95 alone misses repeated 100ms stalls when they affect <5% of frames.
        passed = finished and len(rows) >= args.frames and fps >= 55 and long_gaps == 0
        result = {
            "passed": passed,
            "machine": platform.node(),
            "process_type": args.process_type,
            "finished": finished,
            "frames": len(rows),
            "warmup": args.warmup,
            "fps": fps,
            "arrival_intervals": distribution(gaps),
            "copy_publish_wait": distribution([row[3] / 1e6 for row in frames]),
            "gaps_over_33ms": long_gaps,
        }
        (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2), flush=True)
        return 0 if passed else 1
    finally:
        # launchd also retires this job's subprocess group. No persistent job.
        subprocess.run(["launchctl", "bootout", target],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


if __name__ == "__main__":
    raise SystemExit(main())
