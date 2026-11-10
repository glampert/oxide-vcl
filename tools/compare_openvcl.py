#!/usr/bin/env python3
"""Compare oxide-vcl's code generation with openvcl's on public corpus programs.

Each program is compiled by both compilers with its recorded arguments, and
both listings are measured with openvcl's static cost analyzer
(`openvcl --cost`), so the two are scored by the same model. For every
program OUT/<case>/ receives:

- input.vcl: the program without its corpus header;
- oxide.vsm, openvcl.vsm (and .log): the two compilers' output;
- oxide-vs-openvcl.diff: a diff of the listings with comments and whitespace
  removed; oxide-vs-openvcl.full.diff: the raw diff;
- oxide.cost.txt, openvcl.cost.txt, cost-compare.md: the analyzer's reports.

OUT/summary.md collects whole-program totals, the cost of each labelled
block, and the steady-state loops of software-pipelined code. The estimate is
a static model, not a measurement: see docs/OPENVCL_COMPARISON.md.

Example:
    python3 tools/compare_openvcl.py --openvcl /usr/local/ps2dev --out /tmp/cmp
"""
import argparse
import difflib
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
HEADER_END = b"; -- end of corpus header --"

# The programs in docs/OPENVCL_COMPARISON.md.
DEFAULT_CASES = [
    "quake2-ps2__src_ps2_renderer_vu1progs_particles",
    "quake2-ps2__src_ps2_renderer_vu1progs_textured_triangles",
    "doom3-bfg-ps2__src_ps2_renderer_vu1progs_passthrough",
    "doom3-bfg-ps2__src_ps2_renderer_vu1progs_world_lm",
    "doom3-bfg-ps2__src_ps2_renderer_vu1progs_world_strips",
    "ps2gl__examples_tricked_out_billboard_renderer",
    "ps2gl__vu1_scei",
    "tyra__engine_src_renderer_3d_pipeline_dynamic_core_programs_dynpip_td_vu1",
    "tyra__engine_src_renderer_3d_pipeline_static_core_programs_cull_stapip_cull_tc_vu1",
    "openvcl__test_fixtures_general",
    "openvcl__examples_triangle_strip_vu1TriangleStrip",
]

TOTALS = {
    "estimated": "estimated_total_cycles",
    "static": "static_cycles",
    "paired": "paired_cycles",
    "nop_only": "nop_only_cycles",
    "wait_stall": "wait_stall_cycles",
}


def corpus_input(path):
    """A corpus input without its provenance header (tests/corpus/README.md)."""
    data = path.read_bytes()
    if data.startswith(b"; oxide-vcl corpus program"):
        data = data[data.index(HEADER_END) + len(HEADER_END):]
        data = data[2:] if data.startswith(b"\r\n") else data[1:] if data.startswith(b"\n") else data
    return data


def normalize(text):
    lines = []
    for line in text.splitlines():
        line = re.sub(r"\s+", " ", line.split(";", 1)[0]).strip()
        if line:
            lines.append(line)
    return lines


def openvcl_binary(path):
    """The openvcl executable: PATH itself, or PATH/bin/openvcl for an install prefix."""
    path = path.expanduser()
    if path.is_dir():
        path = path / "bin" / "openvcl"
    if not path.is_file():
        sys.exit(f"openvcl not found: {path}")
    return path


def flags(case):
    """The recorded arguments without the program name and the input file."""
    argv = json.loads((case / "result.json").read_text())["guest_argv"]
    return [argument for argument in argv[1:] if not argument.startswith("/work/")]


def parse_cost(text):
    """Totals and per-block estimated cycles from an `openvcl --cost` report."""
    totals, blocks, in_blocks = {}, {}, False
    for line in text.splitlines():
        if not line.startswith(" "):
            in_blocks = line.startswith("blocks:")
            key, _, value = line.partition(":")
            if not in_blocks and value.strip().lstrip("-").isdigit():
                totals[key] = int(value)
            continue
        if in_blocks:
            label, _, fields = line.strip().partition(": ")
            values = dict(field.split("=", 1) for field in fields.split() if "=" in field)
            if "estimated_cycles" in values:
                blocks[label] = int(values["estimated_cycles"])
    return totals, blocks


