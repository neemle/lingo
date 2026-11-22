use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::{Result, MAX_IMPORT};

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
    pub correction: Option<String>,
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
    pub fn reading_text(&self) -> &str {
        self.correction.as_deref().unwrap_or(&self.text).trim()
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
    let valid = match (extension.as_str(), pdf_pages) {
        ("pdf", Some(1..=40)) => bytes.starts_with(b"%PDF-"),
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
    };
    if !valid {
        return Err("Формат зображення не підтримується або файл пошкоджений".into());
    }

    if lines.is_empty() || lines.len() > 8192 {
        return Err("На зображенні немає доступного тексту або забагато рядків".into());
    }
    let mut text = String::new();
    let mut previous_page = 0;
    let mut word_count = 0;
    for line in lines {
        let value = line.reading_text();
        word_count += line.words.len();
        if line.page < previous_page
            || line.page >= pdf_pages.unwrap_or(1)
            || !valid_text(line.text.trim())
            || !valid_text(value)
            || !valid_box(line.x, line.y, line.width, line.height)
            || line.confidence.is_some_and(|confidence| !unit(confidence))
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
            if line.page != previous_page {
                text.push('\n');
            }
        }
        text.push_str(value);
        previous_page = line.page;
    }
    Ok(text)
}

fn byte_at_utf16(text: &str, target: usize) -> Option<usize> {
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
    let canonical = &document.text;
    let mut byte = 0;
    let mut utf16 = 0;
    let mut previous_page = None;
    let mut page_aligned = true;
    for line in lines {
        let new_page = previous_page != Some(line.page);
        let had_previous_page = previous_page.replace(line.page).is_some();
        if let Some(starts) = document.pdf_page_starts.as_ref().filter(|_| new_page) {
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
            let separator = if new_page { "\n\n" } else { "\n" };
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
                let reading_start = if end < prefix
                    || (end == prefix
                        && raw[end..].chars().next().is_none_or(char::is_whitespace)
                        && reading[end..]
                            .chars()
                            .next()
                            .is_none_or(char::is_whitespace))
                {
                    start
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
                    reading.len() - (raw.len() - start)
                } else {
                    return None;
                };
                let reading_end = reading_start + word.text.len();
                let start_utf16 = reading_utf16
                    + reading
                        .get(reading_byte..reading_start)?
                        .encode_utf16()
                        .count();
                reading_byte = reading_end;
                reading_utf16 = start_utf16 + length_utf16;
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

/// Rebuild derived sentences without dropping or guessing user annotations.
pub(crate) fn update_transcript(document: &mut crate::Document, text: String) -> Result<()> {
    use std::collections::HashMap;

    let sentences = crate::segment(&text, &document.id);
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
            .ok_or("Зміна зачіпає речення із закладкою або збереженим перекладом".to_owned())
    };

    let mut bookmarks: Vec<_> = document
        .bookmarks
        .iter()
        .map(|id| map_id(id))
        .collect::<Result<_>>()?;
    bookmarks.sort_unstable();
    bookmarks.dedup();
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
    let position = document
        .sentences
        .get(document.position)
        .and_then(matching_index)
        .unwrap_or(document.position.min(sentences.len().saturating_sub(1)));

    document.bookmarks = bookmarks;
    document.reviewed_translations = translations;
    document.position = position;
    document.text = text;
    document.sentences = sentences;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn ocr_word_spans_track_scanned_pages_corrections_and_restored_provenance() {
        let line = |page, text: &str, words: &[(&str, usize)]| OcrLine {
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
