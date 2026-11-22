use super::{annotations, database_error, page_revision, read, sentence_prefix};
use crate::{ocr, pdf_reading_position, Document, Result, Sentence, Store};
use rusqlite::{params, OptionalExtension};
use unicode_segmentation::UnicodeSegmentation;

const MAX_BOUNDARY_BYTES: usize = 1024;

#[derive(Clone, Copy)]
enum Neighbor {
    Previous,
    Next,
}

pub(crate) fn neighboring_fragments(
    store: &Store,
    id: &str,
    page: u16,
    target: Option<&str>,
    first: Option<&Sentence>,
    last: Option<&Sentence>,
) -> Result<(Option<String>, Option<String>)> {
    let previous = match (page.checked_sub(1), first) {
        (Some(page), Some(first)) if first.text.starts_with(char::is_lowercase) => excerpt(
            store,
            id,
            page,
            target,
            Neighbor::Previous,
            first.text.len(),
        )?,
        _ => None,
    };
    let next = match (page.checked_add(1), last) {
        (Some(page), Some(last)) if crate::translation::word_hyphen(&last.text).is_some() => {
            excerpt(store, id, page, target, Neighbor::Next, last.text.len())?
        }
        _ => None,
    };
    Ok((previous, next))
}

fn excerpt(
    store: &Store,
    id: &str,
    page: u16,
    target: Option<&str>,
    side: Neighbor,
    center_bytes: usize,
) -> Result<Option<String>> {
    let budget = MAX_BOUNDARY_BYTES.saturating_sub(center_bytes.saturating_add(2));
    if budget == 0 {
        return Ok(None);
    }
    if let Some((text, _)) = read(&store.connection, id, page)? {
        let Some((start, part)) = edge(&text, side, budget) else {
            return Ok(None);
        };
        if let Some(target) = target {
            let start_utf16 = text[..start].encode_utf16().count();
            if annotations::reviewed(&store.connection, id, page, start_utf16)?.contains_key(target) {
                return Ok(None);
            }
        }
        return Ok(Some(part.to_owned()));
    }

    // Read the canonical body only when the immediate neighbor has a text span.
    let path = format!("$.pdfPageStarts[{page}]");
    let body: Option<String> = store
        .connection
        .query_row(
            "SELECT body FROM documents WHERE id=?1 AND json_type(body,?2)='integer'",
            params![id, path],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    let Some(body) = body else {
        return Ok(None);
    };
    let document: Document = serde_json::from_str(&body).map_err(database_error)?;
    let Some((start, end)) = pdf_reading_position::page_bounds(&document, page) else {
        return Ok(None);
    };
    let Some(start_byte) = ocr::byte_at_utf16(&document.text, start) else {
        return Ok(None);
    };
    let Some(end_byte) = ocr::byte_at_utf16(&document.text, end) else {
        return Ok(None);
    };
    let Some((relative, part)) = document
        .text
        .get(start_byte..end_byte)
        .and_then(|text| edge(text, side, budget))
    else {
        return Ok(None);
    };
    if let Some(items) = target.and_then(|target| document.reviewed_translations.get(target)) {
        let absolute = start_byte + relative;
        let index = document
            .sentences
            .partition_point(|sentence| sentence.end_utf8 <= absolute);
        if document.sentences.get(index).is_some_and(|sentence| items.contains_key(&sentence.id)) {
            return Ok(None);
        }
    }
    Ok(Some(part.to_owned()))
}

fn edge(text: &str, side: Neighbor, budget: usize) -> Option<(usize, &str)> {
    let mut edges = text
        .split_sentence_bound_indices()
        .filter_map(|(start, part)| {
            let trimmed = part.trim();
            (!trimmed.is_empty()).then_some((start + part.len() - part.trim_start().len(), trimmed))
        });
    let (start, part) = match side {
        Neighbor::Previous => edges.last(),
        Neighbor::Next => edges.next(),
    }?;
    let continues = match side {
        Neighbor::Previous => crate::translation::word_hyphen(part).is_some(),
        Neighbor::Next => part.starts_with(char::is_lowercase),
    };
    (continues && part.len() <= budget).then_some((start, part))
}

pub(super) fn boundary_sentence(text: &str, id: &str, page: u16) -> Sentence {
    Sentence {
        id: format!(
            "{}:boundary",
            sentence_prefix(id, page, &page_revision(text))
        ),
        text: text.to_owned(),
        start_utf8: 0,
        end_utf8: text.len(),
        start_utf16: 0,
        end_utf16: text.encode_utf16().count(),
    }
}
