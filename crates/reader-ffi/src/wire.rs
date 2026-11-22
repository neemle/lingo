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
    ImportImage {
        name: String,
        bytes: Vec<u8>,
        language: String,
        lines: Vec<reader_core::ocr::OcrLine>,
    },
    PdfPagesNeedingOcr {
        name: String,
        bytes: Vec<u8>,
        pages: u16,
    },
    ImportMixedPdf {
        name: String,
        bytes: Vec<u8>,
        language: String,
        lines: Vec<reader_core::ocr::OcrLine>,
        pages: u16,
    },
    ImportScannedPdf {
        name: String,
        bytes: Vec<u8>,
        language: String,
        lines: Vec<reader_core::ocr::OcrLine>,
        pages: u16,
    },
    CorrectOcrLine {
        id: String,
        index: usize,
        correction: Option<String>,
    },
    SetDocumentLanguage {
        id: String,
        language: String,
    },
    Position {
        id: String,
        position: usize,
    },
    SpeechResume {
        id: String,
    },
    SetSpeechResume {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        #[serde(rename = "textHash")]
        text_hash: String,
        #[serde(rename = "offsetUtf16")]
        offset_utf16: Option<usize>,
    },
    SetBookmark {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        saved: bool,
    },
    SetReviewedTranslation {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        target: String,
        text: String,
    },
    TranslationPackages,
    KnowledgePackages,
    InstallKnowledgePackage {
        language: String,
        path: String,
    },
    LookupWord {
        language: String,
        word: String,
        context: Option<reader_core::knowledge::WordContext>,
    },
    SuggestOcr {
        language: String,
        text: String,
    },
    InstallTranslationPackage {
        path: String,
    },
    TranslateSentence {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        target: String,
    },
    SaveWord {
        entry: Vocabulary,
    },
    Review {
        id: String,
        direction: String,
        grade: u8,
    },
    Practice,
    SetPractice {
        progress: reader_core::practice::PracticeProgress,
    },
    GradePractice {
        revision: u64,
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
