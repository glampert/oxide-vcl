//! Benchmark tables: one line per measurement, its key (the words before
//! the first `name=value`) then its values, in a text file under
//! `tests/bench` that a commit's diff shows row by row. A bench measures,
//! prints what changed against the committed table, and with
//! `BENCH_UPDATE=1` writes the table instead (docs/TESTING.md,
//! "Benchmarks").

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

use crate::vu_model::Stall;

/// One measurement: its key's words and its values, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub key: Vec<String>,
    pub values: Vec<(&'static str, u64)>,
}

impl Row {
    pub fn new(key: &[&str]) -> Self {
        Row {
            key: key.iter().map(|word| word.to_string()).collect(),
            values: Vec::new(),
        }
    }

    pub fn value(mut self, name: &'static str, value: u64) -> Self {
        self.values.push((name, value));
        self
    }
}

/// The table as text: a heading, then the rows in order, keys aligned.
pub fn format(heading: &str, rows: &[Row]) -> String {
    let mut out = String::new();
    for line in heading.lines() {
        let _ = writeln!(out, "# {line}");
    }
    let width = rows
        .iter()
        .map(|row| row.key.join(" ").len())
        .max()
        .unwrap_or(0);
    for row in rows {
        let values: Vec<String> = row
            .values
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        let _ = writeln!(out, "{:width$}  {}", row.key.join(" "), values.join(" "));
    }
    out
}

/// A table's rows by key, each a map of its values.
pub fn parse(text: &str) -> BTreeMap<String, BTreeMap<String, u64>> {
    let mut rows = BTreeMap::new();
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let mut key = Vec::new();
        let mut values = BTreeMap::new();
        for word in line.split_whitespace() {
            match word.split_once('=') {
                Some((name, value)) => {
                    values.insert(name.to_string(), value.parse().unwrap_or(0));
                }
                None => key.push(word),
            }
        }
        if !key.is_empty() {
            rows.insert(key.join(" "), values);
        }
    }
    rows
}

fn change(old: u64, new: u64) -> String {
    let percent = if old == 0 {
        String::new()
    } else {
        format!(" ({:+.1}%)", (new as f64 - old as f64) * 100.0 / old as f64)
    };
    format!("{old} -> {new}{percent}")
}

/// A row's group for the totals: its key's first word (the mode, or `vusl`
/// and `hand`), and the kind of a row whose last word has one (`kernel` in
/// `kernel:xform_loop`, `phase` in `phase:schedule`), so that loop parts
/// and phases total apart from what they are parts of.
fn group(key: &[String]) -> String {
    match key.last().and_then(|word| word.split_once(':')) {
        Some((kind, _)) if key.len() > 1 => format!("{} {kind}", key[0]),
        _ => key[0].clone(),
    }
}

/// What changed from `old` to `new`: each changed row's changed values,
/// rows only one has, and per group ([`group`]) the totals of each value
/// over the rows both have, a value a row lacks counting as 0.
pub fn report(old: &str, new: &[Row]) -> String {
    let old = parse(old);
    let mut out = String::new();
    let mut totals: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();
    let mut seen = Vec::new();
    for row in new {
        let key = row.key.join(" ");
        seen.push(key.clone());
        let Some(before) = old.get(&key) else {
            let _ = writeln!(out, "new      {key}");
            continue;
        };
        let mut changed = Vec::new();
        for &(name, value) in &row.values {
            // Values such as the stall causes are written only when nonzero.
            let earlier = before.get(name).copied();
            let total = totals
                .entry((group(&row.key), name.to_string()))
                .or_insert((0, 0));
            total.0 += earlier.unwrap_or(0);
            total.1 += value;
            if earlier != Some(value) {
                let earlier = earlier.map_or("-".to_string(), |value| value.to_string());
                changed.push(format!("{name} {earlier} -> {value}"));
            }
        }
        for (name, &earlier) in before {
            if row.values.iter().all(|&(known, _)| known != name) {
                let total = totals
                    .entry((group(&row.key), name.clone()))
                    .or_insert((0, 0));
                total.0 += earlier;
                changed.push(format!("{name} {earlier} -> -"));
            }
        }
        if !changed.is_empty() {
            let _ = writeln!(out, "changed  {key}: {}", changed.join(", "));
        }
    }
    for key in old.keys().filter(|key| !seen.contains(key)) {
        let _ = writeln!(out, "gone     {key}");
    }
    let mut group = String::new();
    for ((name, value), (before, after)) in &totals {
        if before == after {
            continue;
        }
        if *name != group {
            let _ = writeln!(out, "total    {name}:");
            group = name.clone();
        }
        let _ = writeln!(out, "           {value} {}", change(*before, *after));
    }
    if out.is_empty() {
        out.push_str("no change\n");
    }
    out
}

/// Prints what `rows` change against the table at `path`, or with
/// `BENCH_UPDATE=1` writes them there.
pub fn compare_or_update(path: &Path, heading: &str, rows: &[Row]) {
    let text = format(heading, rows);
    if std::env::var_os("BENCH_UPDATE").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        println!("wrote {}", path.display());
        return;
    }
    let old = std::fs::read_to_string(path).unwrap_or_default();
    print!("{}", report(&old, rows));
}

/// Stall cycles by cause ([`Stall`]).
pub type Causes = [u64; Stall::COUNT];

/// The stall cycles of `runs` by cause.
pub fn run_causes(runs: &[crate::vu_model::Execution]) -> Causes {
    let mut causes = [0; Stall::COUNT];
    for run in runs {
        for pair in &run.stall_causes {
            add(&mut causes, pair);
        }
    }
    causes
}

fn add(total: &mut Causes, causes: &Causes) {
    for (total, value) in total.iter_mut().zip(causes) {
        *total += value;
    }
}

