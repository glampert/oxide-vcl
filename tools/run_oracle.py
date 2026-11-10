#!/usr/bin/env python3
"""Execute the supplied original vcl under Qiling and retain exact evidence.

Example:
  local/analysis/runtime/venv/bin/python tools/run_oracle.py --name help -- --help
  local/analysis/runtime/venv/bin/python tools/run_oracle.py --name basic \
      --input local/evidence/inputs/basic.vcl -- --t0 @INPUT@

The @INPUT@ token becomes /work/<input basename>. Raw guest stdout/stderr and
exit code are stored separately from emulator diagnostics. Each run gets a fresh
rootfs copy. Its binary is copied from --binary and checked against the expected
digest before execution. This harness adapts only the emulation/CPU-alarm recipe
from openvcl/docs/sce-vcl/run_vcl.py; no reimplementation behavior is used.
"""

import argparse
import hashlib
import importlib.metadata
import json
import math
import pathlib
import shutil
import tempfile
import threading
import time
import traceback


VCL_SHA256 = "83bee75205b5b0f37dbefc2c2769eba2c353befa532472c49aedb2ebaf168274"


def load_hooks(path):
    """The guest alarm handler and the address of the flag it sets.

    Qiling does not deliver SIGALRM, so the harness does what vcl's handler
    does: when the handler registered for SIGALRM is vcl's, it sets vcl's
    time-out flag. Both addresses belong to the binary, so they are kept with
    it in the untracked local/ directory (`local/oracle.json`:
    {"alarm_handler": "0x...", "alarm_flag_pointer": "0x..."}).
    """
    try:
        hooks = json.loads(path.read_text())
    except FileNotFoundError:
        raise SystemExit(f"{path}: missing; it holds the binary's alarm-handler addresses (see load_hooks)")
    return int(hooks["alarm_handler"], 0), int(hooks["alarm_flag_pointer"], 0)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def main(trace=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", type=pathlib.Path, default=pathlib.Path("local/analysis/runtime"))
    parser.add_argument("--binary", type=pathlib.Path, default=pathlib.Path("local/vcl"))
    parser.add_argument("--hooks", type=pathlib.Path, default=pathlib.Path("local/oracle.json"),
                        help="the binary's alarm-handler addresses (see load_hooks)")
    parser.add_argument("--name", required=True)
    parser.add_argument("--input", type=pathlib.Path)
    parser.add_argument("--stdin", type=pathlib.Path)
    parser.add_argument("--out", type=pathlib.Path, default=pathlib.Path("local/evidence/oracle"))
    parser.add_argument("--time-scale", type=float, default=25.0)
    parser.add_argument("--timeout", type=float, default=60.0, help="emulator wall-time limit in seconds")
    parser.add_argument("args", nargs=argparse.REMAINDER)
    options = parser.parse_args()
    alarm_handler, alarm_flag_pointer = load_hooks(options.hooks)
    arguments = options.args[1:] if options.args[:1] == ["--"] else options.args
    binary = options.binary.read_bytes()
    if digest(binary) != VCL_SHA256:
        raise SystemExit("Refusing an oracle binary with unexpected SHA256")
    output_dir = options.out / options.name
    output_dir.mkdir(parents=True, exist_ok=True)
    source = options.input.read_bytes() if options.input else None
    stdin_data = options.stdin.read_bytes() if options.stdin else b""
    if source is not None:
        (output_dir / "input.vcl").write_bytes(source)
        guest_input = "/work/" + options.input.name
        arguments = [argument.replace("@INPUT@", guest_input) for argument in arguments]
    elif any("@INPUT@" in argument for argument in arguments):
        parser.error("@INPUT@ requires --input")
    (output_dir / "stdin.bin").write_bytes(stdin_data)
    from qiling import Qiling
    from qiling.const import QL_INTERCEPT, QL_VERBOSE
    from qiling.extensions import pipe

    runtime = options.runtime.resolve()
    metadata = {
        "binary_sha256": digest(binary), "guest_argv": ["/bin/vcl"] + arguments,
        "input_sha256": digest(source) if source is not None else None,
        "stdin_sha256": digest(stdin_data), "time_scale": options.time_scale,
        "wall_timeout_seconds": options.timeout,
        "qiling_version": importlib.metadata.version("qiling"),
        "unicorn_version": importlib.metadata.version("unicorn"),
    }
    with tempfile.TemporaryDirectory(prefix="vcl-oracle-", dir=runtime) as directory:
        rootfs = pathlib.Path(directory) / "rootfs"
        shutil.copytree(runtime / "rootfs", rootfs, symlinks=True)
        (rootfs / "bin/vcl").write_bytes(binary)
        (rootfs / "bin/vcl").chmod(0o755)
        if source is not None:
            (rootfs / guest_input.lstrip("/")).write_bytes(source)
        stdout, stderr, stdin = pipe.SimpleOutStream(1), pipe.SimpleOutStream(2), pipe.SimpleInStream(0)
        stdin.write(stdin_data)
        ql = Qiling([str(rootfs / "bin/vcl")] + arguments, str(rootfs),
                    env={"PATH": "/bin:/usr/bin"}, verbose=QL_VERBOSE.DISABLED, console=False)
        ql.os.stdout, ql.os.stderr, ql.os.stdin = stdout, stderr, stdin
        if trace is not None:
            trace.install(ql)
        lock = threading.Lock()
        state = {"deadline": None, "armed": 0, "fired": 0, "unhandled": 0, "done": False, "exit_called": False}

        def record_exit(ql, code):
            state["exit_called"] = True

        ql.os.set_syscall("exit", record_exit, QL_INTERCEPT.ENTER)
        ql.os.set_syscall("exit_group", record_exit, QL_INTERCEPT.ENTER)

        def sys_alarm(ql, seconds):
            with lock:
                now = time.process_time()
                previous = state["deadline"]
                remaining = 0 if previous is None else max(1, math.ceil((previous - now) / options.time_scale))
                state["deadline"] = now + seconds * options.time_scale if seconds and options.time_scale > 0 else None
                state["armed"] += bool(seconds and options.time_scale > 0)
                return remaining

        def watch():
            while not state["done"]:
                time.sleep(0.01)
                with lock:
                    deadline = state["deadline"]
                    if deadline is not None and time.process_time() >= deadline:
                        state["deadline"] = None
                        if getattr(trace, "suppress_alarm", False):  # a run traced to completion
                            state["held"] = state.get("held", 0) + 1
                            continue
                        action = ql.os.sigaction_act[14] if len(ql.os.sigaction_act) > 14 else None
                        if action and action[0] == alarm_handler:
                            flag = ql.mem.read_ptr(alarm_flag_pointer, 4)
                            if flag:
                                ql.mem.write(flag, b"\x01")
                            state["fired"] += 1
                        else:
                            state["unhandled"] += 1

        ql.os.set_syscall("alarm", sys_alarm)
        watcher = threading.Thread(target=watch, daemon=True)
        watcher.start()
        start_wall, start_cpu = time.monotonic(), time.process_time()
        error = None
        try:
            ql.run(timeout=int(options.timeout * 1_000_000))
        except Exception:
            error = traceback.format_exc()
        finally:
            state["done"] = True
            watcher.join(timeout=1)
        stdout_data, stderr_data = stdout.read(), stderr.read()
        (output_dir / "stdout.bin").write_bytes(stdout_data)
        (output_dir / "stderr.bin").write_bytes(stderr_data)
        (output_dir / "emulator-error.txt").write_text(error or "")
        metadata.update({
            "exit_code": ql.os.exit_code if state["exit_called"] else None,
            "exit_status": ql.os.exit_code & 255 if state["exit_called"] else None,
            "guest_exited": state["exit_called"],
            "emulator_timeout": not state["exit_called"] and error is None,
            "wall_seconds": time.monotonic() - start_wall,
            "cpu_seconds": time.process_time() - start_cpu,
            "stdout_sha256": digest(stdout_data), "stderr_sha256": digest(stderr_data),
            "stdout_bytes": len(stdout_data), "stderr_bytes": len(stderr_data),
            "emulator_error": bool(error),
            "alarms": {key: state[key] for key in ("armed", "fired", "unhandled")},
        })
        # Include guest-created files for options that send output to a file.
        generated = output_dir / "generated"
        for path in (rootfs / "work").rglob("*"):
            if path.is_file() and (source is None or path != rootfs / guest_input.lstrip("/")):
                target = generated / path.relative_to(rootfs / "work")
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, target)
    (output_dir / "result.json").write_text(json.dumps(metadata, indent=2) + "\n")
    if trace is not None:
        trace.save(output_dir / "trace.json")
    print(json.dumps({"name": options.name, **metadata}))


if __name__ == "__main__":
    main()
