use std::path::Path;

use crate::{
    ocr::{recognized_lines, OcrLine},
    Result, MAX_IMPORT,
};

pub fn text_pages(name: &str, bytes: &[u8], expected: u16) -> Result<Vec<String>> {
    if expected == 0
        || bytes.len() > MAX_IMPORT
        || !bytes.starts_with(b"%PDF-")
        || !Path::new(name)
            .extension()
            .and_then(|suffix| suffix.to_str())
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case("pdf"))
    {
        return Err("Для розпізнавання потрібен PDF до 20 МБ із принаймні однією сторінкою".into());
    }

    crate::document_layout::pdf_text_pages(bytes, expected)
}

pub fn merge(
    name: &str,
    bytes: &[u8],
    expected: u16,
    lines: &[OcrLine],
) -> Result<(String, Vec<Option<usize>>)> {
    merge_page_text(text_pages(name, bytes, expected)?, lines)
}

pub(crate) fn merge_page_text(
    mut pages: Vec<String>,
    lines: &[OcrLine],
) -> Result<(String, Vec<Option<usize>>)> {
    let page_count = u16::try_from(pages.len()).map_err(|_| "PDF містить забагато сторінок")?;
    if page_count == 0 {
        return Err("У PDF немає сторінок".into());
    }
    if !lines.is_empty() {
        recognized_lines(lines, page_count)?;
    }
    for page in &mut pages {
        if page.contains('\0') {
            return Err("Текст PDF містить некоректний символ".into());
        }
        page.truncate(page.trim_end().len());
        let prefix = page.len() - page.trim_start().len();
        page.drain(..prefix);
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
