#!/usr/bin/env python3
"""Survey VU listings for the cycles code generation leaves on the table.

Reads the stream tables of a listing (each starts with a `; _LNOPT_w=` or
`; _LPOPT_w=` comment, docs/OUTPUT_FORMAT.md) and counts, per table: rows,
annotated stall cycles (`STALL_LATENCY ?n`, `STALL_THRUPUT ?n`, ...), empty rows, copies
(`move`, and `max`/`mini` of a register with itself), empty branch delay
slots, empty slots from an E row to the table's end, and for a loop kernel
its resource bound (docs/OPTIMIZATION_PLAN.md, section 2).

Sources are listing files, corpus case directories (`stdout.bin`, or
`native-stdout.bin` where it exists) or corpus directories. Each `--side`
is one listing per case: `recorded` (the original's recorded output, the
default) or a mode for the candidate to compile with, optionally followed by
`:` and extra flags (`oxide`, `oxide:--opt=none`). With two sides the report
compares them, case by case and in total.

    tools/survey_output.py tests/corpus
    tools/survey_output.py tests/corpus --side vcl --side oxide --tables
    tools/survey_output.py --compare-files before.vsm after.vsm
"""
import argparse
from collections import Counter
from dataclasses import asdict, dataclass, field
import json
import pathlib
import re
import shlex
import subprocess
import sys
import tempfile

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from compare_outputs import normalize_instruction  # noqa: E402
from corpus_status import corpus_input  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parent.parent

HEADER = re.compile(r";\s*_(LN|LP)OPT_w=\[\s*(\S*)\s*\]\s*(-?\d+)\s*\[\s*(-?\d+)\s+(-?\d+)\s*\]"
                    r"\s*(-?\d+)\s*\[([^\]]*)\]")
# Kinds combine: STALL_LATENCY_THRUPUT.
STALL = re.compile(r"STALL(?:_(?:LATENCY|THRUPUT|RFWRITE))+\s*\??\s*(\d+)")
LOOP_PART = re.compile(r"__(?:PRO|EPI)\d*$|__(?:ENTRY|EXIT)_POINT$")
BRANCHES = ("b", "bal", "jr", "jalr")
# Unit occupancy per issue, from the VU User's Manual 6.0, section 7.4, which
# the port's instruction table matches.
FDIV = {"div": 7, "sqrt": 7, "rsqrt": 13}
EFU = {"eatan": 53, "eatanxy": 53, "eatanxz": 53, "eexp": 43, "eleng": 17, "ercpr": 11,
       "erleng": 23, "ersadd": 17, "ersqrt": 17, "esadd": 10, "esin": 28, "esqrt": 11,
       "esum": 11}


def mnemonic(instruction):
    """The bare opcode: no field mask, no modifier."""
    return instruction.split(" ", 1)[0].split("[", 1)[0].split(".", 1)[0]


def is_nop(instruction):
    return mnemonic(instruction) == "nop"


def is_branch(instruction):
    name = mnemonic(instruction)
    return name in BRANCHES or name.startswith("ib")


def has_end_bit(instruction):
    modifiers = re.search(r"\[([^\]]*)\]", instruction.split(" ", 1)[0])
    return bool(modifiers and "E" in modifiers[1].upper())


def is_copy(instruction):
    name = mnemonic(instruction)
    if name == "move":
        return True
    operands = instruction.split(" ", 1)[1].split(",") if " " in instruction else []
    return name in ("max", "mini") and len(operands) == 3 and operands[1] == operands[2]


def split_pair(original):
    """The upper and lower field of a listing row, or None for other lines."""
    line = original.split(";", 1)[0].rstrip()
    if not line.strip() or line.strip().endswith(":") or line.strip().startswith("."):
        return None
    if "|" in line:
        return tuple(line.split("|", 1))
    indentation = len(line) - len(line.lstrip())
    lower_column = indentation + 59
    if indentation >= 9 and len(line) > lower_column and line[lower_column] != " ":
        return line[:lower_column], line[lower_column:]
    compact = re.split(r"\s{2,}", line.strip())
    return tuple(compact) if len(compact) == 2 else None


