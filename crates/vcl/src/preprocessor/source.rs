//! Source files: loading them through a [`SourceLoader`], finding the file an
//! `#include` names, and remembering `#pragma once`.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use super::lexer::Loc;

/// Where the preprocessor reads files from: the file system, or memory in
/// tests.
pub trait SourceLoader {
    /// Whether `path` names a regular file.
    fn is_file(&self, path: &Path) -> bool;
    /// The file's text, or `None` when it cannot be read as UTF-8 text.
    fn read(&self, path: &Path) -> Option<String>;
    /// A name the same file has however a path reaches it, for `#pragma
    /// once`.
    fn identity(&self, path: &Path) -> String {
        normalize(path).to_string_lossy().into_owned()
    }
}

/// The file system, with paths relative to the working directory.
pub struct FileSystem;

impl SourceLoader for FileSystem {
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn identity(&self, path: &Path) -> String {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| normalize(path))
            .to_string_lossy()
            .into_owned()
    }
}

/// One inclusion of a file. A file included twice is loaded twice, so that
/// each inclusion knows where it was included from.
#[derive(Clone, Debug)]
pub struct SourceFile {
    /// As opened; `None` for text that is not a file.
    pub path: Option<PathBuf>,
    /// What diagnostics and `__FILE__` call it.
    pub name: String,
    pub text: String,
    pub included_from: Option<Loc>,
}

pub(super) struct Sources<'a> {
    loader: &'a dyn SourceLoader,
    include_dirs: Vec<PathBuf>,
    pub(super) files: Vec<SourceFile>,
    include_once: HashSet<String>,
}

impl<'a> Sources<'a> {
    pub(super) fn new(loader: &'a dyn SourceLoader, include_dirs: &[String]) -> Self {
        Self {
            loader,
            include_dirs: include_dirs.iter().map(PathBuf::from).collect(),
            files: Vec::new(),
            include_once: HashSet::new(),
        }
    }

    pub(super) fn file(&self, index: usize) -> &SourceFile {
        &self.files[index]
    }

    /// Adds the main file, already read, under the name it is given.
    pub(super) fn add_main(&mut self, name: &str, text: &str) -> usize {
        self.files.push(SourceFile {
            path: Some(PathBuf::from(name)),
            name: name.to_string(),
            text: text.to_string(),
            included_from: None,
        });
        self.files.len() - 1
    }

    /// Adds text that is not a file, such as a `--define` value.
    pub(super) fn add_text(&mut self, name: &str, text: &str) -> usize {
        self.files.push(SourceFile {
            path: None,
            name: name.to_string(),
            text: text.to_string(),
            included_from: None,
        });
        self.files.len() - 1
    }

    /// Loads a file, returning its index, or `None` if it cannot be read.
    pub(super) fn load(&mut self, path: &Path, included_from: Loc) -> Option<usize> {
        let text = self.loader.read(path)?;
        self.files.push(SourceFile {
            path: Some(path.to_path_buf()),
            name: normalize(path).to_string_lossy().into_owned(),
            text,
            included_from: Some(included_from),
        });
        Some(self.files.len() - 1)
    }

    /// The file an `#include` names. `"name"` is looked for next to the
    /// file that includes it, then in each `-I` directory, then in the
    /// working directory; `<name>` only in the `-I` directories. Returns the
    /// path found, or every candidate tried.
    pub(super) fn find_include(
        &self,
        name: &str,
        angled: bool,
        includer: usize,
    ) -> Result<PathBuf, Vec<PathBuf>> {
        let name_path = Path::new(name);
        let mut candidates = Vec::new();
        if name_path.is_absolute() {
            candidates.push(name_path.to_path_buf());
        } else {
            if !angled {
                let directory = self.files[includer]
                    .path
                    .as_deref()
                    .and_then(Path::parent)
                    .unwrap_or(Path::new(""));
                candidates.push(directory.join(name_path));
            }
            for directory in &self.include_dirs {
                candidates.push(directory.join(name_path));
            }
            if !angled {
                candidates.push(name_path.to_path_buf());
            }
        }
        let mut tried: Vec<PathBuf> = Vec::new();
        for candidate in candidates {
            let normal = normalize(&candidate);
            // The includer's directory and the working directory are often
            // the same.
            if tried.contains(&normal) {
                continue;
            }
            if self.loader.is_file(&normal) {
                return Ok(normal);
            }
            tried.push(normal);
        }
        Err(tried)
    }

    /// `#pragma once`: the file is not included again.
    pub(super) fn mark_include_once(&mut self, file: usize) {
        if let Some(path) = &self.files[file].path {
            let identity = self.loader.identity(path);
            self.include_once.insert(identity);
        }
    }

    pub(super) fn is_include_once(&self, path: &Path) -> bool {
        self.include_once.contains(&self.loader.identity(path))
    }
}

/// `path` with `.` dropped and `name/..` folded away, without touching the
/// file system.
pub fn normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if matches!(parts.last(), Some(Component::Normal(_))) => {
                parts.pop();
            }
            Component::ParentDir if matches!(parts.last(), Some(Component::RootDir)) => {}
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return PathBuf::from(".");
    }
    parts.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_normalize_without_the_file_system() {
        for (path, normal) in [
            ("a/./b", "a/b"),
            ("a/b/../c", "a/c"),
            ("../a", "../a"),
            ("./a", "a"),
            ("/../a", "/a"),
            ("a/..", "."),
        ] {
            assert_eq!(normalize(Path::new(path)), PathBuf::from(normal), "{path}");
        }
    }
}
