use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::{Result, MAX_IMPORT};

mod tesseract;
mod orientation;
mod quarter_turn;
pub use orientation::merge_rotated_lines;
pub use quarter_turn::rotate_ocr_png;
pub(crate) use orientation::reading_box;
pub use tesseract::{parse_tesseract_quarter_turn, parse_tesseract_tsv, recover_tesseract_lines};
pub(crate) use tesseract::merge_missing_lines;

/// Coordinates use the upright image, with the origin in its upper left corner.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OcrLine {
    #[serde(default, skip_serializing_if = "is_first_page")]
    pub page: u16,
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub block_start: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<OcrWord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_span: Option<OcrSpan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OcrWord {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_start_utf16: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_span: Option<OcrSpan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OcrSpan {
    pub start_utf8: usize,
    pub end_utf8: usize,
    pub start_utf16: usize,
    pub end_utf16: usize,
}

impl OcrLine {
    pub(crate) fn is_rotated(&self) -> bool {
        matches!(self.rotation, Some(90 | 180 | 270))
    }

    pub(crate) fn needs_proofreading(&self) -> bool {
        self.confidence.is_some_and(|value| value < 0.92)
    }

    pub fn reading_text(&self) -> &str {
        self.correction.as_deref().unwrap_or(&self.text).trim()
    }

    pub(crate) fn unchanged_word_starts(&self) -> Option<Vec<usize>> {
        use crate::document_tokens::word_parts;
        self.correction.as_ref()?;
        let raw: Vec<_> = word_parts(self.text.trim()).map(|(_, _, word)| word).collect();
        let reading: Vec<_> = word_parts(self.reading_text()).collect();
        if raw.len() != reading.len() {
            // A restored or removed token can shift the rest of the line. Only the
            // matching prefix and suffix have a clear correspondence in this case.
            let prefix = raw.iter().zip(&reading).take_while(|(raw, (_, _, current))| *raw == current).count();
            let suffix = raw[prefix..].iter().rev().zip(reading[prefix..].iter().rev())
                .take_while(|(raw, (_, _, current))| *raw == current).count();
            return Some(reading[..prefix].iter().chain(&reading[reading.len() - suffix..])
                .map(|(start, _, _)| *start).collect());
        }
        Some(raw.into_iter().zip(reading)
            .filter_map(|(raw, (start, _, current))| (raw == current).then_some(start))
            .collect())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OcrLineEdit {
    pub index: usize,
    pub source: String,
    pub replacement: String,
}

pub(crate) fn apply_reviewed_lines(lines: &mut [OcrLine], page: u16, edits: &[OcrLineEdit]) -> Result<bool> {
    let mut indices = std::collections::HashSet::new();
    if edits.is_empty() || edits.len() > 6 || edits.iter().any(|edit| {
        !indices.insert(edit.index) || edit.replacement.trim().is_empty() ||
            edit.replacement.len() > 1024 || edit.replacement.chars().any(char::is_control)
    }) {
        return Err("Некоректні виправлення OCR.".into());
    }
    for edit in edits {
        if !lines.get(edit.index).is_some_and(|line| line.page == page && line.reading_text() == edit.source) {
            return Err("Розпізнаний рядок змінився. Звірте сторінку знову.".into());
        }
    }
    for edit in edits {
        lines[edit.index].correction = Some(edit.replacement.trim().to_owned());
    }
    Ok(true)
}

impl crate::Store {
    pub fn reconcile_photo_ocr(&mut self, id: &str, revision: &str) -> Result<bool> {
        let document = self.translation_document(id)?;
        if document.format == "pdf" || document.ocr_lines.as_ref().is_none_or(Vec::is_empty) {
            return Err("Розпізнаний текст фото недоступний.".into());
        }
        if document.current_revision_id.as_deref() != Some(revision) {
            return Err("Текст фото змінився. Відкрийте оригінал знову.".into());
        }
        let language = document.language;
        let corpus = self.pdf_ocr_corpus(&language)?.ok_or(
            "Встановіть пакет аналізу цієї мови в налаштуваннях, щоб звірити текст фото.",
        )?;
        let mut changed = false;
        self.edit_document(id, |document, tx| {
            if document.current_revision_id.as_deref() != Some(revision) || document.language != language {
                return Err("Текст або мова фото змінилися. Відкрийте оригінал знову.".into());
            }
            let lines = document.ocr_lines.as_mut().ok_or("Розпізнаний текст фото недоступний.")?;
            let previous: Vec<_> = lines.iter().map(|line| line.reading_text().to_owned()).collect();
            let uncorrected = lines.iter_mut().filter(|line| line.correction.is_none());
            changed = Self::repair_pdf_ocr_with_corpus(&corpus, &language, uncorrected)?;
            if !changed {
                return Ok(false);
            }
            let edits = line_transcript_edits(lines, &previous)?;
            let parsed = crate::document_layout::from_ocr(recognized_lines(lines, 1)?, lines, None);
            let previous_revision = document.current_revision_id.clone();
            let before = update_transcript_with_edits(document, parsed.text, None, &edits)?;
            document.blocks = parsed.blocks;
            refresh_provenance(document);
            crate::document_revisions::append(document, tx)?;
            crate::dictionary_transfer::reanchor_canonical_selections(
                tx, previous_revision.as_deref(), document, &before, None, &edits,
            )?;
            Self::discard_changed_speech_resume(tx, document)?;
            Ok(true)
        })?;
        Ok(changed)
    }

    pub(crate) fn read_ocr_import(
        &self, name: &str, bytes: &[u8], language: &str, lines: &mut [OcrLine],
        pages: Option<u16>, mixed: bool,
    ) -> Result<crate::document_layout::Extracted> {
        use crate::document_layout;

        let parse = |lines: &[OcrLine]| -> Result<document_layout::Extracted> {
            match (pages, mixed) {
                (Some(pages), true) => {
                    let (text, offsets) = crate::pdf_layers::merge(name, bytes, pages, lines)?;
                    Ok(document_layout::from_transcript(text, Some(offsets)))
                }
                (_, false) => Ok(document_layout::from_ocr(
                    recognized_text(name, bytes, lines, pages)?, lines, pages,
                )),
                (None, true) => Err("Некоректні параметри імпорту PDF".into()),
            }
        };
        let original = parse(lines)?;
        if !lines.iter().any(|line| line.correction.is_none() && line.needs_proofreading()) {
            return Ok(original);
        }
        let Ok(Some(corpus)) = self.pdf_ocr_corpus(language) else { return Ok(original); };
        let mut revised = lines.to_vec();
        let uncorrected = revised.iter_mut().filter(|line| line.correction.is_none());
        if let Ok(true) = Self::repair_pdf_ocr_with_corpus(&corpus, language, uncorrected) {
            if let Ok(reading) = parse(&revised) {
                for (line, revised) in lines.iter_mut().zip(revised) {
                    line.correction = revised.correction;
                }
                return Ok(reading);
            }
        }
        Ok(original)
    }
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 8192 && !value.contains(['\0', '\r', '\n'])
}

fn unit(value: f64) -> bool {
    (0.0..=1.0).contains(&value)
}

fn valid_box(x: f64, y: f64, width: f64, height: f64) -> bool {
    [x, y, width, height].into_iter().all(unit)
        && width > 0.0
        && height > 0.0
        && x + width <= 1.000001
        && y + height <= 1.000001
}

fn is_first_page(page: &u16) -> bool {
    *page == 0
}

pub fn recognized_text(
    name: &str,
    bytes: &[u8],
    lines: &[OcrLine],
    pdf_pages: Option<u16>,
) -> Result<String> {
    if bytes.len() > MAX_IMPORT {
        return Err("Зображення завелике. Максимум — 20 МБ.".into());
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|part| part.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !valid_image_source(&extension, bytes, pdf_pages) {
        return Err("Формат зображення не підтримується або файл пошкоджений".into());
    }

    recognized_lines(lines, pdf_pages.unwrap_or(1))
}

pub(crate) fn valid_image_source(extension: &str, bytes: &[u8], pdf_pages: Option<u16>) -> bool {
    match (extension, pdf_pages) {
        ("pdf", Some(1..)) => bytes.starts_with(b"%PDF-"),
        ("png", None) => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        ("jpg" | "jpeg", None) => bytes.starts_with(b"\xff\xd8\xff"),
        ("webp", None) => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        ("heic" | "heif", None) => {
            bytes.get(4..8) == Some(b"ftyp")
                && bytes.get(8..32).is_some_and(|brands| {
                    brands.chunks_exact(4).any(|brand| {
                        matches!(
                            brand,
                            b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1"
                        )
                    })
                })
        }
        _ => false,
    }
}

pub(crate) fn recognized_lines(lines: &[OcrLine], pages: u16) -> Result<String> {
    if lines.is_empty() {
        return Err("На зображенні немає доступного тексту".into());
    }
    let mut text = String::new();
    let mut previous_page = 0;
    let mut line_count = 0;
    let mut word_count = 0;
    for line in lines {
        if line.page > previous_page {
            line_count = 0;
            word_count = 0;
        }
        let value = line.reading_text();
        line_count += 1;
        word_count += line.words.len();
        if line.page < previous_page
            || line.page >= pages
            || !valid_text(line.text.trim())
            || !valid_text(value)
            || !valid_box(line.x, line.y, line.width, line.height)
            || line.confidence.is_some_and(|confidence| !unit(confidence))
            || line.rotation.is_some_and(|rotation| !matches!(rotation, 0 | 90 | 180 | 270))
            || line_count > 8192
            || word_count > 65_536
            || line.words.len() > line.text.len()
            || line.words.iter().any(|word| {
                !valid_text(&word.text) || !valid_box(word.x, word.y, word.width, word.height)
            })
        {
            return Err("Некоректний рядок розпізнаного тексту".into());
        }
        if text.len() + value.len() + 2 > MAX_IMPORT {
            return Err("Розпізнаний текст завеликий".into());
        }
        if !text.is_empty() {
            text.push('\n');
            if line.page != previous_page || line.block_start {
                text.push('\n');
            }
        }
        text.push_str(value);
        previous_page = line.page;
    }
    Ok(text)
}

pub(crate) fn byte_at_utf16(text: &str, target: usize) -> Option<usize> {
    let mut remaining = target;
    for (offset, character) in text.char_indices() {
        if remaining == 0 {
            return Some(offset);
        }
        remaining = remaining.checked_sub(character.len_utf16())?;
    }
    (remaining == 0).then_some(text.len())
}

/// Derive canonical spans from the transcript. The OCR provider only supplies raw offsets.
pub(crate) fn refresh_provenance(document: &mut crate::Document) {
    let Some(lines) = document.ocr_lines.as_mut() else {
        return;
    };
    refresh_line_provenance(&document.text, lines, document.pdf_page_starts.as_deref());
    crate::document_layout::reflow_ocr_paragraphs(document);
}

pub(crate) fn refresh_line_provenance(
    canonical: &str, lines: &mut [OcrLine], page_starts: Option<&[Option<usize>]>,
) {
    let mut byte = 0;
    let mut utf16 = 0;
    let mut previous_page = None;
    let mut page_aligned = true;
    for line in lines {
        let new_page = previous_page != Some(line.page);
        let had_previous_page = previous_page.replace(line.page).is_some();
        if let Some(starts) = page_starts.filter(|_| new_page) {
            let page_start = starts.get(usize::from(line.page)).copied().flatten();
            let skip = page_start
                .and_then(|start| start.checked_sub(utf16))
                .and_then(|remaining| byte_at_utf16(&canonical[byte..], remaining));
            page_aligned = skip.is_some();
            if let (Some(start), Some(skip)) = (page_start, skip) {
                byte += skip;
                utf16 = start;
            }
        } else if had_previous_page && page_aligned {
            let separator = if new_page || line.block_start { "\n\n" } else { "\n" };
            page_aligned = canonical[byte..].starts_with(separator);
            if page_aligned {
                byte += separator.len();
                utf16 += separator.len();
            }
        }

        let raw = line.text.trim();
        let reading = line.correction.as_deref().unwrap_or(raw).trim();
        let aligned = page_aligned
            && canonical[byte..]
                .strip_prefix(reading)
                .is_some_and(|tail| tail.is_empty() || tail.starts_with('\n'));
        let span = aligned.then(|| OcrSpan {
            start_utf8: byte,
            end_utf8: byte + reading.len(),
            start_utf16: utf16,
            end_utf16: utf16 + reading.encode_utf16().count(),
        });
        line.source_span = span;
        if let Some(span) = span {
            byte = span.end_utf8;
            utf16 = span.end_utf16;
        } else {
            page_aligned = false;
        }

        // A correction can change one part of a line and shift the remaining words.
        // Keep only exact ranges in the common prefix or the non-overlapping suffix.
        let prefix: usize = raw
            .chars()
            .zip(reading.chars())
            .take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8())
            .sum();
        let suffix: usize = raw[prefix..]
            .chars()
            .rev()
            .zip(reading[prefix..].chars().rev())
            .take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8())
            .sum();
        let corrected_words = (raw != reading).then(|| corrected_word_ranges(raw, reading)).flatten();
        let mut raw_byte = 0;
        let mut raw_utf16 = 0;
        let mut reading_byte = 0;
        let mut reading_utf16 = 0;
        for word in &mut line.words {
            word.source_span = (|| {
                let start_utf16 = word.raw_start_utf16?;
                let skip = byte_at_utf16(&raw[raw_byte..], start_utf16.checked_sub(raw_utf16)?)?;
                let start = raw_byte + skip;
                let end = start.checked_add(word.text.len())?;
                if raw.get(start..end)? != word.text {
                    return None;
                }
                raw_byte = end;
                let length_utf16 = word.text.encode_utf16().count();
                raw_utf16 = start_utf16 + length_utf16;
                let span = span?;
                let corrected = corrected_words.as_ref().and_then(|ranges| {
                    let index = ranges.binary_search_by_key(&start, |(raw, _)| raw.start).ok()?;
                    (ranges[index].0.end == end).then(|| ranges[index].1.clone())
                });
                let reading_range = if let Some(corrected) = corrected {
                    corrected
                } else if end < prefix
                    || (end == prefix
                        && raw[end..].chars().next().is_none_or(char::is_whitespace)
                        && reading[end..]
                            .chars()
                            .next()
                            .is_none_or(char::is_whitespace))
                {
                    start..end
                } else if start > raw.len() - suffix
                    || (start == raw.len() - suffix
                        && raw[..start]
                            .chars()
                            .next_back()
                            .is_none_or(char::is_whitespace)
                        && reading[..reading.len() - suffix]
                            .chars()
                            .next_back()
                            .is_none_or(char::is_whitespace))
                {
                    reading.len() - (raw.len() - start)..reading.len() - (raw.len() - end)
                } else {
                    return None;
                };
                let reading_start = reading_range.start;
                let reading_end = reading_range.end;
                let start_utf16 = reading_utf16
                    + reading
                        .get(reading_byte..reading_start)?
                        .encode_utf16()
                        .count();
                reading_byte = reading_end;
                reading_utf16 = start_utf16 + reading[reading_range].encode_utf16().count();
                Some(OcrSpan {
                    start_utf8: span.start_utf8 + reading_start,
                    end_utf8: span.start_utf8 + reading_end,
                    start_utf16: span.start_utf16 + start_utf16,
                    end_utf16: span.start_utf16 + reading_utf16,
                })
            })();
        }
    }
}

fn corrected_word_ranges(raw: &str, reading: &str) -> Option<Vec<(std::ops::Range<usize>, std::ops::Range<usize>)>> {
    use crate::document_tokens::word_parts;
    let mut original = word_parts(raw);
    let mut corrected = word_parts(reading);
    let mut ranges = Vec::new();
    let (mut raw_end, mut reading_end) = (0, 0);
    loop {
        match (original.next(), corrected.next()) {
            (Some((start, _, word)), Some((reading_start, _, replacement))) => {
                if raw[raw_end..start] != reading[reading_end..reading_start]
                    || word != replacement && (!word.chars().all(char::is_alphabetic)
                        || !replacement.chars().all(char::is_alphabetic)
                        || !differs_by_one_glyph(word, replacement))
                {
                    return None;
                }
                raw_end = start + word.len();
                reading_end = reading_start + replacement.len();
                ranges.push((start..raw_end, reading_start..reading_end));
            }
            (None, None) => return (raw[raw_end..] == reading[reading_end..]).then_some(ranges),
            _ => return None,
        }
    }
}

pub(crate) fn differs_by_one_glyph(left: &str, right: &str) -> bool {
    let shared: usize = left.chars().zip(right.chars()).take_while(|(a, b)| a == b)
        .map(|(ch, _)| ch.len_utf8()).sum();
    let (left, right) = (&left[shared..], &right[shared..]);
    let left_tail = left.chars().next().map(|ch| &left[ch.len_utf8()..]);
    let right_tail = right.chars().next().map(|ch| &right[ch.len_utf8()..]);
    left_tail == Some(right) || right_tail == Some(left) ||
        left_tail.is_some_and(|tail| Some(tail) == right_tail)
}

/// Match unchanged sentences on the same physical page.
pub(crate) fn corresponding_pdf_sentences<'a>(
    from: &'a crate::Document,
    to_format: &str,
    to_starts: Option<&[Option<usize>]>,
    to_sentences: &'a [crate::Sentence],
) -> Option<std::collections::HashMap<&'a str, &'a str>> {
    let old_starts = from.pdf_page_starts.as_deref()?;
    let new_starts = to_starts?;
    if from.format != "pdf" || to_format != "pdf" || old_starts.len() != new_starts.len() {
        return None;
    }
    let matching = matching_pdf_sentence(old_starts, new_starts, &from.sentences, to_sentences);
    Some(from.sentences.iter().filter_map(|old| {
        let index = matching(old)?;
        Some((old.id.as_str(), to_sentences[index].id.as_str()))
    }).collect())
}

pub(crate) fn matching_pdf_sentence<'a>(
    old_starts: &[Option<usize>], new_starts: &'a [Option<usize>],
    before: &'a [crate::Sentence], after: &'a [crate::Sentence],
) -> impl Fn(&crate::Sentence) -> Option<usize> + 'a {
    let old_pages = page_anchors(old_starts);
    move |old| match_page_sentence(&old_pages, new_starts, before, after, old)
}

