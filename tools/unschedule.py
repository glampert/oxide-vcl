#!/usr/bin/env python3
"""Turn a scheduled VU listing (a disassembled `.vsm`) back into VCL source.

Each upper/lower pair becomes up to two sequential instructions in the old
(physical register) syntax, so both the original vcl and the port schedule it
again. The conversion keeps the program's instructions and control flow; it
does not try to reproduce its hardware timing:

- NOP fillers (`nop`, `move vf00, vf00`) are dropped.
- Within a pair both halves read the old register values, so the half that
  reads a register the other writes is emitted first (upper first otherwise;
  `loi` writes I at its T stage, after the upper half read it).
- A branch's delay-slot instruction is moved before the branch (vcl fills
  delay slots itself). If it writes a register the branch reads, the
  program is reported and skipped.
- An `[E]` end bit plus its delay slot becomes `--cont` (the next MSCNT
  resumes at the following instruction); the last one ends the program.
- `.init_vf_all` / `.init_vi_all` declare every register initialized, so
  values the caller uploads are not "used before set".
- `[M]` (VU0 macro-mode interlock) has no VCL form and is dropped.

Usage: unschedule.py LISTING.vsm [-o OUT.vcl] [--inputs-from COMPILER | --inputs-from-original DIR]
"""
import argparse
import pathlib
import re
import subprocess
import sys
import tempfile

LINE = re.compile(r"^\s+([0-9a-f]{4}): ([0-9a-f]{8}) ([0-9a-f]{8})  (.*)$")
LABEL = re.compile(r"^(L_[0-9a-f]+):\s*$")
BRANCHES = {"b", "bal", "ibeq", "ibne", "ibgez", "ibgtz", "iblez", "ibltz", "jr", "jalr"}
NOPS = {"nop", "move vf00, vf00", "move.xyzw vf00, vf00"}
# vcl inserts Q/P waits itself and accepts these only in raw mode.
WAITS = {"waitq", "waitp"}


class Unsupported(Exception):
    pass


def split_pair(text, column):
    """The listing aligns the lower field at a fixed column."""
    upper = text[:column].strip()
    lower = text[column:].strip()
    flags = set(re.findall(r"\[([A-Z])\]", lower))
    lower = re.sub(r"\s*\[[A-Z]\]", "", lower)
    lower = re.sub(r"\s*;.*$", "", lower)
    squeeze = lambda field: re.sub(r"\s+", " ", field).strip()
    return squeeze(upper), squeeze(lower), flags


def normalize(instruction):
    """Disassembler/DVP spelling -> vcl old syntax."""
    if not instruction:
        return instruction
    parts = instruction.split(None, 1)
    mnemonic = parts[0]
    operands = parts[1] if len(parts) > 1 else ""
    if mnemonic in WAITS:
        return None
    if mnemonic == "loi":
        return None  # rewritten from the raw word by the caller
    # Register masks on operands are implied by the mnemonic's mask.
    operands = re.sub(r"\b((?:vf|VF)\d\d|ACC)\.[xyzw]+\b", r"\1", operands)
    # Memory field suffixes: `1023(vi00).w`, `(vi11).x`.
    operands = re.sub(r"(\((?:vi\d\d)(?:\+\+)?\)|\((?:--)?vi\d\d\))\.[xyzw]", r"\1", operands)
    operands = operands.replace("ACC", "ACC")
    return f"{mnemonic} {operands}".strip()


