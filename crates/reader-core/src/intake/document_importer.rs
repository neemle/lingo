use super::application::{DocumentImporter, ImportMaterial};
use crate::{file_originals, language_detection, ocr, Result, Store};

impl DocumentImporter for Store {
    fn import_material(&mut self, material: ImportMaterial) -> Result<String> {
        let document = match material {
            ImportMaterial::Document { name, bytes, language } => {
                let language = choose_language(language, || language_detection::detect_document(&name, &bytes))?;
                self.import(&name, &bytes, &language)?
            }
            ImportMaterial::Article { name, bytes, url, language } => {
                let language = choose_language(language, || language_detection::detect_document(&name, &bytes))?;
                self.import_article(&name, &bytes, &language, &url)?
            }
            ImportMaterial::Photo { name, bytes, lines, language } => {
                let language = choose_language(language, || {
                    let sample = lines.iter().take(64).map(ocr::OcrLine::reading_text).collect::<Vec<_>>().join(" ");
                    language_detection::detect(&sample)
                })?;
                self.import_image(&name, &bytes, &language, lines)?
            }
            ImportMaterial::Pdf { name, path, page_texts, language } => {
                let prepared = file_originals::prepare_pdf_import(
                    &self.file_original_directory, &name, &path, &language, page_texts, Vec::new(),
                )?;
                self.import_pdf_file(prepared)?
            }
        };
        Ok(document.id)
    }
}

fn choose_language(
    selected: Option<String>, detect: impl FnOnce() -> Option<&'static str>,
) -> Result<String> {
    match selected {
        Some(language) => { crate::valid_language(&language)?; Ok(language) }
        None => detect().map(str::to_owned).ok_or_else(|| "Оберіть мову вхідного матеріалу.".into()),
    }
}

