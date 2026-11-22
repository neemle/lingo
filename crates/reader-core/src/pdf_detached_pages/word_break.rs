use super::{annotations, database_error, page_revision, read, sentence_prefix};
use crate::{
    document_tokens::word_parts, ocr, pdf_reading_position, translation::word_hyphen,
    Document, Result, Sentence, Store,
};
use rusqlite::{params, Connection, OptionalExtension};
use unicode_segmentation::UnicodeSegmentation;
use std::{borrow::Cow, ops::Range};

const MAX_BOUNDARY_BYTES: usize = 1024;

#[derive(Clone, Copy)]
enum Neighbor {
    Previous,
    Next,
}

#[derive(Clone, Copy)]
enum EdgeUse<'a> {
    Lookup(Option<&'a str>),
    Speech,
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
            &store.connection,
            id,
            page,
            EdgeUse::Lookup(target),
            Neighbor::Previous,
            first.text.len(),
        )?,
        _ => None,
    };
    let next = match (page.checked_add(1), last) {
        (Some(page), Some(last)) if crate::translation::word_hyphen(&last.text).is_some() => {
            excerpt(&store.connection, id, page, EdgeUse::Lookup(target), Neighbor::Next, last.text.len())?
        }
        _ => None,
    };
    Ok((previous, next))
}

/// Resolve the detached neighbor of a canonical PDF sentence at a physical page edge.
pub(crate) fn canonical_fragments(
    store: &Store, document: &Document, sentence: &Sentence,
) -> Result<(Option<String>, Option<String>)> {
    canonical_edge_fragments(store, document, None, Some(sentence), Some(sentence))
}

pub(crate) fn canonical_edge_fragments(
    store: &Store, document: &Document, target: Option<&str>,
    first: Option<&Sentence>, last: Option<&Sentence>,
) -> Result<(Option<String>, Option<String>)> {
    canonical_edges(&store.connection, document, EdgeUse::Lookup(target), first, last)
}

fn canonical_edges(
    db: &Connection, document: &Document, usage: EdgeUse<'_>,
    first: Option<&Sentence>, last: Option<&Sentence>,
) -> Result<(Option<String>, Option<String>)> {
    let Some(starts) = &document.pdf_page_starts else { return Ok((None, None)); };
    let page_at = |offset| starts.iter().rposition(|start| start.is_some_and(|start| start <= offset));
    let previous = first.filter(|first| first.text.starts_with(char::is_lowercase))
        .and_then(|first| {
            let page = page_at(first.start_utf16)?;
            let previous = page.checked_sub(1)?;
            let start = ocr::byte_at_utf16(&document.text, starts[page]?)?;
            if starts[previous].is_some() || !document.text.get(start..first.start_utf8)?.trim().is_empty() {
                return None;
            }
            Some((u16::try_from(previous).ok()?, first.text.len()))
        });
    let next = last.filter(|last| word_hyphen(&last.text).is_some()).and_then(|last| {
        let page = page_at(last.end_utf16.checked_sub(1)?)?;
        let next = page + 1;
        if starts.get(next)?.is_some() { return None; }
        let (_, end) = pdf_reading_position::page_bounds(document, u16::try_from(page).ok()?)?;
        let end = ocr::byte_at_utf16(&document.text, end)?;
        if !document.text.get(last.end_utf8..end)?.trim().is_empty() { return None; }
        Some((u16::try_from(next).ok()?, last.text.len()))
    });
    let previous = previous.map(|(page, bytes)| excerpt(
        db, &document.id, page, usage, Neighbor::Previous, bytes,
    )).transpose()?.flatten();
    let next = next.map(|(page, bytes)| excerpt(
        db, &document.id, page, usage, Neighbor::Next, bytes,
    )).transpose()?.flatten();
    Ok((previous, next))
}

// A word spoken across pages belongs to the page where it starts.
pub(super) fn narration(
    db: &Connection, id: &str, page: u16, sentences: &[Sentence], position: usize,
) -> Result<Option<String>> {
    let source = &sentences[position].text;
    let previous = if position == 0 && source.starts_with(char::is_lowercase) {
        page.checked_sub(1).map(|page| excerpt(
            db, id, page, EdgeUse::Speech, Neighbor::Previous, source.len(),
        )).transpose()?.flatten()
    } else { None };
    let next = if position == sentences.len() - 1 && word_hyphen(source).is_some() {
        page.checked_add(1).map(|page| excerpt(
            db, id, page, EdgeUse::Speech, Neighbor::Next, source.len(),
        )).transpose()?.flatten()
    } else { None };
    Ok(narration_fragments(source, previous.as_deref(), next.as_deref()))
}