def short_name(name):
    project = name.split("__", 1)[0].split("-", 1)[0]
    rest = name.split("__", 1)[1]
    for marker in ("vu1progs_", "_core_programs_"):
        if marker in rest:
            rest = rest.split(marker, 1)[1]
    return f"{project}-{rest}"


def compare(case_name, options, openvcl):
    case = options.corpus / case_name
    work = options.out / short_name(case_name)
    work.mkdir(parents=True, exist_ok=True)
    (work / "input.vcl").write_bytes(corpus_input(case / "input.vcl"))
    arguments = flags(case)
    status = {}
    for tool, binary in (("oxide", options.candidate), ("openvcl", openvcl)):
        process = subprocess.run([str(binary), *arguments, f"-o{tool}.vsm", "input.vcl"],
                                 cwd=work, capture_output=True, timeout=options.timeout)
        (work / f"{tool}.log").write_bytes(process.stdout + process.stderr)
        status[tool] = process.returncode
    listings = {tool: (work / f"{tool}.vsm").read_text(errors="replace")
                if (work / f"{tool}.vsm").exists() else "" for tool in status}
    reference = case / "native-stdout.bin"
    if not reference.exists():
        reference = case / "stdout.bin"
    matches_reference = normalize(listings["oxide"]) == normalize(reference.read_text(errors="replace"))
    for suffix, a, b in (("diff", normalize(listings["oxide"]), normalize(listings["openvcl"])),
                         ("full.diff", listings["oxide"].splitlines(), listings["openvcl"].splitlines())):
        diff = difflib.unified_diff(a, b, "oxide.vsm", "openvcl.vsm", lineterm="")
        (work / f"oxide-vs-openvcl.{suffix}").write_text("\n".join(diff) + "\n")
    costs = {}
    for tool in status:
        report = subprocess.run([str(openvcl), "--cost", f"{tool}.vsm"], cwd=work,
                                capture_output=True, text=True).stdout
        (work / f"{tool}.cost.txt").write_text(report)
        costs[tool] = parse_cost(report)
    compare_report = subprocess.run([str(openvcl), "--cost-compare-markdown", "oxide.vsm", "openvcl.vsm"],
                                    cwd=work, capture_output=True, text=True).stdout
    (work / "cost-compare.md").write_text(compare_report)
    return {"name": work.name, "status": status, "matches_reference": matches_reference,
            "costs": costs, "unit": next((a[2:] for a in arguments if a.startswith("-u")), None),
            "aliases": label_aliases((work / "input.vcl").read_text(errors="replace"))}


def label_aliases(source):
    """Source labels grouped by address: labels with no instruction between them."""
    aliases, group = {}, []
    for line in source.splitlines():
        line = line.split(";", 1)[0].strip()
        if not line:
            continue
        match = re.fullmatch(r"([A-Za-z_.$][\w.$]*):", line)
        if match:
            group.append(match.group(1))
            for label in group:
                aliases[label] = list(group)
        elif not line.startswith("--"):
            group = []
    return aliases


def loop_rows(result):
    """Steady-state loops: oxide-vcl's EXPL_<unit>_<label>__MAIN_LOOP against openvcl's loop.

    oxide-vcl names an expanded loop after the first label at its head; openvcl
    keeps the source labels, so the loop's block in openvcl's listing may sit
    under another label at the same address (the largest one is taken).
    """
    oxide, openvcl = result["costs"]["oxide"][1], result["costs"]["openvcl"][1]
    pattern = re.compile(rf"EXPL_{re.escape(result['unit'] or '')}_(.+)__MAIN_LOOP$")
    rows = []
    for label, cycles in sorted(oxide.items()):
        match = pattern.match(label)
        if match:
            head = match.group(1)
            names = result["aliases"].get(head, [head])
            found = [(openvcl[name], name) for name in names if name in openvcl]
            other, other_label = max(found) if found else (None, head)
            rows.append((other_label, cycles, other))
    return rows


