//! What the command-line drivers built on the compiler share (`oxide-vcl`
//! and `oxide-vusl`): when their diagnostics are colored, and writing a run's
//! output files all or nothing.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

use crate::diagnostics::DriverMessage;

/// When diagnostics carry ANSI colors (`--color=`), as with clang and gcc.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Color {
    /// On when standard error is a terminal, unless `NO_COLOR` is set or
    /// `TERM` is `dumb`.
    #[default]
    Auto,
    Always,
    Never,
}

impl Color {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(Self::Auto),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    /// Whether to color: for `Auto`, whether standard error is a terminal
    /// and the environment allows colors (no `NO_COLOR`, `TERM` not
    /// `dumb`).
    pub fn enabled(self, terminal: bool, environment_allows: bool) -> bool {
        match self {
            Self::Auto => terminal && environment_allows,
            Self::Always => true,
            Self::Never => false,
        }
    }

    /// [`Color::enabled`] for this process: whether standard error is a
    /// terminal, and whether the environment allows colors (`NO_COLOR` set
    /// to anything but an empty string turns them off, as does `TERM=dumb`).
    pub fn enabled_here(self) -> bool {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        let dumb = std::env::var_os("TERM").is_some_and(|term| term == "dumb");
        self.enabled(io::stderr().is_terminal(), !no_color && !dumb)
    }
}

/// Writes each file to a temporary file beside it, then renames them all
/// into place. A failed write leaves every target as it was.
pub fn write_all_or_nothing(files: &[(&str, &[u8])]) -> Result<(), DriverMessage> {
    let failed = |path: &str, error: io::Error| DriverMessage::WriteOutput {
        path: path.to_string(),
        error: error.to_string(),
    };
    let mut staged: Vec<(PathBuf, &str)> = Vec::new();
    let discard = |staged: &[(PathBuf, &str)]| {
        for (temporary, _) in staged {
            let _ = std::fs::remove_file(temporary);
        }
    };
    for &(path, bytes) in files {
        let target = Path::new(path);
        let Some(name) = target.file_name() else {
            discard(&staged);
            return Err(failed(path, io::Error::from(io::ErrorKind::InvalidInput)));
        };
        let mut temporary_name = std::ffi::OsString::from(".");
        temporary_name.push(name);
        temporary_name.push(format!(".{}.tmp", std::process::id()));
        let temporary = target.with_file_name(temporary_name);
        if let Err(error) = std::fs::write(&temporary, bytes) {
            let _ = std::fs::remove_file(&temporary);
            discard(&staged);
            return Err(failed(path, error));
        }
        staged.push((temporary, path));
    }
    for (index, (temporary, path)) in staged.iter().enumerate() {
        if let Err(error) = std::fs::rename(temporary, path) {
            discard(&staged[index..]);
            return Err(failed(path, error));
        }
    }
    Ok(())
}
