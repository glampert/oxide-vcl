#!/usr/bin/env python3
"""Replay every saved original invocation against the Rust executable.

This retains the original flags: only guest /work paths are relocated. It
reports acceptance, exact exit status, diagnostic bytes, normalized instruction
pairs (including E bits), generated label names, and stall metrics separately.
It does not turn formatting differences into claims of semantic failure.
"""
import argparse
import collections
import hashlib
import json
import pathlib
import re
import subprocess
import tempfile

from compare_outputs import compare, extract


HEADER_END = b"; -- end of corpus header --"


def corpus_input(path):
    """A corpus input without its provenance header (tests/corpus/README.md)."""
    data = path.read_bytes()
    if data.startswith(b"; oxide-vcl corpus program"):
        data = data[data.index(HEADER_END) + len(HEADER_END):]
        data = data[2:] if data.startswith(b"\r\n") else data[1:] if data.startswith(b"\n") else data
    return data


def canonical_pairs(assembly):
    names = {name: f"@generated_{index}" for index, (_, name) in enumerate(assembly.labels)
             if name.startswith("__v_")}
    def replace(text):
        for name in sorted(names, key=len, reverse=True):
            text = re.sub(r"(?<![\w])" + re.escape(name) + r"(?![\w])", names[name], text)
        return text
    return [tuple(replace(field) for field in pair) for pair in assembly.pairs]


def diagnostic_failure(assembly, stderr, status):
    """The original has internal failure paths with exit zero and no assembly."""
    return status == 0 and not assembly.pairs and b"ERROR:" in stderr


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=pathlib.Path)
    parser.add_argument("cases", nargs="*")
    parser.add_argument("--oracle", type=pathlib.Path, default=pathlib.Path("local/evidence/oracle"))
    parser.add_argument("--out", type=pathlib.Path, default=pathlib.Path("local/evidence/differential"))
    parser.add_argument("--timeout", type=float, default=15)
    options = parser.parse_args()
    candidate = options.candidate.resolve()
    candidate_sha256 = hashlib.sha256(candidate.read_bytes()).hexdigest()
    options.out.mkdir(parents=True, exist_ok=True)
    cases = options.cases or sorted(path.name for path in options.oracle.iterdir() if (path / "result.json").exists())
    report = []
    for name in cases:
        oracle = options.oracle / name
        metadata = json.loads((oracle / "result.json").read_text())
        if metadata.get("exit_code") is None or metadata.get("emulator_timeout"):
            report.append({"name": name, "classification": "blocker", "reason": "original guest did not exit"})
            continue
        record = {"name": name, "original_argv": metadata["guest_argv"],
                  "original_exit_status": metadata["exit_code"] & 255,
                  "original_binary_sha256": metadata["binary_sha256"],
                  "input_sha256": metadata["input_sha256"]}
        if metadata["emulator_error"]:
            record.update(classification="blocker", reason="original emulator error")
            report.append(record)
            continue
        destination = options.out / name
        destination.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="vcl-differential-") as directory:
            work = pathlib.Path(directory) / "work"
            work.mkdir()
            arguments = metadata["guest_argv"][1:]
            if (oracle / "input.vcl").exists():
                inputs = [argument for argument in arguments if argument.startswith("/work/")]
                if inputs:
                    (work / pathlib.Path(inputs[0]).name).write_bytes(corpus_input(oracle / "input.vcl"))
            args = [argument.replace("/work/", str(work) + "/") for argument in arguments]
            try:
                run = subprocess.run([str(candidate)] + args, cwd=directory, capture_output=True,
                                     input=(oracle / "stdin.bin").read_bytes(), timeout=options.timeout)
            except (subprocess.TimeoutExpired, OSError) as error:
                record.update(classification="blocker", reason=str(error))
                report.append(record)
                continue
            normalize = lambda data: data.replace((str(work) + "/").encode(), b"/work/")
            stdout, stderr = normalize(run.stdout), normalize(run.stderr)
            (destination / "stdout.bin").write_bytes(stdout)
            (destination / "stderr.bin").write_bytes(stderr)
            record.update(candidate_exit_status=run.returncode,
                          exit_status_equal=record["original_exit_status"] == run.returncode,
                          acceptance_equal=(record["original_exit_status"] == 0) == (run.returncode == 0),
                          stdout_bytes_equal=stdout == (oracle / "stdout.bin").read_bytes(),
                          stderr_bytes_equal=stderr == (oracle / "stderr.bin").read_bytes())
            original_assembly, candidate_assembly = (oracle / "stdout.bin").read_bytes(), stdout
            output_args = [argument[2:] for argument in arguments if argument.startswith("-o")]
            if output_args and output_args[-1].startswith("/work/"):
                filename = output_args[-1].removeprefix("/work/")
                expected_file, actual_file = oracle / "generated" / filename, work / filename
                if expected_file.exists():
                    original_assembly = expected_file.read_bytes()
                    candidate_assembly = actual_file.read_bytes() if actual_file.exists() else b""
            assembly = compare(original_assembly, candidate_assembly)
            left, right = extract(original_assembly), extract(candidate_assembly)
            original_diagnostic_failure = diagnostic_failure(left, (oracle / "stderr.bin").read_bytes(), record["original_exit_status"])
            candidate_diagnostic_failure = diagnostic_failure(right, stderr, run.returncode)
            record["original_diagnostic_failure"] = original_diagnostic_failure
            record["candidate_diagnostic_failure"] = candidate_diagnostic_failure
            record["acceptance_equal"] &= original_diagnostic_failure == candidate_diagnostic_failure
            record["assembly"] = assembly
            record["pairs_equal_ignoring_generated_names"] = (
                canonical_pairs(left) == canonical_pairs(right)) if assembly["assembly_comparable"] else None
            record["label_names_equal"] = left.labels == right.labels
            if not record["acceptance_equal"]:
                record["classification"] = "acceptance_mismatch"
            elif original_diagnostic_failure:
                record["classification"] = "diagnostic_failure"
            elif record["original_exit_status"] != 0:
                record["classification"] = "both_rejected"
            elif assembly["assembly_comparable"] and not record["pairs_equal_ignoring_generated_names"]:
                record["classification"] = "schedule_difference"
            elif assembly["assembly_comparable"]:
                record["classification"] = "pairs_match"
            else:
                record["classification"] = "non_assembly_success"
            (destination / "result.json").write_text(json.dumps(record, indent=2) + "\n")
            report.append(record)
        print(record["classification"], name)
    summary = dict(collections.Counter(record["classification"] for record in report))
    (options.out / "report.json").write_text(json.dumps({
        "candidate_sha256": candidate_sha256,
        "candidate_changed_during_run": candidate_sha256 != hashlib.sha256(candidate.read_bytes()).hexdigest(),
        "summary": summary, "cases": report}, indent=2) + "\n")
    (options.out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