fn page_anchors(starts: &[Option<usize>]) -> Vec<(usize, usize)> {
    starts
        .iter()
        .enumerate()
        .filter_map(|(page, start)| start.map(|start| (start, page)))
        .collect()
}

fn match_page_sentence(
    old_pages: &[(usize, usize)],
    starts: &[Option<usize>],
    before: &[crate::Sentence],
    after: &[crate::Sentence],
    old: &crate::Sentence,
) -> Option<usize> {
    let old_page = old_pages.partition_point(|(start, _)| *start <= old.start_utf16).checked_sub(1)?;
    let (old_start, page) = old_pages[old_page];
    let page_start = starts.get(page).copied().flatten()?;
    let page_end = starts[page + 1..].iter().flatten().copied().next().unwrap_or(usize::MAX);
    let expected_start = page_start.checked_add(old.start_utf16 - old_start)?;
    if expected_start < page_end {
        if let Ok(index) = after.binary_search_by_key(&expected_start, |sentence| sentence.start_utf16) {
            if after[index].text == old.text { return Some(index); }
        }
    }

    // OCR can add or remove text earlier on this page. A moved sentence must
    // have a single exact match in each revision of this physical page.
    let old_end = old_pages.get(old_page + 1).map_or(usize::MAX, |(start, _)| *start);
    unique_page_sentence(before, old_start, old_end, &old.text)?;
    unique_page_sentence(after, page_start, page_end, &old.text)
}

