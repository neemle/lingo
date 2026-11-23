use crate::{translation::normalize_pdf_translation_input, Document, Result, Store};
use std::ops::Range;

impl Store {
    pub fn passage_source(&self, id: &str, page: Option<u16>, sentence_id: &str) -> Result<String> {
        let (_, text) = self.passage_translation_source(id, page, sentence_id)?;
        passage_chunks(&text)?;
        Ok(text)
    }

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

pub(crate) struct PassageChunk<'a> {
    pub(crate) text: &'a str,
    pub(crate) separator: &'static str,
}

pub(crate) fn passage_chunks(text: &str) -> Result<Vec<PassageChunk<'_>>> {
    let mut pieces = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        for (index, text) in chunks(line)?.into_iter().enumerate() {
            let separator = if pieces.is_empty() { "" } else if index == 0 { "\n" } else { " " };
            pieces.push(PassageChunk { text, separator });
        }
        if pieces.len() > 24 {
            return Err("Вибраний абзац задовгий для локального перекладу.".into());
        }
    }
    if pieces.is_empty() {
        return Err("У цьому абзаці немає тексту для перекладу.".into());
    }
    Ok(pieces)
}

pub(crate) fn join_translated_chunks(
    translated: impl IntoIterator<Item = impl AsRef<str>>,
    separators: impl IntoIterator<Item = &'static str>,
) -> String {
    let mut output = String::new();
    let mut separators = separators.into_iter();
    for text in translated {
        output.push_str(separators.next().unwrap_or(" "));
        output.push_str(text.as_ref());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passage_keeps_dialogue_and_heading_breaks_but_joins_wrapped_words() {
        let document = crate::document_layout::detached_pdf_document(
            "CHAPITRE I\nLe bateau transportait des marchan-\ndises. — Vous partez ? — Demain matin !".into(),
            "fr",
            None,
        );
        let text = normalize_passage(&document, 0..document.text.len());
        assert_eq!(text, "CHAPITRE I\nLe bateau transportait des marchandises.\n— Vous partez ?\n— Demain matin !");
        let chunks = passage_chunks(&text).unwrap();
        let translated = ["РОЗДІЛ I", "Корабель перевозив товари.", "— Ви їдете?", "— Завтра вранці!"];
        assert_eq!(join_translated_chunks(translated, chunks.iter().map(|chunk| chunk.separator)),
            translated.join("\n"));
    }
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
    let mut breaks = Vec::new();
    let mut previous = "";
    let mut offset = range.start;
    for line in document.text[range.clone()].split_inclusive('\n') {
        if offset > range.start && crate::document_layout::keep_pdf_sentence_break(previous, line.trim()) {
            breaks.push(offset);
        }
        previous = line.trim();
        offset += line.len();
    }
    let first = document.blocks.partition_point(|block| block.end_utf8 <= range.start);
    for pair in document.blocks[first..].windows(2) {
        let [left, right] = pair else { continue };
        if right.start_utf8 >= range.end { break; }
        if right.start_utf8 <= range.start || right.footnote { continue; }
        let before = document.text[left.start_utf8.max(range.start)..left.end_utf8].trim();
        let after = document.text[right.start_utf8..right.end_utf8.min(range.end)].trim();
        let heading = left.start_utf8 == range.start && before.chars().count() <= 48
            && before.chars().any(char::is_uppercase) && !before.chars().any(char::is_lowercase);
        if heading || crate::document_layout::keep_pdf_sentence_break(before, after) {
            breaks.push(right.start_utf8);
        }
    }
    breaks.sort_unstable();
    let mut start = range.start;
    let mut lines = Vec::new();
    for end in breaks.into_iter().chain([range.end]) {
        if end <= start { continue; }
        let line = normalize(start..end);
        if !line.trim().is_empty() {
            lines.push(line.trim().to_owned());
        }
        start = end;
    }
    lines.join("\n")
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
