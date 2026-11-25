//! Benchmark tables: one line per measurement, its key (the words before
//! the first `name=value`) then its values, in a text file under
//! `tests/bench` that a commit's diff shows row by row. A bench measures,
//! prints what changed against the committed table, and with
//! `BENCH_UPDATE=1` writes the table instead (docs/TESTING.md,
//! "Benchmarks").

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

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

/// What changed from `old` to `new`: each changed row's changed values,
/// rows only one has, and per group (a key's first word) the totals of
/// each value over the rows both have.
pub fn report(old: &str, new: &[Row]) -> String {
    let old = parse(old);
    let mut out = String::new();
    let mut totals: BTreeMap<(String, &'static str), (u64, u64)> = BTreeMap::new();
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
            let earlier = before.get(name).copied();
            if let Some(earlier) = earlier {
                let total = totals.entry((row.key[0].clone(), name)).or_insert((0, 0));
                total.0 += earlier;
                total.1 += value;
            }
            if earlier != Some(value) {
                let earlier = earlier.map_or("-".to_string(), |value| value.to_string());
                changed.push(format!("{name} {earlier} -> {value}"));
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

/// The work counts of a compile as values.
pub fn work(row: Row, stats: &vcl::stats::Stats) -> Row {
    row.value("nodes", stats.search_nodes)
        .value("steps", stats.wrangle_steps)
        .value("checks", stats.wrangle_checks)
        .value("analyses", stats.flow_analyses)
        .value("walks", stats.flow_walks)
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
/// pair: a kernel's iterations), cycles and stall cycles.
pub type Part = (String, u64, u64, u64);

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
        let profile: Vec<(u64, u64)> = (table.start..end.max(table.start))
            .map(|pc| {
                runs.iter()
                    .map(|run| run.profile[pc])
                    .fold((0, 0), |(a, b), (c, d)| (a + c, b + d))
            })
            .collect();
        let Some(&(entries, _)) = profile.first() else {
            continue;
        };
        let issues: u64 = profile.iter().map(|&(issued, _)| issued).sum();
        let stalls: u64 = profile.iter().map(|&(_, stalled)| stalled).sum();
        let name = part(table, unique);
        match these.iter_mut().find(|(known, ..)| *known == name) {
            Some(entry) => {
                // A prologue's tables run one after another, the first on
                // every entry; a loop's epilogues are its exits, one each.
                entry.1 = if name.starts_with("prologue:") {
                    entry.1.max(entries)
                } else {
                    entry.1 + entries
                };
                entry.2 += issues + stalls;
                entry.3 += stalls;
            }
            None => these.push((name, entries, issues + stalls, stalls)),
        }
    }
    for (name, entries, cycles, stalls) in these {
        match parts.iter_mut().find(|(known, ..)| *known == name) {
            Some(entry) => {
                entry.1 += entries;
                entry.2 += cycles;
                entry.3 += stalls;
            }
            None => parts.push((name, entries, cycles, stalls)),
        }
    }
}

/// A row per part entered, under `key`: entries, cycles, stall cycles, and
/// cycles per entry in hundredths (`per100`: a kernel's cycles per
/// iteration).
pub fn part_rows(key: &[&str], parts: Vec<Part>) -> Vec<Row> {
    parts
        .into_iter()
        .filter(|&(_, entries, ..)| entries > 0)
        .map(|(name, entries, cycles, stalls)| {
            let mut words = key.to_vec();
            words.push(&name);
            Row::new(&words)
                .value("entries", entries)
                .value("cycles", cycles)
                .value("stalls", stalls)
                .value("per100", cycles * 100 / entries)
        })
        .collect()
}
