#!/usr/bin/env python3
"""Compare the Rust compiler with a corpus of original observations (default tests/corpus).

The comparison target is the original stdout with comments and whitespace
removed: every `;` comment is dropped, runs of whitespace collapse to one
space, and blank lines disappear. Exit status must also agree. Everything
else (labels, directives, instruction text, E bits and NOP rows) must match.
"""
import argparse
import difflib
import json
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent


HEADER_END = b"; -- end of corpus header --"


def corpus_input(path):
    """A corpus input without its provenance header (tests/corpus/README.md)."""
    data = path.read_bytes()
    if data.startswith(b"; oxide-vcl corpus program"):
        data = data[data.index(HEADER_END) + len(HEADER_END):]
        data = data[2:] if data.startswith(b"\r\n") else data[1:] if data.startswith(b"\n") else data
    return data


def normalize(data):
    text = data.decode(errors="replace") if isinstance(data, bytes) else data
    lines = []
    for line in text.splitlines():
        line = re.sub(r"\s+", " ", line.split(";", 1)[0]).strip()
        if line:
            lines.append(line)
    return lines


def run_candidate(candidate, case, timeout):
    metadata = json.loads((case / "result.json").read_text())
    with tempfile.TemporaryDirectory() as work:
        work = pathlib.Path(work)
        argv = [str(candidate)]
        for argument in metadata["guest_argv"][1:]:
            if argument.startswith("/work/"):
                name = argument[len("/work/"):]
                (work / name).write_bytes(corpus_input(case / "input.vcl"))
                argument = name
            argv.append(argument)
        try:
            process = subprocess.run(argv, cwd=work, capture_output=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            return None, b"", b"timeout"
        return process.returncode, process.stdout, process.stderr


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cases", nargs="*")
    parser.add_argument("--candidate", type=pathlib.Path, default=ROOT / "target/debug/oxide-vcl")
    parser.add_argument("--oracle", type=pathlib.Path, default=ROOT / "tests/corpus")
    parser.add_argument("--timeout", type=float, default=60)
    parser.add_argument("--diff", action="store_true", help="print a normalized diff per mismatch")
    parser.add_argument("--json", type=pathlib.Path)
    options = parser.parse_args()
    names = options.cases or sorted(path.name for path in options.oracle.iterdir()
                                    if (path / "result.json").exists())
    results = []
    for name in names:
        case = options.oracle / name
        metadata = json.loads((case / "result.json").read_text())
        # A native run of the original is the reference where emulation's
        # watchdog changed the result (`native-stdout.bin`, provenance in `native-stdout.json`).
        reference = case / "native-stdout.bin"
        if not reference.exists():
            reference = case / "stdout.bin"
        expected = normalize(reference.read_bytes())
        status, stdout, stderr = run_candidate(options.candidate.resolve(), case, options.timeout)
        actual = normalize(stdout)
        original_status = metadata["exit_code"] & 255
        same_status = status == original_status
        match = same_status and expected == actual
        first = next((index for index, (left, right) in enumerate(zip(expected, actual)) if left != right),
                     min(len(expected), len(actual)))
        ratio = difflib.SequenceMatcher(None, expected, actual, autojunk=False).ratio()
        results.append({"name": name, "match": match, "original_status": original_status,
                        "rust_status": status, "original_lines": len(expected),
                        "rust_lines": len(actual), "first_difference": None if match else first,
                        "similarity": round(ratio, 4)})
        flag = "MATCH" if match else "diff "
        print(f"{flag} {name:45} status {original_status}/{status} lines {len(expected)}/{len(actual)}"
              f" first {'-' if match else first} sim {ratio:.3f}")
        if options.diff and not match:
            for line in difflib.unified_diff(expected, actual, "original", "rust", lineterm="", n=2):
                print("    " + line)
    matched = sum(result["match"] for result in results)
    print(f"{matched}/{len(results)} normalized matches")
    if options.json:
        options.json.write_text(json.dumps(results, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
