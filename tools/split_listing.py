#!/usr/bin/env python3
"""Split a VIF-stream VU listing into one program per entry point.

Input: a VIF-stream listing of disassembled VU programs:
`; VIF off: MPG n instructions -> VU address a` headers, then rows

    AAAA: LLLLLLLL UUUUUUUU  [E] upper-text        lower-text

with the instruction index, the lower word first and branch targets as
`L<index>`. Consecutive MPG blocks up to the next non-MPG VIF code (BASE,
STCYCL, ...) form one package: a micro-memory image.

In each image an entry point is an instruction nothing reaches: no
fall-through from the instruction before it, no direct branch, no `bal`
return. (MSCAL targets and routines reached through a table-driven `jr` are
exactly these.) Each entry's program is everything its control flow
reaches: fall-through, branch targets, `bal`/`jalr` returns and `jr`
targets given by constant code addresses, up to `[E]` and its delay slot.
Programs are written in the `.vsm` listing form (byte addresses, upper word
first, `L_xxxx` byte labels, `[E]` after the lower field), which
`tools/unschedule.py` reads. Identical programs are written once.

Usage: split_listing.py LISTING.vu OUTDIR --prefix NAME [--vu0-from VIF_OFFSET]
"""
import argparse
import hashlib
import json
import pathlib
import re
import sys

ROW = re.compile(r"^\s+([0-9a-f]{4}): ([0-9a-f]{8}) ([0-9a-f]{8})  (\[E\] )?(.*)$")
VIF = re.compile(r"^; VIF (0x[0-9a-f]+): (\w+)(.*)$")
MPG = re.compile(r"MPG (\d+) instructions -> VU address (0x[0-9a-f]+)")
CONDITIONAL = {"ibeq", "ibne", "ibgez", "ibgtz", "iblez", "ibltz"}


