#!/usr/bin/env python3
"""Compare VU assembly while keeping byte, schedule, and timing findings separate.

Normalized schedule equality is a narrow observation, not a proof that two
different schedules are semantically equivalent or inequivalent. In particular,
end bits, branch operands, labels, pair ordering, and inserted NOPs are retained.
Only instruction case, register spelling, spacing, and integer notation change.
"""
import argparse
from dataclasses import asdict, dataclass, field
import json
import pathlib
import re


@dataclass
class Assembly:
    pairs: list = field(default_factory=list)
    labels: list = field(default_factory=list)
    globals: list = field(default_factory=list)
    stalls: list = field(default_factory=list)
    unparsed: list = field(default_factory=list)
    directives: list = field(default_factory=list)

    def metrics(self):
        nop = lambda instruction: instruction.split(" ")[0].split("[")[0] == "nop"
        end_bits = [index for index, pair in enumerate(self.pairs) if "[" in pair[0]
                    and "E" in pair[0].split("[", 1)[1].split("]", 1)[0]]
        return {"issue_pairs": len(self.pairs),
                "empty_pairs": sum(nop(upper) and nop(lower) for upper, lower in self.pairs),
                "upper_nops": sum(nop(pair[0]) for pair in self.pairs),
                "lower_nops": sum(nop(pair[1]) for pair in self.pairs),
                "annotated_stall_cycles": sum(count for _, count in self.stalls),
                "pairs_plus_annotated_stalls": len(self.pairs) + sum(count for _, count in self.stalls),
                "end_bit_pairs": end_bits}


def normalize_instruction(text):
    pieces = text.strip().split(None, 1)
    if not pieces:
        raise ValueError("empty instruction field")
    mnemonic = pieces[0].lower()
    mnemonic = re.sub(r"\[([^]]+)\]", lambda match: "[" + match[1].upper() + "]", mnemonic)
    operands = pieces[1] if len(pieces) == 2 else ""
    operands = re.sub(r"\b(v[fi])0*(\d+)([xyzw]{1,4})?\b", lambda match: match[1].upper() + f"{int(match[2]):02}" + (match[3] or "").lower(), operands, flags=re.I)
    if not re.match(r"^(?:b|bal|ib\w+|jr|jalr)(?:\[|$)", mnemonic):
        operands = re.sub(r"\b(acc|q|p|i|r)\b", lambda match: match[0].upper(), operands, flags=re.I)
    operands = re.sub(r"(?<![\w.])(?:0[xX][0-9a-fA-F]+|\d+)(?![\w.])", lambda match: str(int(match[0], 0 if match[0].lower().startswith("0x") else 10)), operands)
    operands = re.sub(r"\s+", "", operands)
    return mnemonic + (" " + operands if operands else "")


def extract(data):
    text = data.decode(errors="replace") if isinstance(data, bytes) else data
    result = Assembly()
    for original in text.splitlines():
        stall = re.search(r"STALL_LATENCY\s*\??\s*(\d+)", original)
        line = original.split(";", 1)[0].strip()
        if not line:
            continue
        if line.endswith(":"):
            result.labels.append((len(result.pairs), line[:-1]))
            continue
        if line.lower().startswith(".global"):
            result.globals.append(line.split(None, 1)[1].strip())
            continue
        if line.startswith(".") or line.split(None, 1)[0].lower() in ("dmaret", "vifnop", "mpg"):
            result.directives.append(" ".join(line.split()))
            continue
        fields = None
        if "|" in line:
            fields = line.split("|", 1)
        else:
            indentation = len(original) - len(original.lstrip())
            lower_column = indentation + 59
            if indentation >= 9 and len(original) > lower_column and original[lower_column:lower_column + 1] not in ("", " ", ";"):
                # Normally upper9/lower68; -c adds one leading space.
                fields = (original[:lower_column], original[lower_column:].split(";", 1)[0])
        if fields is None:
            compact = re.split(r"\s{2,}", line)
            if len(compact) == 2:
                fields = compact
        if fields:
            try:
                result.pairs.append(tuple(normalize_instruction(field) for field in fields))
                if stall:
                    result.stalls.append((len(result.pairs) - 1, int(stall[1])))
                continue
            except ValueError:
                pass
        result.unparsed.append(line)
    return result


def compare(expected, actual):
    left, right = extract(expected), extract(actual)
    comparable = bool(left.pairs or right.pairs) and not left.unparsed and not right.unparsed
    metrics_left, metrics_right = left.metrics(), right.metrics()
    return {
        "raw_bytes_equal": expected == actual,
        "assembly_comparable": comparable,
        "normalized_schedule_equal": ((left.pairs, left.labels, left.globals) ==
                                      (right.pairs, right.labels, right.globals)) if comparable else None,
        "stall_annotations_equal": left.stalls == right.stalls if comparable else None,
        "directives_equal": left.directives == right.directives,
        "metrics_equal": metrics_left == metrics_right if comparable else None,
        "expected_metrics": metrics_left, "actual_metrics": metrics_right,
        "expected": asdict(left), "actual": asdict(right),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("expected", type=pathlib.Path)
    parser.add_argument("actual", type=pathlib.Path)
    parser.add_argument("--json", type=pathlib.Path)
    options = parser.parse_args()
    result = compare(options.expected.read_bytes(), options.actual.read_bytes())
    output = json.dumps(result, indent=2) + "\n"
    if options.json:
        options.json.write_text(output)
    print(output, end="")


if __name__ == "__main__":
    main()