/// `stalls` and, after it, the nonzero stall cycles of each cause.
pub fn stalls(mut row: Row, causes: &Causes) -> Row {
    row = row.value("stalls", causes.iter().sum());
    for cause in Stall::ALL {
        let value = causes[cause as usize];
        if value > 0 {
            row = row.value(cause.name(), value);
        }
    }
    row
}

/// The work counts of a compile as values: its program and region
/// compiles, then its total work ([`work_values`]).
pub fn work(row: Row, stats: &vcl::stats::Stats) -> Row {
    let row = row
        .value("programs", stats.programs)
        .value("regions", stats.regions);
    work_values(row, &stats.total)
}

/// Work counts as values: search nodes, wrangler steps and checks, flow
/// analyses and the blocks they walk.
pub fn work_values(row: Row, work: &vcl::stats::Work) -> Row {
    row.value("nodes", work.search_nodes)
        .value("steps", work.wrangle_steps)
        .value("checks", work.wrangle_checks)
        .value("analyses", work.flow_analyses)
        .value("walks", work.flow_walks)
}

/// A row per phase that did work, under `key` and `phase:NAME`.
pub fn phase_rows(key: &[&str], stats: &vcl::stats::Stats) -> Vec<Row> {
    vcl::stats::Phase::ALL
        .into_iter()
        .zip(&stats.phases)
        .filter(|(_, work)| !work.is_empty())
        .map(|(phase, work)| {
            let name = format!("phase:{}", phase.name());
            let mut words = key.to_vec();
            words.push(&name);
            work_values(Row::new(&words), work)
        })
        .collect()
}

/// The part of a loop a table is, from its name: a modulo-scheduled kernel,
/// a prologue (`..._ENTRY_POINT`, `..._PROn`), an epilogue (`..._EPIn`),
/// each a loop's tables together, or an exit (`..._EXIT_POINT`); else a
/// table of its own, such as a loop scheduled as straight-line code.
pub fn part(table: &crate::vu_model::Table, unique: &str) -> String {
    let prefix = format!("EXPL_{unique}_");
    let name = table.name.strip_prefix(&prefix).unwrap_or(&table.name);
    if let Some((loop_name, part)) = name.rsplit_once("__") {
        let part = match part {
            "MAIN_LOOP" => "kernel",
            "ENTRY_POINT" => "prologue",
            "EXIT_POINT" => "exit",
            part if part.starts_with("PRO") => "prologue",
            part if part.starts_with("EPI") => "epilogue",
            _ => return format!("table:{name}"),
        };
        return format!("{part}:{loop_name}");
    }
    if table.kernel {
        format!("kernel:{name}")
    } else {
        format!("table:{name}")
    }
}

/// A loop part's or table's totals: its name, entries (issues of its first
/// pair: a kernel's iterations), cycles and stall cycles by cause.
#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    pub entries: u64,
    pub cycles: u64,
    pub causes: Causes,
}

/// Adds `part` to `parts`: to the part of the same name, with `entries`
/// combining the two entry counts, or as a new one.
pub fn merge(parts: &mut Vec<Part>, part: Part, entries: impl Fn(u64, u64) -> u64) {
    match parts.iter_mut().find(|known| known.name == part.name) {
        Some(known) => {
            known.entries = entries(known.entries, part.entries);
            known.cycles += part.cycles;
            add(&mut known.causes, &part.causes);
        }
        None => parts.push(part),
    }
}

/// Adds the cycles `runs` of `program` spent in each table to `parts`, by
/// [`part`]: what a table's pairs issued and stalled, and how often its
/// first pair issued, which for a prologue of several tables is its
/// first's. `unique` is the generated labels' prefix (`-u`). The runs are
/// one measurement: a program's runs at one count, or on one scene.
pub fn add_parts(
    parts: &mut Vec<Part>,
    program: &crate::vu_model::Program,
    runs: &[crate::vu_model::Execution],
    unique: &str,
) {
    let tables = program.tables();
    // These runs' parts, then added to `parts`.
    let mut these: Vec<Part> = Vec::new();
    for (index, table) in tables.iter().enumerate() {
        let end = tables
            .get(index + 1)
            .map_or(program.pair_count(), |next| next.start);
        let pairs = table.start..end.max(table.start);
        if pairs.is_empty() {
            continue;
        }
        let entries: u64 = runs.iter().map(|run| run.profile[table.start].0).sum();
        let mut causes = [0; Stall::COUNT];
        let mut issues = 0;
        for pc in pairs {
            for run in runs {
                issues += run.profile[pc].0;
                add(&mut causes, &run.stall_causes[pc]);
            }
        }
        let name = part(table, unique);
        // A prologue's tables run one after another, the first on every
        // entry; a loop's epilogues are its exits, one each.
        let prologue = name.starts_with("prologue:");
        let part = Part {
            name,
            entries,
            cycles: issues + causes.iter().sum::<u64>(),
            causes,
        };
        merge(
            &mut these,
            part,
            |a, b| if prologue { a.max(b) } else { a + b },
        );
    }
    for part in these {
        merge(parts, part, |a, b| a + b);
    }
}

/// A row per part entered, under `key`: entries, cycles, stall cycles with
/// their causes, and cycles per entry in hundredths (`per100`: a kernel's
/// cycles per iteration).
pub fn part_rows(key: &[&str], parts: Vec<Part>) -> Vec<Row> {
    parts
        .into_iter()
        .filter(|part| part.entries > 0)
        .map(|part| {
            let mut words = key.to_vec();
            words.push(&part.name);
            let row = Row::new(&words)
                .value("entries", part.entries)
                .value("cycles", part.cycles);
            stalls(row, &part.causes).value("per100", part.cycles * 100 / part.entries)
        })
        .collect()
}
