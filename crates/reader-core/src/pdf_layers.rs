use std::path::Path;

use crate::{
    ocr::{recognized_text, OcrLine},
    Result, MAX_IMPORT,
};

pub fn text_pages(name: &str, bytes: &[u8], expected: u16) -> Result<Vec<String>> {
    if !(1..=40).contains(&expected)
        || bytes.len() > MAX_IMPORT
        || !bytes.starts_with(b"%PDF-")
        || !Path::new(name)
            .extension()
            .and_then(|suffix| suffix.to_str())
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case("pdf"))
    {
        return Err("Для розпізнавання потрібен PDF до 20 МБ і 40 сторінок".into());
    }

    let pages = pdf_extract::extract_text_from_mem_by_pages(bytes)
        .map_err(|error| format!("Не вдалося прочитати текстові шари PDF: {error}"))?;
    if pages.len() != usize::from(expected) {
        return Err("Не вдалося звірити сторінки PDF з текстовим шаром".into());
    }

    let mut result = Vec::with_capacity(pages.len());
    let mut total: usize = 0;
    for page in pages {
        let text =
            page.trim_matches(|c: char| c.is_whitespace() || c.is_control() || c == '\u{fffd}');
        total = total.checked_add(text.len()).ok_or("Текст PDF завеликий")?;
        if total > MAX_IMPORT {
            return Err("Текст PDF завеликий".into());
        }
        result.push(text.to_owned());
    }
    Ok(result)
}

pub fn merge(
    name: &str,
    bytes: &[u8],
    expected: u16,
    lines: &[OcrLine],
) -> Result<(String, Vec<Option<usize>>)> {
    let mut pages = text_pages(name, bytes, expected)?;
    if !lines.is_empty() {
        recognized_text(name, bytes, lines, Some(expected))?;
    }

    let mut prior = None;
    for line in lines {
        let page = &mut pages[usize::from(line.page)];
        if prior == Some(line.page) {
            page.push('\n');
        } else if !page.is_empty() {
            return Err(
                "Розпізнані рядки збігаються зі сторінкою, яка вже містить текстовий шар".into(),
            );
        }
        page.push_str(line.reading_text());
        prior = Some(line.page);
    }

    let mut text = String::new();
    let mut offsets = Vec::with_capacity(pages.len());
    let mut utf16 = 0;
    for page in pages {
        if page.is_empty() {
            offsets.push(None);
            continue;
        }
        if text.len() + page.len() + 2 > MAX_IMPORT {
            return Err("Текст PDF завеликий".into());
        }
        if !text.is_empty() {
            text.push_str("\n\n");
            utf16 += 2;
        }
        offsets.push(Some(utf16));
        utf16 += page.encode_utf16().count();
        text.push_str(&page);
    }
    if text.is_empty() {
        return Err("У PDF не знайдено тексту".into());
    }
    Ok((text, offsets))
}