def summary(results, openvcl_version):
    lines = [f"# oxide-vcl and openvcl ({openvcl_version})", "",
             "Estimated cycles from `openvcl --cost`; each block counted once.", "",
             "| program | oxide-vcl | openvcl | ratio | paired cycles | openvcl exit | oxide-vcl = reference |",
             "|---|---:|---:|---:|---:|---:|---|"]
    for result in results:
        oxide, openvcl = result["costs"]["oxide"][0], result["costs"]["openvcl"][0]
        a, b = oxide.get(TOTALS["estimated"], 0), openvcl.get(TOTALS["estimated"], 0)
        ratio = f"{b / a:.2f}x" if a else "-"
        paired = f"{oxide.get(TOTALS['paired'], 0)} / {openvcl.get(TOTALS['paired'], 0)}"
        lines.append(f"| {result['name']} | {a} | {b} | {ratio} | {paired} | "
                     f"{result['status']['openvcl']} | {'yes' if result['matches_reference'] else 'NO'} |")
    lines += ["", "## Software-pipelined loops (cycles per pass of the steady-state loop)", "",
              "| program | loop | oxide-vcl main loop | openvcl loop |", "|---|---|---:|---:|"]
    for result in results:
        for label, a, b in loop_rows(result):
            lines.append(f"| {result['name']} | {label} | {a} | {'-' if b is None else b} |")
    lines += ["", "## Blocks (estimated cycles)", ""]
    for result in results:
        oxide, openvcl = result["costs"]["oxide"][1], result["costs"]["openvcl"][1]
        lines += [f"### {result['name']}", "", "| label | oxide-vcl | openvcl |", "|---|---:|---:|"]
        for label in sorted(set(oxide) | set(openvcl)):
            if label.startswith("__"):
                continue
            lines.append(f"| {label} | {oxide.get(label, '-')} | {openvcl.get(label, '-')} |")
        lines.append("")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("cases", nargs="*", help="tests/corpus program names (default: the documented set)")
    parser.add_argument("--openvcl", type=pathlib.Path, required=True,
                        help="the openvcl executable, or an install prefix holding bin/openvcl")
    parser.add_argument("--out", type=pathlib.Path, required=True, help="output directory")
    parser.add_argument("--candidate", type=pathlib.Path, default=ROOT / "target/release/oxide-vcl")
    parser.add_argument("--corpus", type=pathlib.Path, default=ROOT / "tests/corpus")
    parser.add_argument("--timeout", type=float, default=120)
    options = parser.parse_args()
    openvcl = openvcl_binary(options.openvcl)
    options.candidate = options.candidate.resolve()
    options.out = options.out.resolve()
    version = subprocess.run([str(openvcl), "--version"], capture_output=True, text=True).stdout.strip()
    results = []
    for name in options.cases or DEFAULT_CASES:
        result = compare(name, options, openvcl)
        results.append(result)
        oxide, other = (result["costs"][tool][0].get(TOTALS["estimated"], 0) for tool in ("oxide", "openvcl"))
        note = "" if result["status"]["openvcl"] == 0 else f"  (openvcl exit {result['status']['openvcl']})"
        reference = "" if result["matches_reference"] else "  (oxide-vcl differs from the reference)"
        print(f"{result['name']:52} oxide-vcl {oxide:5}  openvcl {other:5}{note}{reference}")
    (options.out / "summary.md").write_text(summary(results, version or "unknown version"))
    print(f"wrote {options.out / 'summary.md'}")


if __name__ == "__main__":
    main()
