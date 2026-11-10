#!/usr/bin/env python3
"""Replay saved oracle invocations against a candidate executable.

This never executes the original and needs no emulator. It compares exit status,
stdout, stderr, and generated files independently. Host temporary paths are
replaced by the original guest paths; a default -u is inserted to preserve the
original anonymous-label seed when relocating the input. No whitespace,
instruction, diagnostic, or scheduling differences are discarded.

Example: python3 tools/compare_oracle.py target/debug/oxide-vcl sched-empty cli-H
"""
import argparse
import difflib
import json
import pathlib
import subprocess
import tempfile


HEADER_END = b"; -- end of corpus header --"


def corpus_input(path):
    """A corpus input without its provenance header (tests/corpus/README.md)."""
    data = path.read_bytes()
    if data.startswith(b"; oxide-vcl corpus program"):
        data = data[data.index(HEADER_END) + len(HEADER_END):]
        data = data[2:] if data.startswith(b"\r\n") else data[1:] if data.startswith(b"\n") else data
    return data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=pathlib.Path)
    parser.add_argument("cases", nargs="*")
    parser.add_argument("--oracle", type=pathlib.Path, default=pathlib.Path("local/evidence/oracle"))
    parser.add_argument("--out", type=pathlib.Path, default=pathlib.Path("local/analysis/differential"))
    parser.add_argument("--timeout", type=float, default=60)
    options = parser.parse_args()
    candidate = options.candidate.resolve()
    names = options.cases or sorted(path.name for path in options.oracle.iterdir() if (path / "result.json").exists())
    options.out.mkdir(parents=True, exist_ok=True)
    report = []
    for name in names:
        oracle = options.oracle / name
        metadata = json.loads((oracle / "result.json").read_text())
        if metadata["emulator_error"] or metadata.get("exit_code") is None or metadata.get("emulator_timeout"):
            report.append({"name": name, "skipped": "oracle emulator error or timeout"})
            continue
        with tempfile.TemporaryDirectory(prefix="vcl-candidate-") as directory:
            work = pathlib.Path(directory) / "work"
            work.mkdir()
            arguments = metadata["guest_argv"][1:]
            source_args = [argument for argument in arguments if argument.startswith("/work/")]
            # A valid fixture has one explicit source path, even when malformed
            # trailing options make the complete invocation fail.
            guest_input = source_args[0] if source_args and (oracle / "input.vcl").exists() else None
            if guest_input:
                source = work / pathlib.Path(guest_input).name
                source.write_bytes(corpus_input(oracle / "input.vcl"))
                if not any(argument.startswith("-u") for argument in arguments):
                    arguments = ["-u" + guest_input] + arguments
            rewritten = [argument.replace("/work/", str(work) + "/") for argument in arguments]
            # The anonymous-label seed must remain the guest path.
            if guest_input and not any(argument.startswith("-u") for argument in metadata["guest_argv"][1:]):
                rewritten[0] = "-u" + guest_input
            run_dir = options.out / name
            run_dir.mkdir(exist_ok=True)
            try:
                run = subprocess.run([str(candidate)] + rewritten,
                                     input=(oracle / "stdin.bin").read_bytes(),
                                     capture_output=True, timeout=options.timeout, cwd=directory)
            except subprocess.TimeoutExpired:
                report.append({"name": name, "timeout": True, "equal": False})
                continue
            normalize = lambda data: data.replace((str(work) + "/").encode(), b"/work/")
            actual = {"stdout": normalize(run.stdout), "stderr": normalize(run.stderr)}
            expected = {kind: (oracle / (kind + ".bin")).read_bytes() for kind in actual}
            results = {"name": name, "exit_expected": metadata["exit_code"] & 255,
                       "exit_actual": run.returncode, "streams": {}}
            for kind in actual:
                (run_dir / (kind + ".bin")).write_bytes(actual[kind])
                equal = actual[kind] == expected[kind]
                results["streams"][kind] = equal
                if not equal:
                    difference = difflib.unified_diff(expected[kind].decode(errors="replace").splitlines(True),
                                                      actual[kind].decode(errors="replace").splitlines(True),
                                                      fromfile="oracle/" + kind, tofile="candidate/" + kind)
                    (run_dir / (kind + ".diff")).write_text("".join(difference))
            files = {}
            expected_files = oracle / "generated"
            if expected_files.exists():
                for path in expected_files.rglob("*"):
                    if path.is_file():
                        relative = path.relative_to(expected_files)
                        actual_path = work / relative
                        files[str(relative)] = actual_path.exists() and normalize(actual_path.read_bytes()) == path.read_bytes()
            results["generated_files"] = files
            results["equal"] = (results["exit_expected"] == results["exit_actual"]
                                and all(results["streams"].values()) and all(files.values()))
            report.append(results)
        print(("PASS " if results["equal"] else "FAIL ") + name)
    (options.out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    compared = [result for result in report if "equal" in result]
    print(f"{sum(result['equal'] for result in compared)}/{len(compared)} cases match")
    raise SystemExit(0 if all(result["equal"] for result in compared) else 1)


if __name__ == "__main__":
    main()