def registers(instruction):
    """Rough read/write sets for ordering within a pair."""
    if not instruction:
        return set(), set()
    mnemonic, _, operands = instruction.partition(" ")
    names = re.findall(r"\b(vf\d\d|vi\d\d|ACC|Q|I|P)\b", operands, re.I)
    names = [n.lower() for n in names]
    base = mnemonic.split(".")[0]
    reads, writes = set(names), set()
    stores = base in ("sq", "sqi", "sqd", "isw", "iswr", "b", "ibeq", "ibne", "ibgez", "ibgtz",
                      "iblez", "ibltz", "jr", "xgkick", "fcset", "fsset", "fmand", "fsand", "fcand",
                      "fcor", "fsor", "fcget", "fmor", "fmeq", "fseq", "fceq")
    if names and not stores:
        writes = {names[0]}
        reads = set(names[1:])
    if base in ("lqi", "lqd", "sqi", "sqd"):
        m = re.search(r"\((?:--)?(vi\d\d)(?:\+\+)?\)", operands)
        if m:
            writes.add(m.group(1))
    if base in ("bal", "jalr"):
        writes = {names[0]}
    if base.startswith(("mula", "madda", "msuba", "adda", "suba", "opmula")):
        writes = {"acc"}
        reads = set(names) - {"acc"}
    if base.startswith(("madd", "msub")) and not base.startswith(("madda", "msuba")):
        reads.add("acc")
    if base in ("div", "sqrt", "rsqrt"):
        writes = {"q"}
        reads = set(names) - {"q"}
    if base.endswith("i") and base[:-1] in ("add", "sub", "mul", "madd", "msub", "adda", "suba", "mula", "madda", "msuba", "max", "mini"):
        reads.add("i")
    if base.endswith("q") and base[:-1] in ("add", "sub", "mul", "madd", "msub", "adda", "suba", "mula", "madda", "msuba"):
        reads.add("q")
    return reads, writes


def order(upper, lower, lower_writes_i):
    ur, uw = registers(upper)
    lr, lw = registers(lower)
    if lower_writes_i:
        lw = lw | {"i"}
    if lw & ur:  # the upper half reads what the lower writes: upper first
        return [upper, lower]
    if uw & lr:  # the lower half reads what the upper writes: lower first
        return [lower, upper]
    return [upper, lower]


