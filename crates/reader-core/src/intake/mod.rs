use crate::{ocr, Result, Store};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod application;
mod document_importer;
mod persistence;

pub use persistence::{prepare, PreparedIntake};
pub(crate) use persistence::initialize;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum IntakeInput {
    Text { name: String, text: String },
    Link { url: String },
    File { name: String, bytes: Vec<u8> },
    PdfFile { name: String, path: PathBuf },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum IntakeImport {
    Document { language: Option<String> },
    Article { name: String, text: String, language: Option<String> },
    Photo { lines: Vec<ocr::OcrLine>, language: Option<String> },
    Pdf {
        #[serde(rename = "pageTexts")]
        page_texts: Vec<String>,
        language: String,
    },
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum IntakeKind {
    Text, Link, Txt, Docx, Photo, Pdf, TextPdf, ScannedPdf, MixedPdf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum IntakeAction { ImportDocument, ExtractArticle, RecognizePhoto, ReadPdfPages, Complete }

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    name: String,
    kind: IntakeKind,
    bytes: u64,
    received_at: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntakeItem {
    pub id: String,
    #[serde(flatten)]
    metadata: Metadata,
    next_action: IntakeAction,
    document_id: Option<String>,
}

impl IntakeItem {
    fn new(id: String, metadata: Metadata, document_id: Option<String>) -> Self {
        let next_action = if document_id.is_some() {
            IntakeAction::Complete
        } else {
            match metadata.kind {
                IntakeKind::Text | IntakeKind::Txt | IntakeKind::Docx => IntakeAction::ImportDocument,
                IntakeKind::Link => IntakeAction::ExtractArticle,
                IntakeKind::Photo => IntakeAction::RecognizePhoto,
                _ => IntakeAction::ReadPdfPages,
            }
        };
        Self { id, metadata, next_action, document_id }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum IntakePayload {
    Bytes { bytes: Vec<u8> },
    Link { url: String },
    File { path: PathBuf },
}

impl Store {
    pub fn stage_intake(&mut self, input: IntakeInput) -> Result<IntakeItem> {
        let prepared = prepare(&self.file_original_directory, input)?;
        self.insert_intake(prepared, None)
    }

    pub fn complete_intake(&mut self, id: &str, result: IntakeImport) -> Result<IntakeItem> {
        let _guard = self.lock_intake()?;
        application::complete(self, id, result)
    }
}
