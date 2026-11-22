use super::{ArticleDraft, IntakeImport, IntakeInput, IntakeItem, IntakeKind, IntakePayload};
use crate::{ocr, Result, MAX_IMPORT};
use std::path::{Path, PathBuf};

pub(super) trait IntakeRepository {
    fn load_for_import(&self, id: &str) -> Result<StoredIntake>;
    fn record_imported(&self, item: &IntakeItem) -> Result<()>;
}

pub(super) trait DocumentImporter {
    fn import_material(&mut self, material: ImportMaterial) -> Result<String>;
}

pub(super) struct StoredIntake {
    pub item: IntakeItem,
    pub payload: Option<IntakePayload>,
    pub article: Option<ArticleDraft>,
}

pub(super) enum ImportMaterial {
    Document { name: String, bytes: Vec<u8>, language: Option<String> },
    Article { name: String, bytes: Vec<u8>, url: String, language: Option<String> },
    Photo { name: String, bytes: Vec<u8>, lines: Vec<ocr::OcrLine>, language: Option<String> },
    Pdf { name: String, path: PathBuf, page_texts: Vec<String>, deferred_from: Option<u16>, language: String },
}

pub(super) fn complete(
    ports: &mut (impl IntakeRepository + DocumentImporter),
    id: &str,
    result: IntakeImport,
) -> Result<IntakeItem> {
    let StoredIntake { mut item, payload, article } = ports.load_for_import(id)?;
    if item.document_id.is_some() {
        return Ok(item);
    }
    let payload = payload.ok_or("Відсутній вміст матеріалу.")?;
    let material = match (item.metadata.kind, payload, result) {
        (
            IntakeKind::Text | IntakeKind::Txt | IntakeKind::Docx,
            IntakePayload::Bytes { bytes },
            IntakeImport::Document { language },
        ) => ImportMaterial::Document {
            name: item.metadata.name.clone(), bytes, language,
        },
        (
            IntakeKind::Link,
            IntakePayload::Link { url },
            IntakeImport::Article { name, text, language, source_url },
        ) => {
            let url = source_url.unwrap_or(url);
            validate_article_url(&url)?;
            ImportMaterial::Article {
                name: text_name(&name)?, bytes: text_bytes(text)?, url, language,
            }
        }
        (IntakeKind::Link, IntakePayload::Link { .. }, IntakeImport::StagedArticle { language }) => {
            let draft = article.ok_or("Завантажений текст статті відсутній.")?;
            validate_article_draft(&draft)?;
            ImportMaterial::Article {
                name: text_name(&draft.title)?,
                bytes: draft.text.into_bytes(),
                url: draft.source_url,
                language,
            }
        }
        (IntakeKind::Photo, IntakePayload::Bytes { bytes }, IntakeImport::Photo { lines, language }) => {
            ImportMaterial::Photo {
                name: item.metadata.name.clone(), bytes, lines, language,
            }
        }
        (IntakeKind::Pdf, IntakePayload::File { path }, IntakeImport::Pdf { page_texts, deferred_from, language }) => {
            if deferred_from.is_none() {
                item.metadata.kind = pdf_kind(&page_texts);
            }
            ImportMaterial::Pdf {
                name: item.metadata.name.clone(), path, page_texts, deferred_from, language,
            }
        }
        _ => return Err("Результат обробки не відповідає типу матеріалу.".into()),
    };
    let document_id = ports.import_material(material)?;
    let item = IntakeItem::new(item.id, item.metadata, Some(document_id));
    ports.record_imported(&item)?;
    Ok(item)
}

fn pdf_kind(page_texts: &[String]) -> IntakeKind {
    let has_text = |page: &String| !page.trim().is_empty();
    if !page_texts.iter().any(has_text) {
        IntakeKind::ScannedPdf
    } else if page_texts.iter().all(has_text) {
        IntakeKind::TextPdf
    } else {
        IntakeKind::MixedPdf
    }
}

pub(super) struct ValidatedInput {
    pub name: String,
    pub kind: IntakeKind,
    pub asset: ValidatedAsset,
}