def parse(path):
    """Packages as lists of (vif offset, {index: row})."""
    packages, current, block = [], None, None
    column = None
    for line in path.read_text().splitlines():
        m = VIF.match(line)
        if m:
            offset, code = int(m.group(1), 16), m.group(2)
            if code == "MPG":
                if current is None:
                    current = {"vif": offset, "rows": {}}
                    packages.append(current)
                mm = MPG.search(line)
                block = (int(mm.group(2), 16) // 8, int(mm.group(1)))
            else:
                current = None
            continue
        m = ROW.match(line)
        if not m:
            continue
        index = int(m.group(1), 16)
        body = m.group(5)
        if column is None and body.startswith("nop "):
            column = re.match(r"nop\s+", body).end()
        if current is None:
            raise SystemExit(f"{path}: row {m.group(1)} outside an MPG block")
        if index in current["rows"]:
            raise SystemExit(f"{path}: {m.group(1)} written twice in one package")
        current["rows"][index] = {"lower_word": m.group(2), "upper_word": m.group(3),
                                  "end": bool(m.group(4)), "body": body}
    if column is None:
        raise SystemExit(f"{path}: no nop row to find the lower column")
    for package in packages:
        for row in package["rows"].values():
            body = row.pop("body")
            row["upper"] = body[:column].strip()
            row["lower"] = re.sub(r"\s+", " ", body[column:].strip())
    return packages


def wrap(rows, unit):
    """Branch offsets wrap around micro memory (the disassembler prints the raw sum)."""
    size = 0x200 if unit == "VU0" else 0x800
    for row in rows.values():
        row["lower"] = re.sub(r"\bL([0-9a-f]{4})$", lambda m: f"L{int(m.group(1), 16) % size:04x}",
                              row["lower"])


def target(lower):
    m = re.search(r"\bL([0-9a-f]{4})$", lower)
    return int(m.group(1), 16) if m else None


def mnemonic(lower):
    return lower.split(" ")[0].split(".")[0] if lower else ""


def flow(rows):
    """Successors per instruction (delay slots modelled explicitly)."""
    constants = {}
    for row in rows.values():
        m = re.match(r"iaddiu (vi\d\d), vi00, (0x[0-9a-f]+|\d+)$", row["lower"])
        if m and mnemonic(row["lower"]) != "loi":
            constants.setdefault(m.group(1), set()).add(int(m.group(2), 0))
    successors = {a: set() for a in rows}
    incoming = {a: set() for a in rows}

    def edge(a, b):
        if b in rows:
            successors[a].add(b)
            incoming[b].add(a)

    for a, row in rows.items():
        op = mnemonic(row["lower"])
        delay = a + 1
        if row["end"]:
            edge(a, delay)  # the delay slot runs, then the program ends
            successors.setdefault(delay, set())
            row["_end_slot"] = delay
            continue
        if op in CONDITIONAL or op in ("b", "bal", "jr", "jalr"):
            edge(a, delay)
            row["_branch"] = (op, delay)
            continue
        edge(a, a + 1)
    # Delay slots: replace their fall-through by the branch's destinations.
    for a, row in rows.items():
        if "_end_slot" in row:
            slot = row.pop("_end_slot")
            if slot in rows:
                for b in list(successors[slot]):
                    incoming[b].discard(slot)
                successors[slot] = set()
        if "_branch" not in row:
            continue
        op, slot = row.pop("_branch")
        if slot not in rows:
            continue
        for b in list(successors[slot]):
            incoming[b].discard(slot)
        successors[slot] = set()
        dest = target(row["lower"])
        if op in CONDITIONAL:
            edge(slot, a + 2)
            edge(slot, dest)
        elif op == "b":
            edge(slot, dest)
        elif op == "bal":
            edge(slot, dest)
            edge(slot, a + 2)  # the return lands here
        else:
            register = re.search(r"(vi\d\d)$", row["lower"]).group(1)
            for t in constants.get(register, ()):
                edge(slot, t)
            if op == "jalr":
                edge(slot, a + 2)
    return successors, incoming


def programs(rows):
    successors, incoming = flow(rows)
    entries = sorted(a for a in rows if not incoming[a])
    for entry in entries:
        seen, stack = set(), [entry]
        while stack:
            a = stack.pop()
            if a in seen:
                continue
            seen.add(a)
            stack.extend(successors[a])
        if not any(rows[a]["end"] for a in seen):
            yield entry, None  # never ends: padding or dead code
            continue
        yield entry, sorted(seen)


def vsm(name, unit, entry, addresses, rows, source):
    targets = set()
    for a in addresses:
        t = target(rows[a]["lower"])
        if t is not None:
            targets.add(t)
    lines = [f"; microprogram {name}  entry=0x{entry * 8:04x} ({len(addresses)} instr) guess={unit}",
             f";   from {source}"]
    for a in addresses:
        row = rows[a]
        if a in targets:
            lines.append(f"L_{a * 8:04x}:")
        lower = re.sub(r"\bL([0-9a-f]{4})$", lambda m: f"L_{int(m.group(1), 16) * 8:04x}", row["lower"])
        flag = " [E]" if row["end"] else ""
        lines.append(f"  {a * 8:04x}: {row['upper_word']} {row['lower_word']}  "
                     f"{row['upper']:<45}{lower}{flag}")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("listing", type=pathlib.Path)
    parser.add_argument("outdir", type=pathlib.Path)
    parser.add_argument("--prefix", required=True)
    parser.add_argument("--vu0-from", type=lambda s: int(s, 0), default=None,
                        help="packages starting at or after this VIF offset are VU0 (4 KB micro memory)")
    options = parser.parse_args()
    options.outdir.mkdir(parents=True, exist_ok=True)
    seen = {}
    index = []
    for number, package in enumerate(parse(options.listing)):
        unit = "VU0" if options.vu0_from is not None and package["vif"] >= options.vu0_from else "VU1"
        rows = package["rows"]
        wrap(rows, unit)
        for entry, addresses in programs(rows):
            if addresses is None:
                index.append({"package": number, "entry": entry * 8, "skipped": "no end bit reached"})
                continue
            name = f"{options.prefix}_p{number}_{entry * 8:04x}"
            body = vsm("X", unit, entry, addresses, rows, options.listing.name).split("\n", 1)[1]
            digest = hashlib.sha256(body.encode()).hexdigest()
            if digest in seen:
                index.append({"package": number, "entry": entry * 8, "duplicate_of": seen[digest]})
                continue
            seen[digest] = name
            (options.outdir / f"{name}.vsm").write_text(vsm(name, unit, entry, addresses, rows,
                                                             f"{options.listing.name} package {number}"))
            index.append({"package": number, "entry": entry * 8, "name": name, "unit": unit,
                          "instructions": len(addresses)})
    (options.outdir / f"{options.prefix}_index.json").write_text(json.dumps(index, indent=1) + "\n")
    written = [e for e in index if "name" in e]
    print(f"{options.listing.name}: {len(written)} programs, "
          f"{sum('duplicate_of' in e for e in index)} duplicates, "
          f"{sum('skipped' in e for e in index)} without an end", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
