//! VCL source being written: its lines, each with the place in the
//! preprocessed program it came from, and the registers it names.
//! Instructions stay structured until the end, for the peephole passes.

use crate::sema::types::{Lane, Placement};
use crate::syntax::Pos;

/// A VF register: VF00, or a VCL variable.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Vf {
    /// VF00: (0, 0, 0, 1).
    Zero,
    Var(usize),
}

/// A VI register: VI00, VI01 (where FCAND writes), or a VCL variable.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum Vi {
    Zero,
    Flags,
    Var(usize),
}

/// A line of VCL source.
#[derive(Clone, Debug)]
pub enum Line {
    /// A directive or comment, as it is.
    Text(String),
    Label(String),
    Inst(Inst),
}

/// An instruction: its mnemonic, field mask and operands.
#[derive(Clone, Debug)]
pub struct Inst {
    pub mnemonic: String,
    pub mask: Option<String>,
    pub operands: Vec<String>,
    /// For a MOVE of raw bits: the exact instructions it becomes if the
    /// peephole passes leave it (vcl may make a MOVE an inexact MAX).
    pub exact: Option<Vec<Inst>>,
}

impl Inst {
    fn text(&self) -> String {
        let mnemonic = match &self.mask {
            Some(mask) => format!("{}.{mask}", self.mnemonic),
            None => self.mnemonic.clone(),
        };
        if self.operands.is_empty() {
            format!("        {mnemonic}")
        } else {
            format!("        {mnemonic:<10} {}", self.operands.join(", "))
        }
    }
}

/// The text being written.
#[derive(Default)]
pub struct Output {
    lines: Vec<(Line, Option<Pos>)>,
    /// The place of the statement or expression being lowered.
    pub pos: Option<Pos>,
    vf_names: Vec<String>,
    vi_names: Vec<String>,
    labels: usize,
    /// Before every label: a library entry's name, so the entries of one
    /// listing, and the programs beside them in micro memory, name theirs
    /// apart.
    pub label_prefix: String,
}

/// A name's characters a VCL variable may hold: letters, digits and `_`.
fn sanitized(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Field letters, in lane order, for a mask.
pub fn mask_of(placement: &Placement) -> String {
    let mut lanes: Vec<Lane> = placement.lanes().to_vec();
    lanes.sort();
    lanes.iter().map(|lane| lane.letter()).collect()
}

impl Output {
    /// A new VF variable, named after `hint`.
    pub fn vf(&mut self, hint: &str) -> Vf {
        self.vf_names
            .push(format!("f{}_{}", sanitized(hint), self.vf_names.len()));
        Vf::Var(self.vf_names.len() - 1)
    }

    /// A new VI variable, named after `hint`.
    pub fn vi(&mut self, hint: &str) -> Vi {
        self.vi_names
            .push(format!("i{}_{}", sanitized(hint), self.vi_names.len()));
        Vi::Var(self.vi_names.len() - 1)
    }

    /// Physical register VF`number`, which a library call passes a value
    /// in: its name is not a variable's, so no pass renames it.
    pub fn vf_physical(&mut self, number: u8) -> Vf {
        self.vf_names.push(format!("vf{number:02}"));
        Vf::Var(self.vf_names.len() - 1)
    }

    /// Physical register VI`number`, as [`Output::vf_physical`].
    pub fn vi_physical(&mut self, number: u8) -> Vi {
        self.vi_names.push(format!("vi{number:02}"));
        Vi::Var(self.vi_names.len() - 1)
    }

    /// A new label, named after `hint`.
    pub fn label(&mut self, hint: &str) -> String {
        self.labels += 1;
        format!("{}l{}_{}", self.label_prefix, sanitized(hint), self.labels)
    }

    pub fn vf_name(&self, reg: Vf) -> String {
        match reg {
            Vf::Zero => "vf00".into(),
            Vf::Var(index) => self.vf_names[index].clone(),
        }
    }

    pub fn vi_name(&self, reg: Vi) -> String {
        match reg {
            Vi::Zero => "vi00".into(),
            Vi::Flags => "vi01".into(),
            Vi::Var(index) => self.vi_names[index].clone(),
        }
    }

    /// A VF register with one field: `fC_3[w]`.
    pub fn field(&self, reg: Vf, lane: Lane) -> String {
        format!("{}[{}]", self.vf_name(reg), lane.letter())
    }

    /// A line as it is, at the current place.
    pub fn line(&mut self, text: impl Into<String>) {
        self.lines.push((Line::Text(text.into()), self.pos));
    }

    /// An instruction: its mnemonic, a field mask if any, and operands.
    pub fn inst(&mut self, mnemonic: &str, mask: Option<&str>, operands: &[String]) {
        let inst = Inst {
            mnemonic: mnemonic.to_string(),
            mask: mask.filter(|mask| !mask.is_empty()).map(str::to_string),
            operands: operands.to_vec(),
            exact: None,
        };
        self.lines.push((Line::Inst(inst), self.pos));
    }

    /// A MOVE of raw bits, which becomes `exact` unless a peephole pass
    /// removes it.
    pub fn raw_move(&mut self, mask: &str, operands: &[String], exact: Vec<Inst>) {
        let inst = Inst {
            mnemonic: "move".into(),
            mask: Some(mask.to_string()),
            operands: operands.to_vec(),
            exact: Some(exact),
        };
        self.lines.push((Line::Inst(inst), self.pos));
    }

    /// An instruction to keep aside, for [`Output::raw_move`].
    pub fn make(mnemonic: &str, mask: Option<&str>, operands: &[String]) -> Inst {
        Inst {
            mnemonic: mnemonic.to_string(),
            mask: mask.map(str::to_string),
            operands: operands.to_vec(),
            exact: None,
        }
    }

    /// An instruction at line `at`, before the lines from there on, at
    /// the current place.
    pub fn insert_inst(&mut self, at: usize, mnemonic: &str, mask: &str, operands: &[String]) {
        let inst = Inst {
            mnemonic: mnemonic.to_string(),
            mask: Some(mask.to_string()),
            operands: operands.to_vec(),
            exact: None,
        };
        self.lines.insert(at, (Line::Inst(inst), self.pos));
    }

    /// How many VF variables there are so far: those made before a point
    /// number below its count.
    pub fn vf_count(&self) -> usize {
        self.vf_names.len()
    }

    /// Whether `reg` names a physical register, a library call's.
    pub fn is_physical(&self, reg: Vf) -> bool {
        matches!(reg, Vf::Var(index) if self.vf_names[index].starts_with("vf"))
    }

    /// How many lines are written so far: a count that grows with every
    /// instruction.
    pub fn instructions(&self) -> usize {
        self.lines.len()
    }

    pub fn place_label(&mut self, label: &str) {
        self.lines.push((Line::Label(label.to_string()), self.pos));
    }

    /// The text, after the peephole passes, and for each of its lines the
    /// place it came from.
    pub fn finish(mut self) -> (String, Vec<Option<Pos>>) {
        super::peephole::run(&mut self.lines);
        let mut text = String::new();
        let mut origins = Vec::with_capacity(self.lines.len());
        for (line, pos) in self.lines {
            match line {
                Line::Text(line) => text.push_str(&line),
                Line::Label(label) => {
                    text.push_str(&label);
                    text.push(':');
                }
                Line::Inst(inst) => text.push_str(&inst.text()),
            }
            text.push('\n');
            origins.push(pos);
        }
        (text, origins)
    }
}
