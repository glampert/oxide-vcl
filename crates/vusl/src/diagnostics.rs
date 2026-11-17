//! VUSL's messages and their wording. They reach `vcl`'s reporter as
//! [`vcl::Message::Frontend`], through `From`; nothing here prints.

use std::fmt;

use vcl::Message;
use vcl::diagnostics::FrontendMessage;

/// Every message the VUSL compiler and its driver give.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VuslMessage {
    // The command line.
    /// An option the driver does not know.
    UnknownOption { option: String },
    /// An option given without the value it takes.
    MissingValue { option: String },
    /// `--color=` with something other than `auto`, `always` or `never`.
    BadColor { value: String },
    /// `-D` with something other than `NAME` or `NAME=VALUE`, or naming a
    /// built-in macro.
    BadDefine { argument: String },
    /// No input file.
    NoInput,
    /// A second input file.
    ExtraInput { argument: String },

    // The driver.
    /// A run that would compile: the frontend is not there yet, so only
    /// `--preprocess-only` runs.
    CompileUnfinished,
}

impl VuslMessage {
    /// Whether the message is about the program's source, rather than the
    /// command line or the driver.
    pub fn in_source(&self) -> bool {
        use VuslMessage::*;
        match self {
            UnknownOption { .. }
            | MissingValue { .. }
            | BadColor { .. }
            | BadDefine { .. }
            | NoInput
            | ExtraInput { .. }
            | CompileUnfinished => false,
        }
    }

    /// The message's text, the token it is about and a suggested fix.
    pub fn wording(&self) -> FrontendMessage {
        use VuslMessage::*;
        let (text, subject, help): (String, Option<String>, Option<String>) = match self {
            UnknownOption { option } => (
                format!("unknown option '{option}'"),
                None,
                Some("run 'oxide-vusl --help' for the options".into()),
            ),
            MissingValue { option } => (format!("option '{option}' needs a value"), None, None),
            BadColor { value } => (
                format!("unknown --color value '{value}'"),
                None,
                Some("use --color=auto, --color=always or --color=never".into()),
            ),
            BadDefine { argument } => (
                format!("can't define '{argument}'"),
                None,
                Some(
                    "write -D NAME or -D NAME=VALUE, with a name that is not a built-in macro"
                        .into(),
                ),
            ),
            NoInput => (
                "no input file".into(),
                None,
                Some("name the program to compile: oxide-vusl prog.vusl".into()),
            ),
            ExtraInput { argument } => (
                format!("more than one input file: '{argument}'"),
                None,
                Some("compile one program per run".into()),
            ),
            CompileUnfinished => (
                "compiling VUSL is not implemented yet".into(),
                None,
                Some("--preprocess-only runs the preprocessor".into()),
            ),
        };
        FrontendMessage {
            text,
            subject,
            help,
            in_source: self.in_source(),
        }
    }
}

impl From<VuslMessage> for Message {
    fn from(message: VuslMessage) -> Self {
        Message::Frontend(message.wording())
    }
}

impl fmt::Display for VuslMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.wording().text)
    }
}