@dataclass
class Table:
    name: str
    kind: str  # kernel, loop part, raw or linear
    strategy: str
    header: list
    pairs: list = field(default_factory=list)
    stalls: list = field(default_factory=list)

    def metrics(self):
        empty = [is_nop(upper) and is_nop(lower) for upper, lower in self.pairs]
        leading = next((index for index, value in enumerate(empty) if not value), len(empty))
        branch_rows = [index for index, (_, lower) in enumerate(self.pairs) if is_branch(lower)]
        delay_rows = [index + 1 for index in branch_rows if index + 1 < len(self.pairs)]
        end_rows = [index for index, (upper, _) in enumerate(self.pairs) if has_end_bit(upper)]
        exit_slots = 0
        if end_rows:
            exit_slots = sum(is_nop(upper) + is_nop(lower)
                             for upper, lower in self.pairs[end_rows[0]:])
        result = {
            "rows": len(self.pairs),
            "stall_cycles": sum(self.stalls),
            "cost": len(self.pairs) + sum(self.stalls),
            "empty_rows": sum(empty),
            "leading_empty_rows": leading if self.kind != "kernel" else 0,
            "copies": sum(is_copy(upper) + is_copy(lower) for upper, lower in self.pairs),
            "delay_slots": len(delay_rows),
            "empty_delay_slots": sum(empty[index] for index in delay_rows),
            "exit_tables": int(bool(end_rows)),
            "empty_exit_slots": exit_slots,
        }
        if self.kind == "kernel":
            upper = sum(not is_nop(upper) for upper, _ in self.pairs)
            lower = sum(not is_nop(lower) for _, lower in self.pairs)
            fdiv = sum(FDIV.get(mnemonic(lower), 0) for _, lower in self.pairs)
            efu = sum(EFU.get(mnemonic(lower), 0) for _, lower in self.pairs)
            result.update({"period": len(self.pairs), "upper_ops": upper, "lower_ops": lower,
                           "bound": max(upper, lower, fdiv, efu)})
        return result


def parse_listing(text):
    tables = []
    current = Table("(before the first table)", "linear", "", [])
    for line in text.splitlines():
        header = HEADER.search(line)
        if header:
            if current.pairs:
                tables.append(current)
            loop, strategy, printed, first, second, walked, name = header.groups()
            if loop == "LP":
                kind = "kernel"
            elif not strategy:
                kind = "loop part" if LOOP_PART.search(name) else "raw"
            else:
                kind = "linear"
            current = Table(name, kind, strategy,
                            [int(printed), int(first), int(second), int(walked)])
            continue
        fields = split_pair(line)
        if fields is None:
            continue
        try:
            pair = tuple(normalize_instruction(text) for text in fields)
        except ValueError:
            continue
        current.pairs.append(pair)
        stall = STALL.search(line)
        current.stalls.append(int(stall[1]) if stall else 0)
    if current.pairs:
        tables.append(current)
    return tables


KINDS = ("kernel", "loop part", "linear", "raw")
TOTALS = ("rows", "stall_cycles", "cost", "empty_rows", "leading_empty_rows", "copies",
          "delay_slots", "empty_delay_slots", "exit_tables", "empty_exit_slots")


def summarize(tables):
    by_kind = {kind: Counter() for kind in KINDS}
    for table in tables:
        metrics = table.metrics()
        by_kind[table.kind]["tables"] += 1
        for key in TOTALS:
            by_kind[table.kind][key] += metrics[key]
    total = sum(by_kind.values(), Counter())
    return by_kind, total


@dataclass
class Side:
    name: str
    mode: str | None
    flags: list

    @classmethod
    def parse(cls, text):
        if text == "recorded":
            return cls(text, None, [])
        mode, _, flags = text.partition(":")
        return cls(text, mode, shlex.split(flags))


def cases(paths):
    for path in paths:
        if path.is_file():
            yield path.stem, path
        elif (path / "result.json").exists():
            yield path.name, path
        else:
            for child in sorted(path.iterdir()):
                if (child / "result.json").exists():
                    yield child.name, child


def listing(case, side, candidate, timeout):
    if case.is_file():
        if side.mode is not None:
            raise SystemExit(f"{case}: a listing file has only its recorded side")
        return case.read_text(errors="replace")
    if side.mode is None:
        reference = case / "native-stdout.bin"
        if not reference.exists():
            reference = case / "stdout.bin"
        return reference.read_bytes().decode(errors="replace")
    metadata = json.loads((case / "result.json").read_text())
    with tempfile.TemporaryDirectory() as work:
        work = pathlib.Path(work)
        argv = [str(candidate), f"--mode={side.mode}", *side.flags]
        for argument in metadata["guest_argv"][1:]:
            if argument.startswith("/work/"):
                name = argument[len("/work/"):]
                (work / name).write_bytes(corpus_input(case / "input.vcl"))
                argument = name
            argv.append(argument)
        process = subprocess.run(argv, cwd=work, capture_output=True, timeout=timeout)
        return process.stdout.decode(errors="replace")