fn unique_page_sentence(sentences: &[crate::Sentence], start: usize, end: usize, text: &str) -> Option<usize> {
    let first = sentences.partition_point(|sentence| sentence.start_utf16 < start);
    let last = sentences.partition_point(|sentence| sentence.start_utf16 < end);
    let mut matches = (first..last).filter(|&index| sentences[index].text == text);
    let index = matches.next()?;
    matches.next().is_none().then_some(index)
}

/// Rebuild derived sentences without dropping or guessing user annotations.
pub(crate) fn update_transcript(document: &mut crate::Document, text: String) -> Result<()> {
    update_transcript_with_page_starts(document, text, None)
}

pub(crate) fn update_transcript_with_page_starts(
    document: &mut crate::Document,
    text: String,
    starts: Option<&[Option<usize>]>,
) -> Result<()> {
    update_transcript_with_edits(document, text, starts, &[]).map(drop)
}

pub(crate) struct TranscriptEdit {
    pub start_utf16: usize,
    pub old_len: usize,
    pub new_len: usize,
}

pub(crate) fn line_transcript_edits(lines: &[OcrLine], previous: &[String]) -> Result<Vec<TranscriptEdit>> {
    let mut edits = Vec::new();
    for (line, old) in lines.iter().zip(previous.iter().map(String::as_str)) {
        let new = line.reading_text();
        if new == old {
            continue;
        }
        let span = line.source_span.ok_or("Розпізнаний рядок не має позиції в тексті.")?;
        edits.extend(word_replacement_edits(span.start_utf16, old, new)
            .unwrap_or_else(|| vec![TranscriptEdit {
                start_utf16: span.start_utf16,
                old_len: old.encode_utf16().count(),
                new_len: new.encode_utf16().count(),
            }]));
    }
    Ok(edits)
}

