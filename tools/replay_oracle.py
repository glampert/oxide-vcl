#!/usr/bin/env python3
"""Replay original-binary oracle cases from their saved argv/input records.

Run with the Qiling virtualenv's Python, from the repository root. --oracle can
also be tests/probes or tests/corpus. Output goes to a separate folder, so the
recorded runs are not touched. Every input and expected original binary
digest is checked by the runner before execution.
"""
import argparse
import json
import pathlib
import subprocess
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cases", nargs="*")
    parser.add_argument("--oracle", type=pathlib.Path, default=pathlib.Path("local/evidence/oracle"))
    parser.add_argument("--out", type=pathlib.Path, default=pathlib.Path("local/analysis/oracle-replay"))
    args = parser.parse_args()
    names = args.cases or sorted(path.name for path in args.oracle.iterdir() if (path / "result.json").exists())
    for name in names:
        case = args.oracle / name
        metadata = json.loads((case / "result.json").read_text())
        guest_args = metadata["guest_argv"][1:]
        command = [sys.executable, str(pathlib.Path(__file__).with_name("run_oracle.py")), "--name", name,
                   "--out", str(args.out), "--time-scale", str(metadata["time_scale"]),
                   "--timeout", str(metadata["wall_timeout_seconds"])]
        if (case / "stdin.bin").exists():
            command.extend(["--stdin", str(case / "stdin.bin")])
        with tempfile.TemporaryDirectory(prefix="vcl-replay-") as directory:
            if (case / "input.vcl").exists():
                guest_inputs = [argument for argument in guest_args if argument.startswith("/work/")]
                filename = pathlib.Path(guest_inputs[0]).name if guest_inputs else "input.vcl"
                source = pathlib.Path(directory) / filename
                source.write_bytes((case / "input.vcl").read_bytes())
                command.extend(["--input", str(source)])
            subprocess.run(command + ["--"] + guest_args, check=True)


if __name__ == "__main__":
    main()
