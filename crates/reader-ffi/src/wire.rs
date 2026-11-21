use reader_core::{Backup, Vocabulary};
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const MAX_WIRE: usize = 256 * 1024 * 1024;

#[derive(Deserialize)]
pub struct Request {
    pub version: u32,
    #[serde(flatten)]
    pub command: Command,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Open {
        path: String,
    },
    Snapshot,
    Import {
        name: String,
        bytes: Vec<u8>,
        language: String,
    },
    Position {
        id: String,
        position: usize,
    },
    SetBookmark {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        saved: bool,
    },
    SaveWord {
        entry: Vocabulary,
    },
    Review {
        id: String,
        direction: String,
        grade: u8,
    },
    Original {
        id: String,
    },
    Export,
    Restore {
        backup: Backup,
    },
}

#[derive(Debug, Serialize)]
pub struct Error {
    pub version: u32,
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            version: VERSION,
            code,
            message: message.into(),
            retryable: false,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