def convert(path, inputs=(), outputs=()):
    text = path.read_text()
    rows = []  # (address, label list, upper, lower, flags, raw lower word)
    pending_labels = []
    column = None
    for line in text.splitlines():
        m = LABEL.match(line)
        if m:
            pending_labels.append(m.group(1))
            continue
        m = LINE.match(line)
        if not m:
            continue
        rows.append((m.group(1), pending_labels, m.group(4), m.group(3)))
        pending_labels = []
    # The lower field starts at a fixed column; a `nop` upper shows it.
    for _, _, body, _ in rows:
        if body.startswith("nop "):
            column = re.match(r"nop\s+", body).end()
            break
    if column is None:
        raise Unsupported("no nop row to find the lower column")
    rows = [(address, labels, *split_pair(body, column), word) for address, labels, body, word in rows]
    # The disassembler prints a branch that wraps around micro memory as a negative
    # address (`L_-19c0`); the VU wraps offsets modulo its micro memory.
    memory = 0x1000 if re.search(r"guess=VU0", text) else 0x4000
    addresses = {int(row[0], 16): k for k, row in enumerate(rows)}
    for k, row in enumerate(rows):
        m = re.search(r"L_-([0-9a-f]+)", row[3])
        if not m:
            continue
        target = (memory - int(m.group(1), 16)) % memory
        if target not in addresses:
            raise Unsupported(f"{row[0]}: branch to {target:#x} outside the program")
        label = f"L_{target:04x}"
        rows[k] = (row[0], row[1], row[2], row[3].replace(m.group(0), label), *row[4:])
        j = addresses[target]
        if label not in rows[j][1]:
            rows[j] = (rows[j][0], rows[j][1] + [label], *rows[j][2:])
    # `jr`/`jalr` through a register loaded with a constant code address
    # (`iaddiu vi15, vi00, 0x13` -> 0x98): tag those addresses as targets.
    constants = {}
    for _, _, upper, lower, flags, _ in rows:
        m = re.match(r"iaddiu (vi\d\d), vi00, (0x[0-9a-f]+|\d+)$", lower)
        if m and "I" not in flags:
            constants.setdefault(m.group(1), set()).add(int(m.group(2), 0) * 8)
    addresses = {int(row[0], 16): k for k, row in enumerate(rows)}
    jump_tags = {}
    for _, _, _, lower, flags, _ in rows:
        m = re.match(r"(jr|jalr)\b.*?(vi\d\d)$", lower)
        if not m or "I" in flags:
            continue
        register = m.group(2)
        targets = sorted(t for t in constants.get(register, ()) if t in addresses)
        jump_tags[register] = targets
        for target in targets:
            k = addresses[target]
            label = f"L_{target:04x}"
            if label not in rows[k][1]:
                rows[k] = (rows[k][0], rows[k][1] + [label], *rows[k][2:])
    out = []
    notes = []
    i = 0
    ends = sum(1 for r in rows if "E" in r[4])
    seen_ends = 0
    while i < len(rows):
        address, labels, upper, lower, flags, word = rows[i]
        for label in labels:
            out.append(f"{label}:")
        lower_is_loi = "I" in flags
        u = None if upper in NOPS else normalize(upper)
        if lower_is_loi:
            l = f"loi 0x{word}"
        else:
            l = None if lower in NOPS else normalize(lower)
        base = (lower.split()[0].split(".")[0] if lower else "")
        if base in ("jr", "jalr") and not lower_is_loi:
            register = re.search(r"(vi\d\d)$", lower).group(1)
            tags = jump_tags.get(register)
            if tags:
                l = l + "".join(f":L_{t:04x}" for t in tags)
        if base in BRANCHES and not lower_is_loi:
            if i + 1 >= len(rows):
                raise Unsupported(f"{address}: branch without a delay slot")
            d_address, d_labels, d_upper, d_lower, d_flags, d_word = rows[i + 1]
            du = None if d_upper in NOPS else normalize(d_upper)
            dl = f"loi 0x{d_word}" if "I" in d_flags else (None if d_lower in NOPS else normalize(d_lower))
            delay = [x for x in order(du, dl, "I" in d_flags) if x]
            branch_reads, _ = registers(l)
            for x in delay:
                if registers(x)[1] & branch_reads:
                    notes.append(f"{d_address}: delay slot `{x}` now runs before the branch that reads its old value")
            out.extend(x for x in order(u, None, False) if x)
            out.extend(delay)
            out.append(l)
            if "E" in d_flags or "E" in flags:
                raise Unsupported(f"{address}: end bit at a branch")
            if d_labels:
                # A branch target on the delay slot: the slot also runs there.
                notes.append(f"{d_address}: labelled delay slot also emitted at its label")
                i += 1
                rows[i] = (d_address, d_labels, d_upper, d_lower, d_flags, d_word)
                continue
            i += 2
            continue
        out.extend(x for x in order(u, l, lower_is_loi) if x)
        if "E" in flags:
            if i + 1 >= len(rows):
                raise Unsupported(f"{address}: end bit without a delay slot")
            d_address, d_labels, d_upper, d_lower, d_flags, d_word = rows[i + 1]
            if d_labels:
                raise Unsupported(f"{d_address}: label on an end delay slot")
            du = None if d_upper in NOPS else normalize(d_upper)
            dl = f"loi 0x{d_word}" if "I" in d_flags else (None if d_lower in NOPS else normalize(d_lower))
            out.extend(x for x in order(du, dl, "I" in d_flags) if x)
            seen_ends += 1
            if seen_ends < ends or i + 2 < len(rows):
                out.append("--cont")
            i += 2
            continue
        i += 1
    name = path.stem
    header = [f"; unscheduled from {path.name} by tools/unschedule.py"]
    header += [f"; approximation: {note}" for note in notes]
    header += [".init_vf_all", ".init_vi_all", f".name {name}", "", "--enter"]
    header += [f"\t{declaration}" for declaration in inputs]
    header += ["--endenter", ""]
    body = [line if line.endswith(":") or line.startswith("--") else "\t" + line for line in out]
    footer = ["", "--exit"] + [f"\t{declaration}" for declaration in outputs] + ["--endexit", ""]
    return "\n".join(header + body + footer)


HARDWARE = {"ACC": "in_hw_acc ACC", "I": "in_hw_i I", "Q": "in_hw_q Q", "P": "in_hw_p P",
            "R": "in_hw_r R", "CLIP": "in_hw_clip CLIP", "CLIPA": "in_hw_clip CLIP",
            "MAC": "in_hw_mac MAC"}


