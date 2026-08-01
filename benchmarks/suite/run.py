#!/usr/bin/env python3
"""Run output-validated Gossamer and Go benchmark workloads on Linux."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import pathlib
import platform
import shutil
import statistics
import subprocess
import tempfile
import time


ROOT = pathlib.Path(__file__).resolve().parent
WORKLOADS = ("arithmetic", "allocation", "json", "concurrency", "startup")


def command_text(command: list[str]) -> str:
    return subprocess.check_output(command, text=True, stderr=subprocess.STDOUT).strip()


def host_metadata() -> dict[str, object]:
    cpu_model = "unknown"
    memory_bytes = None
    cpuinfo = pathlib.Path("/proc/cpuinfo")
    if cpuinfo.exists():
        for line in cpuinfo.read_text(encoding="utf-8").splitlines():
            if line.lower().startswith("model name"):
                cpu_model = line.split(":", 1)[1].strip()
                break
    meminfo = pathlib.Path("/proc/meminfo")
    if meminfo.exists():
        for line in meminfo.read_text(encoding="utf-8").splitlines():
            if line.startswith("MemTotal:"):
                memory_bytes = int(line.split()[1]) * 1024
                break
    return {
        "os": platform.system(),
        "arch": platform.machine(),
        "kernel": platform.release(),
        "cpu_count": os.cpu_count() or 1,
        "cpu_model": cpu_model,
        "memory_bytes": memory_bytes,
    }


def compile_command(command: list[str]) -> int:
    started = time.perf_counter_ns()
    subprocess.run(command, check=True)
    return time.perf_counter_ns() - started


def measured_run(command: list[str], expected: bytes, rss_path: pathlib.Path) -> tuple[int, int]:
    started = time.perf_counter_ns()
    completed = subprocess.run(
        ["/usr/bin/time", "-f", "%M", "-o", str(rss_path), *command],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    elapsed = time.perf_counter_ns() - started
    if completed.stdout != expected:
        raise RuntimeError(
            f"output mismatch for {' '.join(command)}: "
            f"expected {expected!r}, found {completed.stdout!r}"
        )
    return elapsed, int(rss_path.read_text(encoding="utf-8").strip())


def result_for(
    workload: str,
    tier: str,
    command: list[str],
    expected: bytes,
    runs: int,
    compile_ns: int,
    binary: pathlib.Path,
    scratch: pathlib.Path,
) -> dict[str, object]:
    subprocess.run(command, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    samples: list[int] = []
    peak_rss = 0
    for index in range(runs):
        elapsed, rss = measured_run(command, expected, scratch / f"rss-{tier}-{index}.txt")
        samples.append(elapsed)
        peak_rss = max(peak_rss, rss)
    median = int(statistics.median(samples))
    mad = int(statistics.median(abs(sample - median) for sample in samples))
    return {
        "workload": workload,
        "tier": tier,
        "samples_ns": samples,
        "median_ns": median,
        "mad_ns": mad,
        "peak_rss_kib": peak_rss,
        "compile_ns": compile_ns,
        "binary_bytes": binary.stat().st_size,
        "output_sha256": hashlib.sha256(expected).hexdigest(),
    }


def compare(current: dict[str, object], baseline_path: pathlib.Path) -> None:
    if not baseline_path.is_file():
        raise RuntimeError(f"benchmark baseline is missing: {baseline_path}")
    baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    if baseline.get("schema_version") != current.get("schema_version"):
        raise RuntimeError("benchmark schema versions differ")
    if baseline.get("host", {}).get("arch") != current.get("host", {}).get("arch"):
        raise RuntimeError("benchmark host architectures differ")
    old = {
        (row["workload"], row["tier"]): row
        for row in baseline.get("results", [])
    }
    failures: list[str] = []
    for row in current["results"]:
        key = (row["workload"], row["tier"])
        if key not in old:
            failures.append(f"missing baseline row: {key[0]}/{key[1]}")
            continue
        prior = old[key]
        time_ratio = row["median_ns"] / max(1, prior["median_ns"])
        rss_ratio = row["peak_rss_kib"] / max(1, prior["peak_rss_kib"])
        status = "warning" if time_ratio >= 1.10 or rss_ratio >= 1.10 else "ok"
        print(
            f"{key[0]}/{key[1]}: time {time_ratio:.3f}x, "
            f"RSS {rss_ratio:.3f}x [{status}]"
        )
        if time_ratio >= 1.25 or rss_ratio >= 1.25:
            failures.append(f"regression: {key[0]}/{key[1]}")
    if failures:
        raise RuntimeError("; ".join(failures))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--gos", type=pathlib.Path, default=pathlib.Path("target/release/gos"))
    parser.add_argument("--go", default="go")
    parser.add_argument("--runs", type=int, default=7)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--compare", type=pathlib.Path)
    args = parser.parse_args()
    if args.runs < 3:
        parser.error("--runs must be at least 3")
    if platform.system() != "Linux" or not pathlib.Path("/usr/bin/time").is_file():
        parser.error("v1 runner requires Linux and /usr/bin/time")
    gos = args.gos.resolve()
    if not gos.is_file():
        parser.error(f"gos binary does not exist: {gos}")
    go = shutil.which(args.go)
    if go is None:
        parser.error(f"Go executable not found: {args.go}")

    results: list[dict[str, object]] = []
    with tempfile.TemporaryDirectory(prefix="gossamer-suite-") as raw_scratch:
        scratch = pathlib.Path(raw_scratch)
        for workload in WORKLOADS:
            directory = ROOT / workload
            expected = (directory / "expected.txt").read_bytes()
            build_dir = scratch / workload
            debug_dir = build_dir / "debug"
            release_dir = build_dir / "release"
            debug_dir.mkdir(parents=True)
            release_dir.mkdir(parents=True)

            go_binary = build_dir / "go-main"
            go_compile = compile_command([go, "build", "-trimpath", "-o", str(go_binary), str(directory / "main.go")])
            debug_compile = compile_command([str(gos), "build", "--out-dir", str(debug_dir), str(directory / "main.gos")])
            release_compile = compile_command([str(gos), "build", "--release", "--out-dir", str(release_dir), str(directory / "main.gos")])
            debug_binary = debug_dir / "main"
            release_binary = release_dir / "main"
            if not debug_binary.is_file() or not release_binary.is_file():
                raise RuntimeError(f"native output missing for {workload}")

            tiers = (
                ("vm-no-jit", [str(gos), "--no-jit", str(directory / "main.gos")], 0, gos),
                ("vm-jit", [str(gos), str(directory / "main.gos")], 0, gos),
                ("llvm-debug", [str(debug_binary)], debug_compile, debug_binary),
                ("llvm-release", [str(release_binary)], release_compile, release_binary),
                ("go-release", [str(go_binary)], go_compile, go_binary),
            )
            for tier, command, compile_ns, binary in tiers:
                print(f"measuring {workload}/{tier}", flush=True)
                results.append(
                    result_for(
                        workload, tier, command, expected, args.runs,
                        compile_ns, binary, scratch,
                    )
                )

    document: dict[str, object] = {
        "schema_version": 1,
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z"),
        "host": host_metadata(),
        "toolchains": {
            "gos": command_text([str(gos), "--version"]),
            "go": command_text([go, "version"]),
            "rustc": command_text(["rustc", "--version"]),
            "llvm": os.environ.get("GOS_LLC", "system/default"),
        },
        "runs": args.runs,
        "results": results,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {args.output}")
    if args.compare is not None:
        compare(document, args.compare)


if __name__ == "__main__":
    main()
