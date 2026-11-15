//! The macros defined so far: `#define` constants and function-like macros,
//! `#macro` blocks and the built-ins.

use std::collections::HashMap;
use std::rc::Rc;

use super::lexer::{Loc, Token};

/// The built-in macros, which can be neither redefined nor undefined.
pub(super) const BUILTIN_NAMES: [&str; 4] = ["__FILE__", "__LINE__", "__COUNTER__", "__VCLPP__"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MacroKind {
    /// `#define NAME body`
    ObjectLike,
    /// `#define NAME(params) body`
    FunctionLike,
    /// `#macro NAME: params ... #endmacro`, invoked as `NAME{ args }`
    Block,
    /// `__FILE__`, `__LINE__`, `__COUNTER__` and `__VCLPP__`
    Builtin,
}

#[derive(Clone, Debug)]
pub(super) struct Macro {
    pub name: String,
    /// The interned name, for hide sets.
    pub id: u32,
    pub kind: MacroKind,
    pub params: Vec<String>,
    /// Function-like with a trailing `...` (`__VA_ARGS__`).
    pub variadic: bool,
    pub body: Vec<Token>,
    pub defined_at: Loc,
}

impl Macro {
    pub fn new(name: &str, kind: MacroKind, defined_at: Loc) -> Self {
        Self {
            name: name.to_string(),
            id: 0,
            kind,
            params: Vec::new(),
            variadic: false,
            body: Vec::new(),
            defined_at,
        }
    }

    /// The index of the parameter `token` names, if any. `__VA_ARGS__` is
    /// the one after the named ones.
    pub fn param_index(&self, token: &Token) -> Option<usize> {
        if !token.is_identifier() {
            return None;
        }
        if let Some(index) = self.params.iter().position(|param| *param == token.text) {
            return Some(index);
        }
        (self.variadic && token.text == "__VA_ARGS__").then_some(self.params.len())
    }

    /// The same definition, as C allows a macro to be redefined with: the
    /// same tokens, with whitespace in the same places (however much).
    pub fn same_definition(&self, other: &Macro) -> bool {
        self.kind == other.kind
            && self.params == other.params
            && self.variadic == other.variadic
            && self.body.len() == other.body.len()
            && self.body.iter().zip(&other.body).all(|(a, b)| {
                a.text == b.text
                    && a.starts_line == b.starts_line
                    && a.leading.is_empty() == b.leading.is_empty()
            })
    }
}

pub(super) struct MacroTable {
    macros: HashMap<String, Rc<Macro>>,
    ids: HashMap<String, u32>,
}

impl MacroTable {
    /// A table with the built-ins defined.
    pub fn new() -> Self {
        let mut table = Self {
            macros: HashMap::new(),
            ids: HashMap::new(),
        };
        for name in BUILTIN_NAMES {
            table.define(Macro::new(name, MacroKind::Builtin, Loc::default()));
        }
        table
    }

    pub fn find(&self, name: &str) -> Option<Rc<Macro>> {
        self.macros.get(name).cloned()
    }

    pub fn is_defined(&self, name: &str) -> bool {
        self.macros.contains_key(name)
    }

    /// Defines or redefines a macro, returning the definition it replaced.
    pub fn define(&mut self, mut definition: Macro) -> Option<Rc<Macro>> {
        definition.id = self.intern(&definition.name);
        self.macros
            .insert(definition.name.clone(), Rc::new(definition))
    }

    pub fn undefine(&mut self, name: &str) {
        self.macros.remove(name);
    }

    /// The id hide sets know a name by.
    pub fn intern(&mut self, name: &str) -> u32 {
        let next = self.ids.len() as u32;
        *self.ids.entry(name.to_string()).or_insert(next)
    }

    pub fn is_builtin_name(name: &str) -> bool {
        BUILTIN_NAMES.contains(&name)
    }
}