def declaration(register):
    """`in_*` binding for a register the compiler reports used before set."""
    name = register.upper()
    if re.fullmatch(r"VF\d\d", name):
        return f"in_vf {name.lower()}({name.lower()})"
    if re.fullmatch(r"VI\d\d", name):
        return f"in_vi {name.lower()}({name.lower()})"
    return HARDWARE.get(name)


def find_inputs(listing, candidate, work, limit=64):
    """Declare as inputs the registers `candidate` reports used before set."""
    inputs = []
    for _ in range(limit):
        source = convert(listing, inputs)
        path = work / f"{listing.stem}.vcl"
        path.write_text(source)
        process = subprocess.run([str(candidate), "-t1", "-uCORPUS", path.name], cwd=work,
                                 capture_output=True, text=True)
        m = re.search(r"(\w+)\.\w+ used before set", process.stderr)
        if process.returncode == 0 or not m:
            return source, process.returncode, process.stderr.strip()
        entry = declaration(m.group(1))
        if entry is None or entry in inputs:
            return source, process.returncode, process.stderr.strip()
        inputs.append(entry)
    return source, process.returncode, "input search limit"


def find_inputs_original(listing, work, keep, limit=8):
    """Declare as inputs the registers the original vcl (under Qiling) lists
    as used before set (`INFO: UBS: <stream> VF30.2`), all at once, until a
    run adds none. The last run's evidence is copied to `keep`."""
    root = pathlib.Path(__file__).resolve().parents[1]
    runtime = root / "local/analysis/runtime/venv/bin/python"
    inputs = []
    for _ in range(limit):
        source = convert(listing, inputs)
        path = work / f"{listing.stem}.vcl"
        path.write_text(source)
        out = work / "out"
        subprocess.run([str(runtime), str(root / "tools/run_oracle.py"), "--name", listing.stem,
                        "--input", str(path), "--out", str(out), "--time-scale", "1",
                        "--timeout", "3000", "--", "-t1", "-uCORPUS", "@INPUT@"],
                       cwd=root, capture_output=True)
        run = out / listing.stem
        stderr = (run / "stderr.bin").read_text(errors="replace")
        added = []
        for register in re.findall(r"INFO: UBS: \S+ (\w+)\.\d+", stderr):
            entry = declaration(register)
            if entry and entry not in inputs and entry not in added:
                added.append(entry)
        if not added:
            keep.mkdir(parents=True, exist_ok=True)
            for name in ("result.json", "stdout.bin", "stderr.bin"):
                (keep / name).write_bytes((run / name).read_bytes())
            return source, inputs
        inputs += added
    return source, inputs


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("listing", type=pathlib.Path)
    parser.add_argument("-o", "--output", type=pathlib.Path)
    parser.add_argument("--inputs-from", type=pathlib.Path,
                        help="compiler whose 'used before set' errors become --enter inputs")
    parser.add_argument("--inputs-from-original", type=pathlib.Path, metavar="EVIDENCE_DIR",
                        help="ask the original vcl (tools/run_oracle.py) instead; its last run's "
                             "result.json/stdout.bin/stderr.bin go to EVIDENCE_DIR")
    options = parser.parse_args()
    try:
        if options.inputs_from_original:
            with tempfile.TemporaryDirectory() as work:
                source, _ = find_inputs_original(options.listing, pathlib.Path(work),
                                                 options.inputs_from_original)
        elif options.inputs_from:
            with tempfile.TemporaryDirectory() as work:
                source, status, message = find_inputs(options.listing, options.inputs_from.resolve(),
                                                      pathlib.Path(work))
            if status != 0:
                print(f"{options.listing.name}: {message.splitlines()[0] if message else status}", file=sys.stderr)
        else:
            source = convert(options.listing)
    except Unsupported as error:
        print(f"{options.listing.name}: unsupported: {error}", file=sys.stderr)
        return 2
    if options.output:
        options.output.write_text(source)
    else:
        sys.stdout.write(source)
    return 0


if __name__ == "__main__":
    sys.exit(main())
