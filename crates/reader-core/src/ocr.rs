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
}

impl OcrLine {
    pub fn reading_text(&self) -> &str {
        self.correction.as_deref().unwrap_or(&self.text).trim()
    }
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 8192 && !value.contains(['\0', '\r', '\n'])
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
    for line in lines {
        let unit = |value: f64| (0.0..=1.0).contains(&value);
        let value = line.reading_text();
        if line.page < previous_page
            || line.page >= pdf_pages.unwrap_or(1)
            || !valid_text(line.text.trim())
            || !valid_text(value)
            || ![line.x, line.y, line.width, line.height]
                .into_iter()
                .all(unit)
            || line.width <= 0.0
            || line.height <= 0.0
            || line.x + line.width > 1.000001
            || line.y + line.height > 1.000001
            || line.confidence.is_some_and(|confidence| !unit(confidence))
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