pub(crate) fn canonical_narration(
    db: &Connection, document: &Document, sentence: &Sentence,
) -> Result<Option<String>> {
    if document.speech_text_overrides.contains_key(&sentence.id) {
        return Ok(None);
    }
    let (previous, next) = canonical_edges(db, document, EdgeUse::Speech, Some(sentence), Some(sentence))?;
    Ok(narration_fragments(&sentence.text, previous.as_deref(), next.as_deref()))
}

fn narration_fragments(source: &str, previous: Option<&str>, next: Option<&str>) -> Option<String> {
    let mut text = Cow::Borrowed(source);
    if let Some(consumed) = continuation(source, previous.and_then(word_hyphen)) {
        text = Cow::Borrowed(source[consumed.len()..].trim_start());
    }
    if let (Some(next), Some(prefix)) = (next, word_hyphen(&text)) {
        if let Some(added) = continuation(next, Some(prefix)) {
            text = Cow::Owned(format!("{prefix}{added}"));
        }
    }
    (text != source).then(|| text.into_owned())
}

fn continuation<'a>(text: &'a str, prefix: Option<&str>) -> Option<&'a str> {
    let prefix = prefix?;
    let (_, _, first_word) = word_parts(text).next()?;
    if !text.starts_with(first_word) || !first_word.starts_with(char::is_lowercase)
        || crate::validate_speech_text(prefix).is_err()
        || crate::validate_speech_text(text).is_err()
        || prefix.len() + text.len() > 1000
    {
        return None;
    }
    let end = first_word.len();
    let punctuation: usize = text[end..].chars()
        .take_while(|c| matches!(c, '.' | ',' | '!' | '?' | ':' | ';' | '…' | '»' | ')' | '”'))
        .map(char::len_utf8).sum();
    Some(&text[..end + punctuation])
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
    project_fragments(sentence, start_utf16, selected, previous.as_deref(), next.as_deref())
}

pub(crate) fn project_canonical_selection<'a>(
    store: &Store, document: &Document, sentence: &'a Sentence, start_utf16: usize, selected: &'a str,
) -> Result<ProjectedSelection<'a>> {
    let (previous, next) = canonical_fragments(store, document, sentence)?;
    project_fragments(sentence, start_utf16, selected, previous.as_deref(), next.as_deref())
}

fn project_fragments<'a>(
    sentence: &'a Sentence, start_utf16: usize, selected: &'a str, previous: Option<&str>, next: Option<&str>,
) -> Result<ProjectedSelection<'a>> {
    let trimmed = selected.trim_start();
    let leading = selected[..selected.len() - trimmed.len()].encode_utf16().count();
    let local = start_utf16 + leading - sentence.start_utf16;
    let start_utf8 = ocr::byte_at_utf16(&sentence.text, local)
        .ok_or("Некоректна позиція виділення.")?;
    let Some(joined) = join_fragments(&sentence.text, previous, next) else {
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
    db: &Connection,
    id: &str,
    page: u16,
    usage: EdgeUse<'_>,
    side: Neighbor,
    center_bytes: usize,
) -> Result<Option<String>> {
    let overhead = match usage { EdgeUse::Lookup(_) => 2, EdgeUse::Speech => 0 };
    let budget = MAX_BOUNDARY_BYTES.saturating_sub(center_bytes.saturating_add(overhead));
    if budget == 0 {
        return Ok(None);
    }
    if let Some((text, _)) = read(db, id, page)? {
        let Some((start, part)) = edge(&text, side, budget) else {
            return Ok(None);
        };
        let offset = text[..start].encode_utf16().count();
        match usage {
            EdgeUse::Lookup(Some(target)) if annotations::reviewed(db, id, page, offset)?.contains_key(target) => return Ok(None),
            EdgeUse::Speech if annotations::speech_text(db, id, page, offset)?.is_some() => return Ok(None),
            _ => {}
        }
        return Ok(Some(part.to_owned()));
    }

    // Read the canonical body only when the immediate neighbor has a text span.
    let path = format!("$.pdfPageStarts[{page}]");
    let body: Option<String> = db.query_row(
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
    let absolute = start_byte + relative;
    let index = document.sentences.partition_point(|sentence| sentence.end_utf8 <= absolute);
    if let Some(sentence) = document.sentences.get(index) {
        let has_override = match usage {
            EdgeUse::Lookup(Some(target)) => document.reviewed_translations.get(target)
                .is_some_and(|items| items.contains_key(&sentence.id)),
            EdgeUse::Speech => document.speech_text_overrides.contains_key(&sentence.id),
            EdgeUse::Lookup(None) => false,
        };
        if has_override {
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