def print_summary(title, by_kind, total):
    print(title)
    print(f"  {'kind':10} {'tables':>6} {'rows':>7} {'stalls':>7} {'cost':>7} {'empty':>6}"
          f" {'lead':>5} {'copies':>6} {'delay':>9} {'exit':>9}")
    for kind, counts in [*by_kind.items(), ("total", total)]:
        if not counts["tables"]:
            continue
        print(f"  {kind:10} {counts['tables']:6} {counts['rows']:7} {counts['stall_cycles']:7}"
              f" {counts['cost']:7} {counts['empty_rows']:6} {counts['leading_empty_rows']:5}"
              f" {counts['copies']:6}"
              f" {str(counts['empty_delay_slots']) + '/' + str(counts['delay_slots']):>9}"
              f" {str(counts['empty_exit_slots']) + '/' + str(counts['exit_tables']):>9}")


def print_kernels(name, tables):
    for table in tables:
        if table.kind == "kernel":
            metrics = table.metrics()
            print(f"  {name:45} {table.name:45} period {metrics['period']:3}"
                  f" bound {metrics['bound']:3} (upper {metrics['upper_ops']},"
                  f" lower {metrics['lower_ops']})")


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("paths", nargs="*", type=pathlib.Path)
    parser.add_argument("--side", action="append", type=Side.parse,
                        help="recorded, or MODE[:FLAGS] for the candidate (repeatable)")
    parser.add_argument("--candidate", type=pathlib.Path,
                        default=ROOT / "target/release/oxide-vcl")
    parser.add_argument("--timeout", type=float, default=600)
    parser.add_argument("--compare-files", nargs=2, type=pathlib.Path, metavar=("A", "B"),
                        help="compare two listing files")
    parser.add_argument("--kernels", action="store_true", help="list each kernel's period and bound")
    parser.add_argument("--tables", action="store_true",
                        help="with two sides, list the tables whose cost differs")
    parser.add_argument("--json", type=pathlib.Path)
    options = parser.parse_args()

    if options.compare_files:
        sides = [Side(str(path), None, []) for path in options.compare_files]
        entries = [("files", {side.name: parse_listing(path.read_text(errors="replace"))
                              for side, path in zip(sides, options.compare_files)})]
    else:
        if not options.paths:
            parser.error("give listing files, case directories or corpus directories")
        sides = options.side or [Side.parse("recorded")]
        if len(sides) > 2:
            parser.error("at most two sides")
        entries = []
        for name, case in cases(options.paths):
            entries.append((name, {side.name: parse_listing(listing(case, side, options.candidate.resolve(),
                                                                    options.timeout))
                                   for side in sides}))

    report = {"sides": [side.name for side in sides], "cases": []}
    totals = {side.name: [] for side in sides}
    for name, by_side in entries:
        row = {"name": name}
        for side in sides:
            tables = by_side[side.name]
            totals[side.name].extend(tables)
            by_kind, total = summarize(tables)
            row[side.name] = {"total": dict(total), "by_kind": {k: dict(v) for k, v in by_kind.items()},
                              "tables": [{"name": t.name, "kind": t.kind, "strategy": t.strategy,
                                          "header": t.header, **t.metrics()} for t in tables]}
        report["cases"].append(row)

    for side in sides:
        by_kind, total = summarize(totals[side.name])
        print_summary(f"{side.name}: {len(entries)} listings", by_kind, total)
        if options.kernels:
            for name, by_side in entries:
                print_kernels(name, by_side[side.name])

    if len(sides) == 2:
        left, right = (side.name for side in sides)
        print(f"\ncost per listing, {left} -> {right} (rows + annotated stalls)")
        changed = 0
        for name, by_side in entries:
            before = sum(table.metrics()["cost"] for table in by_side[left])
            after = sum(table.metrics()["cost"] for table in by_side[right])
            if before == after and not options.tables:
                continue
            changed += before != after
            if before != after:
                print(f"  {name:55} {before:6} -> {after:6} ({after - before:+})")
            if options.tables:
                # Tables pair up by name and, for a repeated name, by occurrence.
                costs = {}
                for column, side in enumerate((left, right)):
                    seen = Counter()
                    for table in by_side[side]:
                        key = (table.name, seen[table.name])
                        seen[table.name] += 1
                        costs.setdefault(key, [None, None])[column] = table.metrics()["cost"]
                for (table, _), (a, b) in costs.items():
                    if a != b:
                        print(f"      {table:51} {a if a is not None else '-':>6} -> "
                              f"{b if b is not None else '-':>6}")
        before = sum(table.metrics()["cost"] for table in totals[left])
        after = sum(table.metrics()["cost"] for table in totals[right])
        print(f"  {changed} of {len(entries)} listings differ; total {before} -> {after}"
              f" ({after - before:+})")

    if options.json:
        options.json.write_text(json.dumps(report, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