pub(crate) fn word_replacement_edits(start: usize, old: &str, new: &str) -> Option<Vec<TranscriptEdit>> {
    use crate::document_tokens::word_parts;
    let mut old_words = word_parts(old);
    let mut new_words = word_parts(new);
    let mut old_end = 0;
    let mut new_end = 0;
    let mut edits = Vec::new();
    loop {
        match (old_words.next(), new_words.next()) {
            (Some((old_byte, offset, raw)), Some((new_byte, _, corrected))) => {
                if old[old_end..old_byte] != new[new_end..new_byte] {
                    return None;
                }
                if raw != corrected {
                    edits.push(TranscriptEdit {
                        start_utf16: start + offset,
                        old_len: raw.encode_utf16().count(),
                        new_len: corrected.encode_utf16().count(),
                    });
                }
                old_end = old_byte + raw.len();
                new_end = new_byte + corrected.len();
            }
            (None, None) => return (old[old_end..] == new[new_end..]).then_some(edits),
            _ => return None,
        }
    }
}

pub(crate) fn map_transcript_offset(offset: usize, edits: &[TranscriptEdit]) -> Option<usize> {
    let mut mapped = offset;
    for edit in edits.iter().take_while(|edit| edit.start_utf16 < offset) {
        if offset < edit.start_utf16 + edit.old_len {
            return None;
        }
        mapped = mapped - edit.old_len + edit.new_len;
    }
    Some(mapped)
}

