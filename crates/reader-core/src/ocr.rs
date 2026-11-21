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
        let value = line.text.trim();
        if line.page < previous_page
            || line.page >= pdf_pages.unwrap_or(1)
            || value.is_empty()
            || value.len() > 8192
            || value.contains(['\0', '\r', '\n'])
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
