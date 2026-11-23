use crate::{translation::normalize_pdf_translation_input, Document, Result, Store};
use std::ops::Range;

impl Store {
    pub(crate) fn passage_translation_source(
        &self, id: &str, page: Option<u16>, sentence_id: &str,
    ) -> Result<(String, String)> {
        let (language, text) = if let Some(page) = page {
            let cached = self
                .detached_pdf_sentences(id, page)?
                .ok_or("Текст сторінки PDF недоступний.")?;
            let sentence = cached
                .sentences
                .iter()
                .find(|sentence| sentence.id == sentence_id)
                .ok_or("Речення сторінки змінилося.")?;
            let document = crate::document_layout::detached_pdf_document(
                cached.text, &cached.language, self.detached_pdf_ocr(id, page)?.map(|cached| cached.lines),
            );
            if crate::document_layout::sentence_in_footnote(&document.blocks, sentence) {
                return Err("Виноски пропускаються під час перекладу.".into());
            }
            let range = document.paragraph_for_sentence(sentence);
            (cached.language, normalize_passage(&document, range))
        } else {
            let mut document = self.token_document(id)?;
            let sentence = document
                .sentences
                .iter()
                .find(|sentence| sentence.id == sentence_id)
                .ok_or("Речення документа змінилося.")?;
            let sentence = sentence.clone();
            crate::document_layout::reflow_ocr_paragraphs(&mut document);
            if crate::document_layout::sentence_in_footnote(&document.blocks, &sentence) {
                return Err("Виноски пропускаються під час перекладу.".into());
            }
            let range = document.paragraph_for_sentence(&sentence);
            let text = normalize_passage(&document, range);
            (document.language, text)
        };
        Ok((language, text))
    }
}

pub(crate) fn passage_chunks(text: &str) -> Result<Vec<&str>> {
    let mut pieces = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        pieces.extend(chunks(line)?);
    }
    if pieces.is_empty() {
        return Err("У цьому абзаці немає тексту для перекладу.".into());
    }
    Ok(pieces)
}

fn normalize_passage(document: &Document, range: Range<usize>) -> String {
    let normalize = |span: Range<usize>| {
        normalize_pdf_translation_input(
            document,
            &document.text[span.clone()],
            span.start,
            document.text[..span.start].encode_utf16().count(),
        )
    };
    if let Some(first) = document
        .blocks
        .iter()
        .find(|block| block.start_utf8 == range.start)
    {
        let title = &document.text[first.start_utf8..first.end_utf8];
        if first.end_utf8 < range.end
            && title.chars().count() <= 48
            && title.chars().any(char::is_uppercase)
            && !title.chars().any(char::is_lowercase)
        {
            return format!(
                "{}\n{}",
                normalize(first.start_utf8..first.end_utf8),
                normalize(first.end_utf8..range.end).trim_start()
            );
        }
    }
    normalize(range).trim().to_owned()
}

fn chunks(text: &str) -> Result<Vec<&str>> {
    const MAX_CHARS: usize = 260;
    const MIN_CHARS: usize = 100;
    let mut result = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        if result.len() == 24 {
            return Err("Вибраний абзац задовгий для локальної моделі.".into());
        }
        let end = rest
            .char_indices()
            .nth(MAX_CHARS)
            .map_or(rest.len(), |(offset, _)| offset);
        let prefix = &rest[..end];
        let cut = if end == rest.len() {
            end
        } else {
            let minimum = prefix
                .char_indices()
                .nth(MIN_CHARS)
                .map_or(0, |(offset, _)| offset);
            prefix
                .rmatch_indices(['.', '!', '?', '…'])
                .next()
                .map(|(offset, ch)| offset + ch.len())
                .filter(|&offset| offset >= minimum)
                .or_else(|| prefix.rfind(char::is_whitespace))
                .filter(|&offset| offset > 0)
                .unwrap_or(end)
        };
        result.push(rest[..cut].trim());
        rest = rest[cut..].trim_start();
    }

    if result.is_empty() {
        return Err("У цьому абзаці немає тексту для перекладу.".into());
    }
    Ok(result)
}
