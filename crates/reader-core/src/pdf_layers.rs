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
        page.push_str(line.text.trim());
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

/// Upgrade an earlier import of the same bytes without dropping user annotations.
pub(crate) fn upgrade(
    document: &mut crate::Document,
    text: String,
    offsets: Vec<Option<usize>>,
    lines: Vec<OcrLine>,
    pages: u16,
) -> Result<()> {
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
            .ok_or(
                "Не вдалося безпечно перенести закладку або переклад зі старої версії PDF"
                    .to_owned(),
            )
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
                return Err("Не вдалося розрізнити переклади зі старої версії PDF".into());
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
    document.ocr_lines = Some(lines);
    document.ocr_pdf_pages = Some(pages);
    document.pdf_page_starts = Some(offsets);
    Ok(())
}