pub(crate) fn map_unchanged_range_start(start: usize, length: usize, edits: &[TranscriptEdit]) -> Option<usize> {
    let end = start.checked_add(length)?;
    let mut mapped = start;
    for edit in edits.iter().take_while(|edit| edit.start_utf16 < end) {
        if edit.start_utf16.checked_add(edit.old_len)? > start { return None; }
        mapped = mapped.checked_sub(edit.old_len)?.checked_add(edit.new_len)?;
    }
    Some(mapped)
}

pub(crate) fn update_transcript_with_edits(
    document: &mut crate::Document,
    text: String,
    starts: Option<&[Option<usize>]>,
    edits: &[TranscriptEdit],
) -> Result<Vec<crate::Sentence>> {
    use std::collections::HashMap;

    let sentences = crate::pdf_sentences::segment_document(document, &text);
    let old_pages = document
        .pdf_page_starts
        .as_deref()
        .map(page_anchors)
        .unwrap_or_default();
    let page_match = |old: &crate::Sentence| {
        match_page_sentence(&old_pages, starts?, &document.sentences, &sentences, old)
    };
    let by_id: HashMap<_, _> = sentences
        .iter()
        .enumerate()
        .map(|(index, sentence)| (sentence.id.as_str(), index))
        .collect();
    let mut by_text = HashMap::new();
    for (index, sentence) in sentences.iter().enumerate() {
        by_text
            .entry(sentence.text.as_str())
            .and_modify(|slot| *slot = None)
            .or_insert(Some(index));
    }
    let old_by_id: HashMap<_, _> = document
        .sentences
        .iter()
        .map(|sentence| (sentence.id.as_str(), sentence))
        .collect();
    let mut old_counts = HashMap::new();
    for sentence in &document.sentences {
        *old_counts.entry(sentence.text.as_str()).or_insert(0usize) += 1;
    }
    let matching_index = |old: &crate::Sentence| {
        if !edits.is_empty() {
            let start = map_transcript_offset(old.start_utf16, edits)?
                + edits.iter().filter(|edit| edit.start_utf16 == old.start_utf16 && edit.old_len == 0)
                    .map(|edit| edit.new_len).sum::<usize>();
            let end = map_transcript_offset(old.end_utf16, edits)?;
            let index = sentences.binary_search_by_key(&start, |sentence| sentence.start_utf16).ok()?;
            return (sentences[index].end_utf16 == end).then_some(index);
        }
        // Inserting an earlier page can reuse an old offset on a different page.
        if starts.is_some() && !old_pages.is_empty() {
            return page_match(old);
        }
        by_id
            .get(old.id.as_str())
            .copied()
            .filter(|&index| sentences[index].text == old.text)
            .or_else(|| {
                let key = old.text.as_str();
                (old_counts.get(key) == Some(&1))
                    .then(|| by_text.get(key).copied().flatten())
                    .flatten()
            })
    };
    let map_id = |old_id: &str| {
        old_by_id
            .get(old_id)
            .and_then(|old| matching_index(old))
            .map(|index| sentences[index].id.clone())
            .ok_or(
                "Зміна зачіпає речення із закладкою, нотаткою, збереженим перекладом або текстом озвучення".to_owned(),
            )
    };

    let mut bookmarks: Vec<_> = document
        .bookmarks
        .iter()
        .map(|id| map_id(id))
        .collect::<Result<_>>()?;
    bookmarks.sort_unstable();
    bookmarks.dedup();
    let mut notes = std::collections::BTreeMap::new();
    for (id, note) in &document.notes {
        if notes.insert(map_id(id)?, note.clone()).is_some() {
            return Err("Не вдалося однозначно перенести нотатки".into());
        }
    }
    let mut speech_text_overrides = std::collections::BTreeMap::new();
    for (id, text) in &document.speech_text_overrides {
        if speech_text_overrides
            .insert(map_id(id)?, text.clone())
            .is_some()
        {
            return Err("Не вдалося однозначно перенести текст озвучення".into());
        }
    }
    let mut translations = std::collections::BTreeMap::new();
    for (language, entries) in &document.reviewed_translations {
        let mut moved = std::collections::BTreeMap::new();
        for (id, translation) in entries {
            if moved.insert(map_id(id)?, translation.clone()).is_some() {
                return Err("Не вдалося однозначно перенести збережені переклади".into());
            }
        }
        translations.insert(language.clone(), moved);
    }
    let speech_resume = document.speech_resume.clone().and_then(|mut resume| {
        resume.sentence_id = map_id(&resume.sentence_id).ok()?;
        Some(resume)
    });
    let previous_sentence = document.sentences.get(document.position);
    let matched_position = previous_sentence.and_then(matching_index);
    let position = matched_position
        .unwrap_or(document.position.min(sentences.len().saturating_sub(1)));

    document.bookmarks = bookmarks;
    document.notes = notes;
    document.speech_text_overrides = speech_text_overrides;
    document.reviewed_translations = translations;
    if matched_position.is_none_or(|index| previous_sentence.is_some_and(|old| old.text != sentences[index].text)) {
        document.reading_token_utf16 = None;
    }
    document.position = position;
    document.text = text;
    let before = std::mem::replace(&mut document.sentences, sentences);
    document.speech_resume = speech_resume.filter(|resume| document.matches_speech_resume(resume));
    Ok(before)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn reviewed_page_rejects_the_entire_batch_if_one_line_changed() {
        let line = |text: &str, y| OcrLine {
            rotation: None,
            block_start: false,
            page: 0, text: text.into(), x: 0.1, y, width: 0.8, height: 0.04,
            confidence: Some(0.5), correction: None, words: vec![], source_span: None,
        };
        let mut store = crate::Store::open(Path::new(":memory:")).unwrap();
        let document = store.import_image(
            "scan.png", b"\x89PNG\r\n\x1a\n", "en",
            vec![line("A rnagic book.", 0.1), line("A rnagic story.", 0.2)],
        ).unwrap();
        let revision = document.current_revision_id.as_deref().unwrap();
        let corrections = |second: &str| vec![
            OcrLineEdit { index: 0, source: "A rnagic book.".into(), replacement: "A magic book.".into() },
            OcrLineEdit { index: 1, source: second.into(), replacement: "A magic story.".into() },
        ];
        assert!(store.apply_ocr_page_review(&document.id, 0, false, revision, corrections("A different line.")).is_err());
        assert_eq!(store.token_document(&document.id).unwrap().text, document.text);
        store.apply_ocr_page_review(&document.id, 0, false, revision, corrections("A rnagic story.")).unwrap();
        let corrected = store.token_document(&document.id).unwrap();
        assert!(corrected.text.contains("A magic book."));
        assert!(corrected.text.contains("A magic story."));
        assert_ne!(corrected.current_revision_id, document.current_revision_id);
        assert_eq!(corrected.ocr_lines.unwrap()[0].text, "A rnagic book.");
    }

    #[test]
    fn ocr_word_spans_track_scanned_pages_corrections_and_restored_provenance() {
        let line = |page, text: &str, words: &[(&str, usize)]| OcrLine {
            rotation: None,
            block_start: false,
            page,
            text: text.into(),
            x: 0.1,
            y: 0.1,
            width: 0.8,
            height: 0.1,
            confidence: None,
            correction: None,
            words: words
                .iter()
                .map(|(word, start)| OcrWord {
                    text: (*word).into(),
                    x: 0.1,
                    y: 0.1,
                    width: 0.1,
                    height: 0.1,
                    raw_start_utf16: Some(*start),
                    source_span: None,
                })
                .collect(),
            source_span: None,
        };
        let mut store = crate::Store::open(Path::new(":memory:")).unwrap();
        let document = store
            .import_scanned_pdf(
                "scan.pdf",
                b"%PDF-1.7",
                "uk",
                vec![
                    line(0, "📚 Море дім", &[("📚", 1), ("Море", 3), ("дім", 8)]),
                    line(2, "Ліс", &[("Ліс", 0)]),
                ],
                3,
            )
            .unwrap();
        let lines = document.ocr_lines.as_ref().unwrap();
        assert_eq!(lines[0].words[0].source_span, None);
        assert_eq!(lines[0].words[1].source_span.unwrap().start_utf8, 5);
        assert_eq!(lines[0].words[1].source_span.unwrap().start_utf16, 3);
        assert_eq!(lines[1].source_span.unwrap().start_utf16, 13);

        store
            .correct_ocr_line(&document.id, 0, Some("📚 Океан дім".into()))
            .unwrap();
        let mut backup = store.export().unwrap();
        let updated = &backup.snapshot.documents[0];
        let lines = updated.ocr_lines.as_ref().unwrap();
        assert_eq!(lines[0].words[1].source_span, None);
        assert_eq!(lines[0].words[1].text, "Море");
        for (word, offset) in [(&lines[0].words[2], 9), (&lines[1].words[0], 14)] {
            let span = word.source_span.unwrap();
            assert_eq!(span.start_utf16, offset);
            assert_eq!(&updated.text[span.start_utf8..span.end_utf8], word.text);
        }

        backup.snapshot.documents[0].ocr_lines.as_mut().unwrap()[1].words[0].source_span = None;
        let mut restored = crate::Store::open(Path::new(":memory:")).unwrap();
        restored.restore(backup).unwrap();
        let snapshot = restored.snapshot().unwrap();
        let lines = snapshot.documents[0].ocr_lines.as_ref().unwrap();
        assert_eq!(lines[1].words[0].source_span.unwrap().start_utf16, 14);
    }
}
