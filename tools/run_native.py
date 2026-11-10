#!/usr/bin/env python3
"""Run the original vcl natively on an x86 Linux host, keeping oracle-style evidence.

Example (on a machine with 32-bit support, from a copy of this repository):
  python3 tools/run_native.py --name general_quad \
      --input local/evidence/corpus/selected-oracle/openvcl__test_fixtures_general_quad/input.vcl \
      -- -t1 -uCORPUS @INPUT@

The binary runs through the analysis rootfs's own i386 loader and libraries
(glibc 2.31, libstdc++.so.5), so the host needs no i386 packages, only a
kernel that runs 32-bit executables. Unlike tools/run_oracle.py the watchdog
is the real alarm(2), so `-t` and the per-run budgets are wall seconds at
native speed. Output lands in --out/NAME as stdout.bin, stderr.bin and
result.json, the same layout the comparison tools read.
"""

import argparse
import hashlib
import json
import os
import pathlib
import platform
import shutil
import subprocess
import tempfile
import time

VCL_SHA256 = "83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274"


def digest(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--rootfs", type=pathlib.Path, default=pathlib.Path("local/analysis/runtime/rootfs"))
    parser.add_argument("--binary", type=pathlib.Path, default=pathlib.Path("local/vcl"))
    parser.add_argument("--name", required=True)
    parser.add_argument("--input", type=pathlib.Path)
    parser.add_argument("--out", type=pathlib.Path, default=pathlib.Path("local/evidence/native"))
    parser.add_argument("--timeout", type=float, default=None, help="wall-time limit in seconds")
    parser.add_argument("args", nargs=argparse.REMAINDER)
    options = parser.parse_args()
    arguments = options.args[1:] if options.args[:1] == ["--"] else options.args
    binary = options.binary.read_bytes()
    if digest(binary) != VCL_SHA256:
        raise SystemExit("Refusing an oracle binary with unexpected SHA256")
    rootfs = options.rootfs.resolve()
    loader = rootfs / "lib/ld-linux.so.2"
    libraries = ":".join(str(rootfs / path) for path in ("lib/i386-linux-gnu", "usr/lib/i386-linux-gnu", "lib"))
    output_dir = options.out / options.name
    output_dir.mkdir(parents=True, exist_ok=True)
    source = options.input.read_bytes() if options.input else None
    with tempfile.TemporaryDirectory(prefix="vcl-native-") as directory:
        work = pathlib.Path(directory)
        (work / "vcl").write_bytes(binary)
        (work / "vcl").chmod(0o755)
        if source is not None:
            (output_dir / "input.vcl").write_bytes(source)
            (work / options.input.name).write_bytes(source)
            arguments = [argument.replace("@INPUT@", options.input.name) for argument in arguments]
        command = [str(loader), "--library-path", libraries, "./vcl"] + arguments
        start_wall = time.monotonic()
        before = os.times()
        try:
            completed = subprocess.run(command, cwd=work, capture_output=True, timeout=options.timeout,
                                       env={"PATH": "/bin:/usr/bin"})
            stdout, stderr, code, timed_out = completed.stdout, completed.stderr, completed.returncode, False
        except subprocess.TimeoutExpired as expired:
            stdout, stderr, code, timed_out = expired.stdout or b"", expired.stderr or b"", None, True
        after = os.times()
        wall = time.monotonic() - start_wall
        generated = output_dir / "generated"
        for path in work.rglob("*"):
            if path.is_file() and path.name != "vcl" and (options.input is None or path.name != options.input.name):
                target = generated / path.relative_to(work)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, target)
    (output_dir / "stdout.bin").write_bytes(stdout)
    (output_dir / "stderr.bin").write_bytes(stderr)
    metadata = {
        "binary_sha256": digest(binary), "guest_argv": ["vcl"] + arguments,
        "input_sha256": digest(source) if source is not None else None,
        "native": True, "host": platform.platform(), "machine": platform.machine(),
        "exit_code": code, "wall_timeout": timed_out, "wall_seconds": wall,
        "cpu_seconds": (after.children_user - before.children_user) + (after.children_system - before.children_system),
        "stdout_sha256": digest(stdout), "stderr_sha256": digest(stderr),
        "time_outs": stdout.count(b"time out..") + stderr.count(b"time out.."),
    }
    (output_dir / "result.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(json.dumps({"name": options.name, **metadata}))


if __name__ == "__main__":
    main()