pub(super) enum ValidatedAsset {
    Bytes(Vec<u8>),
    Link,
    PdfFile(PathBuf),
}

pub(super) fn validate_input(input: IntakeInput) -> Result<ValidatedInput> {
    let (name, kind, asset) = match input {
        IntakeInput::Text { name, text } => (
            text_name(&name)?, IntakeKind::Text, ValidatedAsset::Bytes(text_bytes(text)?),
        ),
        IntakeInput::Link { url } => {
            validate_article_url(&url)?;
            (url, IntakeKind::Link, ValidatedAsset::Link)
        }
        IntakeInput::File { name, bytes } => {
            if bytes.is_empty() || bytes.len() > MAX_IMPORT {
                return Err("Файл має містити від 1 байта до 20 МіБ. Для великого PDF передайте шлях.".into());
            }
            let name = safe_name(&name, 4092)?;
            let kind = classify_file(name, &bytes)?;
            (name.to_owned(), kind, ValidatedAsset::Bytes(bytes))
        }
        IntakeInput::PdfFile { name, path } => {
            let name = safe_name(&name, 4096)?;
            if !extension(name).eq_ignore_ascii_case("pdf") {
                return Err("Виберіть файл PDF".into());
            }
            if path.as_os_str().is_empty() || path.as_os_str().len() > 4096 {
                return Err("Некоректний шлях до PDF.".into());
            }
            (name.to_owned(), IntakeKind::Pdf, ValidatedAsset::PdfFile(path))
        }
    };
    Ok(ValidatedInput { name, kind, asset })
}

fn classify_file(name: &str, bytes: &[u8]) -> Result<IntakeKind> {
    let extension = extension(name).to_ascii_lowercase();
    match extension.as_str() {
        "pdf" if bytes.starts_with(b"%PDF-") => Ok(IntakeKind::Pdf),
        "txt" if std::str::from_utf8(bytes).is_ok_and(|text| !text.trim().is_empty()) => Ok(IntakeKind::Txt),
        "docx" if bytes.starts_with(b"PK\x03\x04") => Ok(IntakeKind::Docx),
        ext if ocr::valid_image_source(ext, bytes, None) => Ok(IntakeKind::Photo),
        _ => Err("Формат вхідного файлу пошкоджено або він не підтримується.".into()),
    }
}

fn extension(name: &str) -> &str {
    Path::new(name).extension().and_then(|value| value.to_str()).unwrap_or_default()
}

fn validate_article_url(url: &str) -> Result<()> {
    let authority = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))
        .and_then(|rest| rest.split(['/', '?', '#']).next());
    if url.len() > 4096 || url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\')
        || authority.is_none_or(|host| host.is_empty() || host.contains('@'))
    {
        return Err("Некоректна адреса джерела статті.".into());
    }
    Ok(())
}

pub(super) fn validate_article_draft(draft: &ArticleDraft) -> Result<()> {
    text_name(&draft.title)?;
    validate_article_url(&draft.source_url)?;
    validate_text(&draft.text)
}

fn text_bytes(text: String) -> Result<Vec<u8>> {
    validate_text(&text)?;
    Ok(text.into_bytes())
}

fn validate_text(text: &str) -> Result<()> {
    if text.trim().is_empty() || text.len() > MAX_IMPORT {
        return Err("Потрібне непорожнє текстове джерело до 20 МіБ.".into());
    }
    Ok(())
}

fn text_name(name: &str) -> Result<String> {
    let name = safe_name(name, 4092)?;
    Ok(if name.to_ascii_lowercase().ends_with(".txt") { name.to_owned() } else { format!("{name}.txt") })
}

fn safe_name(name: &str, limit: usize) -> Result<&str> {
    Path::new(name).file_name().and_then(|name| name.to_str())
        .filter(|name| name.len() <= limit && !name.trim().is_empty() && !name.chars().any(char::is_control))
        .ok_or_else(|| "Некоректна назва вхідного матеріалу.".into())
}
