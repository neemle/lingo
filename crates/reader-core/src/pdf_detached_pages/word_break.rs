use super::{annotations, database_error, page_revision, read, sentence_prefix};
use crate::{
    document_tokens::word_parts, ocr, pdf_reading_position, translation::word_hyphen,
    Document, Result, Sentence, Store,
};
use rusqlite::{params, OptionalExtension};
use unicode_segmentation::UnicodeSegmentation;
use std::{borrow::Cow, ops::Range};

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

pub(crate) struct JoinedBoundary {
    pub text: String,
    pub center: Range<usize>,
}

pub(crate) fn join_fragments(
    original: &str, previous: Option<&str>, next: Option<&str>,
) -> Option<JoinedBoundary> {
    let prefix = previous.and_then(word_hyphen).unwrap_or_default();
    let (center, suffix) = next
        .and_then(|suffix| word_hyphen(original).map(|center| (center, suffix)))
        .unwrap_or((original, ""));
    (!prefix.is_empty() || !suffix.is_empty()).then(|| JoinedBoundary {
        text: format!("{prefix}{center}{suffix}"),
        center: prefix.len()..prefix.len() + center.len(),
    })
}

pub(crate) struct ProjectedSelection<'a> {
    pub text: Cow<'a, str>,
    pub context: Cow<'a, str>,
    pub start_utf8: usize,
}

pub(crate) fn project_selection<'a>(
    store: &Store, location: (&str, u16), sentences: &[Sentence], sentence: &'a Sentence,
    start_utf16: usize, selected: &'a str,
) -> Result<ProjectedSelection<'a>> {
    let (id, page) = location;
    let (previous, next) = neighboring_fragments(
        store, id, page, None, sentences.first().filter(|first| first.id == sentence.id),
        sentences.last().filter(|last| last.id == sentence.id),
    )?;
    let trimmed = selected.trim_start();
    let leading = selected[..selected.len() - trimmed.len()].encode_utf16().count();
    let local = start_utf16 + leading - sentence.start_utf16;
    let start_utf8 = ocr::byte_at_utf16(&sentence.text, local)
        .ok_or("Некоректна позиція виділення.")?;
    let Some(joined) = join_fragments(&sentence.text, previous.as_deref(), next.as_deref()) else {
        return Ok(ProjectedSelection {
            text: trimmed.trim_end().into(), context: sentence.text.as_str().into(), start_utf8,
        });
    };
    let start = start_utf8 + joined.center.start;
    let end = (start + trimmed.trim_end().len()).min(joined.center.end);
    let mut extent = start..end;
    for (offset, _, word) in word_parts(&joined.text) {
        let word_end = offset + word.len();
        if offset < end && word_end > start
            && (offset < joined.center.start || word_end > joined.center.end)
        {
            extent.start = extent.start.min(offset);
            extent.end = extent.end.max(word_end);
        }
    }
    Ok(ProjectedSelection {
        start_utf8: extent.start,
        text: joined.text[extent].to_owned().into(),
        context: joined.text.into(),
    })
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
