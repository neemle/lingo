use crate::{Document, Result, Sentence, SpeechResume};
use rusqlite::{params, Connection, TransactionBehavior};

pub(crate) fn segment(text: &str, id: &str, soft_lines: bool) -> Vec<Sentence> {
    if soft_lines {
        crate::segment_pdf(text, id)
    } else {
        crate::segment(text, id)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Layout {
    Literal = 0,
    SoftLines = 1,
    Structured = 2,
    Titles = 3,
    ClosingQuotes = 4,
    PageReferences = 5,
    ListItems = 6,
}

impl Layout {
    pub(crate) fn from_storage(value: u8) -> Self {
        match value {
            6 => Self::ListItems,
            5 => Self::PageReferences,
            4 => Self::ClosingQuotes,
            3 => Self::Titles,
            2 => Self::Structured,
            1 => Self::SoftLines,
            _ => Self::Literal,
        }
    }

    pub(crate) fn segment(self, text: &str, id: &str) -> Vec<Sentence> {
        match self {
            Self::ListItems => attach_closing_quotes(text, id, crate::segment_pdf_with_list_items(text, id)),
            Self::PageReferences => attach_closing_quotes(text, id, crate::segment_pdf_with_references(text, id)),
            Self::ClosingQuotes => attach_closing_quotes(text, id, crate::segment_pdf_with_titles(text, id)),
            Self::Titles => crate::segment_pdf_with_titles(text, id),
            Self::Structured => crate::segment_pdf_structured(text, id),
            Self::SoftLines => crate::segment_pdf(text, id),
            Self::Literal => crate::segment(text, id),
        }
    }
}

pub(crate) fn segment_document(document: &Document, text: &str) -> Vec<Sentence> {
    if (document.format == "pdf" && document.pdf_list_item_boundaries)
        || (document.ocr_lines.is_some() && document.photo_list_item_boundaries)
    {
        Layout::ListItems.segment(text, &document.id)
    } else if document.format == "pdf" && document.pdf_reference_boundaries {
        Layout::PageReferences.segment(text, &document.id)
    } else if (document.format == "pdf" && document.pdf_quote_boundaries) ||
        (document.format != "pdf" && document.ocr_lines.is_some())
    {
        Layout::ClosingQuotes.segment(text, &document.id)
    } else if document.format == "pdf" && document.pdf_abbreviation_boundaries {
        crate::segment_pdf_with_titles(text, &document.id)
    } else if document.format == "pdf" && document.pdf_structured_sentences {
        crate::segment_pdf_structured(text, &document.id)
    } else if matches!(document.format.as_str(), "txt" | "docx") && document.prose_sentence_boundaries {
        segment_prose(text, &document.id)
    } else {
        segment(text, &document.id, document.pdf_soft_lines)
    }
}

pub(crate) fn segment_prose(text: &str, id: &str) -> Vec<Sentence> {
    let mut boundaries = text.as_bytes().to_vec();
    mask_title_periods(&mut boundaries);
    let boundaries = std::str::from_utf8(&boundaries).expect("Masking ASCII periods preserves UTF-8");
    attach_closing_quotes(text, id, crate::segment_with_boundaries(text, boundaries, id))
}

pub(crate) fn position_at(sentences: &[Sentence], offset: usize) -> usize {
    sentences
        .partition_point(|sentence| sentence.end_utf16 <= offset)
        .min(sentences.len().saturating_sub(1))
}

/// The legacy sentence remains intact if changing it would alter a saved translation or annotation.
pub(crate) fn upgrade_document(document: &mut Document) -> bool {
    if document.format != "pdf" || document.pdf_quote_boundaries {
        return false;
    }
    let candidates = [Layout::ClosingQuotes, Layout::Titles, Layout::Structured, Layout::SoftLines];
    let previous = if document.pdf_abbreviation_boundaries { Layout::Titles }
        else if document.pdf_structured_sentences { Layout::Structured }
        else if document.pdf_soft_lines { Layout::SoftLines } else { Layout::Literal };
    let Some((layout, sentences)) = candidates.into_iter()
        .take_while(|layout| *layout as u8 > previous as u8)
        .map(|layout| (layout, layout.segment(&document.text, &document.id)))
        .find(|(_, sentences)| preserves_anchors(document, sentences))
    else { return false; };

    if let Some(previous) = document.sentences.get(document.position) {
        let cursor = document.reading_token_utf16.unwrap_or_else(|| {
            let speech = previous.text.trim_start_matches(['»', '”', '›', '」', '』']).trim_start();
            previous.text[..previous.text.len() - speech.len()].encode_utf16().count()
        });
        let offset = previous.start_utf16 + cursor;
        document.position = position_at(&sentences, offset);
        document.reading_token_utf16 = document.reading_token_utf16.and_then(|_| {
            let new = sentences.get(document.position)?;
            (new.start_utf16 <= offset && offset < new.end_utf16).then(|| offset - new.start_utf16)
        });
    }
    document.sentences = sentences;
    document.pdf_soft_lines = true;
    document.pdf_structured_sentences = layout != Layout::SoftLines;
    document.pdf_abbreviation_boundaries = matches!(layout, Layout::Titles | Layout::ClosingQuotes);
    document.pdf_quote_boundaries = layout == Layout::ClosingQuotes;
    true
}

fn preserves_anchors(document: &Document, sentences: &[Sentence]) -> bool {
    let original = &document.sentences;
    let protected = document
        .bookmarks
        .iter()
        .chain(document.notes.keys())
        .chain(document.speech_text_overrides.keys())
        .chain(
            document
                .reviewed_translations
                .values()
                .flat_map(|entries| entries.keys()),
        )
        .chain(
            document
                .speech_resume
                .iter()
                .map(|resume| &resume.sentence_id),
        );
    for id in protected {
        if !unchanged_sentence(original, sentences, id) {
            return false;
        }
    }

    true
}

fn unchanged_sentence(before: &[Sentence], after: &[Sentence], id: &str) -> bool {
    before.iter().find(|sentence| sentence.id == id).is_some_and(|old| {
        after.binary_search_by_key(&old.start_utf16, |new| new.start_utf16).is_ok_and(|index| {
            after[index].id == id && after[index].end_utf16 == old.end_utf16 && after[index].text == old.text
        })
    })
}

pub(crate) fn upgrade_documents(connection: &mut Connection) -> Result<()> {
    if connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
        .map_err(storage_error)?
        >= 5
    {
        return Ok(());
    }

    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    if tx
        .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
        .map_err(storage_error)?
        < 5
    {
        let ids = {
            let mut query = tx
                .prepare("SELECT id FROM documents WHERE json_extract(body,'$.format')='pdf'")
                .map_err(storage_error)?;
            let rows = query
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(storage_error)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage_error)?
        };
        for id in ids {
            let (body, resume): (String, Option<String>) = tx.query_row(
                "SELECT d.body,s.body FROM documents d LEFT JOIN speech_resume s ON s.document_id=d.id WHERE d.id=?1",
                [&id], |row| Ok((row.get(0)?, row.get(1)?)),
            ).map_err(storage_error)?;
            let mut document: Document = serde_json::from_str(&body).map_err(storage_error)?;
            document.speech_resume = resume
                .as_deref()
                .and_then(|body| serde_json::from_str(body).ok());
            if upgrade_document(&mut document) {
                document.speech_resume = None;
                let body = serde_json::to_string(&document).map_err(storage_error)?;
                tx.execute(
                    "UPDATE documents SET body=?2 WHERE id=?1",
                    params![id, body],
                )
                .map_err(storage_error)?;
            }
        }
        crate::pdf_detached_pages::upgrade_sentence_layouts(&tx, Layout::ClosingQuotes, None)?;
        tx.pragma_update(None, "user_version", 5)
            .map_err(storage_error)?;
    }
    tx.commit().map_err(storage_error)
}

pub(crate) fn upgrade_prose_documents(connection: &mut Connection) -> Result<()> {
    upgrade_anchored_sentences(connection, 7, BoundaryUpgrade::Prose)
}

pub(crate) fn upgrade_pdf_references(connection: &mut Connection) -> Result<()> {
    upgrade_anchored_sentences(connection, 8, BoundaryUpgrade::Pdf(Layout::PageReferences))
}

pub(crate) fn upgrade_pdf_list_items(connection: &mut Connection) -> Result<()> {
    upgrade_anchored_sentences(connection, 9, BoundaryUpgrade::Pdf(Layout::ListItems))
}

pub(crate) fn upgrade_photo_list_items(connection: &mut Connection) -> Result<()> {
    upgrade_anchored_sentences(connection, 10, BoundaryUpgrade::PhotoLists)
}

#[derive(Clone, Copy)]
enum BoundaryUpgrade {
    Prose,
    Pdf(Layout),
    PhotoLists,
}

fn upgrade_anchored_sentences(connection: &mut Connection, target_version: u32, upgrade: BoundaryUpgrade) -> Result<()> {
    let version = |db: &Connection| db.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
        .map_err(storage_error);
    if version(connection)? >= target_version {
        return Ok(());
    }

    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(storage_error)?;
    if version(&tx)? < target_version {
        let ids = {
            let selection = match upgrade {
                BoundaryUpgrade::PhotoLists => "SELECT id FROM documents WHERE json_extract(body,'$.format')!='pdf'
                 AND json_type(body,'$.ocrLines')='array'
                 AND coalesce(json_extract(body,'$.photoListItemBoundaries'),0)=0",
                BoundaryUpgrade::Pdf(Layout::ListItems) => "SELECT id FROM documents WHERE json_extract(body,'$.format')='pdf'
                 AND json_extract(body,'$.pdfQuoteBoundaries')=1
                 AND coalesce(json_extract(body,'$.pdfListItemBoundaries'),0)=0",
                BoundaryUpgrade::Pdf(_) => "SELECT id FROM documents WHERE json_extract(body,'$.format')='pdf'
                 AND json_extract(body,'$.pdfQuoteBoundaries')=1
                 AND coalesce(json_extract(body,'$.pdfReferenceBoundaries'),0)=0
                 AND instr(lower(json_extract(body,'$.text')),'p.')>0",
                BoundaryUpgrade::Prose => "SELECT id FROM documents WHERE json_extract(body,'$.format') IN ('txt','docx')
                 AND coalesce(json_extract(body,'$.proseSentenceBoundaries'),0)=0
                 AND coalesce(json_type(body,'$.ocrLines'),'null')='null'",
            };
            let mut query = tx.prepare(selection).map_err(storage_error)?;
            let rows = query.query_map([], |row| row.get::<_, String>(0)).map_err(storage_error)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(storage_error)?
        };
        for id in ids {
            let Some((mut document, checkpoint)) = read_upgrade_candidate(&tx, &id)? else { continue; };
            if upgrade.apply(&tx, &mut document, checkpoint.as_ref())? {
                save_upgrade(&tx, &mut document)?;
            }
        }
        if let BoundaryUpgrade::Pdf(layout) = upgrade {
            crate::pdf_detached_pages::upgrade_sentence_layouts(&tx, layout, None)?;
        }
        tx.pragma_update(None, "user_version", target_version).map_err(storage_error)?;
    }
    tx.commit().map_err(storage_error)
}

pub(crate) fn upgrade_restored_boundaries(db: &Connection, ids: &[String]) -> Result<()> {
    for id in ids {
        let Some((mut document, checkpoint)) = read_upgrade_candidate(db, id)? else { continue; };
        let upgrades: &[BoundaryUpgrade] = if document.format == "pdf" {
            &[
                BoundaryUpgrade::Pdf(Layout::PageReferences),
                BoundaryUpgrade::Pdf(Layout::ListItems),
            ]
        } else if document.ocr_lines.is_some() {
            &[BoundaryUpgrade::PhotoLists]
        } else {
            &[BoundaryUpgrade::Prose]
        };
        let mut changed = false;
        for upgrade in upgrades {
            changed |= upgrade.apply(db, &mut document, checkpoint.as_ref())?;
        }
        if changed {
            save_upgrade(db, &mut document)?;
        }

        if document.format == "pdf" {
            for layout in [Layout::ClosingQuotes, Layout::PageReferences, Layout::ListItems] {
                crate::pdf_detached_pages::upgrade_sentence_layouts(db, layout, Some(id))?;
            }
        }
    }
    Ok(())
}

fn read_upgrade_candidate(db: &Connection, id: &str) -> Result<Option<(Document, Option<SpeechResume>)>> {
    let (body, checkpoint): (String, Option<String>) = db.query_row(
        "SELECT d.body,s.body FROM documents d LEFT JOIN speech_resume s ON s.document_id=d.id WHERE d.id=?1",
        [id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).map_err(storage_error)?;
    let Ok(checkpoint) = checkpoint.as_deref().map(serde_json::from_str).transpose() else {
        return Ok(None);
    };
    let document = serde_json::from_str(&body).map_err(storage_error)?;
    Ok(Some((document, checkpoint)))
}

impl BoundaryUpgrade {
    fn apply(self, db: &Connection, document: &mut Document, checkpoint: Option<&SpeechResume>) -> Result<bool> {
        let sentences = match self {
            Self::Pdf(Layout::ListItems)
                if document.pdf_quote_boundaries && !document.pdf_list_item_boundaries
                    && has_list_marker(&document.text) =>
                Layout::ListItems.segment(&document.text, &document.id),
            Self::Pdf(Layout::PageReferences)
                if document.pdf_quote_boundaries && !document.pdf_reference_boundaries =>
                Layout::PageReferences.segment(&document.text, &document.id),
            Self::PhotoLists if !document.photo_list_item_boundaries =>
                Layout::ListItems.segment(&document.text, &document.id),
            Self::Prose if matches!(document.format.as_str(), "txt" | "docx")
                && !document.prose_sentence_boundaries => segment_prose(&document.text, &document.id),
            _ => return Ok(false),
        };
        if document.sentences.iter().map(|s| (s.start_utf8, s.end_utf8))
            .eq(sentences.iter().map(|s| (s.start_utf8, s.end_utf8)))
        {
            return Ok(false);
        }
        if checkpoint.is_some_and(|resume| !unchanged_sentence(&document.sentences, &sentences, &resume.sentence_id))
            || !preserves_anchors(document, &sentences)
            || !preserves_dictionary_anchors(db, document, &sentences)?
        {
            return Ok(false);
        }
        if let Some(cursor) = document.sentences.get(document.position) {
            let offset = cursor.start_utf16.saturating_add(document.reading_token_utf16.unwrap_or(0));
            document.position = position_at(&sentences, offset);
            document.reading_token_utf16 = sentences.get(document.position).and_then(|sentence| {
                (sentence.start_utf16 < offset && offset < sentence.end_utf16)
                    .then(|| offset - sentence.start_utf16)
            });
        }
        document.sentences = sentences;
        match self {
            Self::Pdf(layout) => {
                document.pdf_reference_boundaries = true;
                document.pdf_list_item_boundaries = layout == Layout::ListItems;
            }
            Self::PhotoLists => document.photo_list_item_boundaries = true,
            Self::Prose => document.prose_sentence_boundaries = true,
        }
        Ok(true)
    }
}

fn save_upgrade(db: &Connection, document: &mut Document) -> Result<()> {
    crate::document_revisions::append(document, db)?;
    let body = serde_json::to_string(document).map_err(storage_error)?;
    db.execute("UPDATE documents SET body=?1 WHERE id=?2", params![body, document.id]).map_err(storage_error)?;
    Ok(())
}

fn preserves_dictionary_anchors(db: &Connection, document: &Document, sentences: &[Sentence]) -> Result<bool> {
    let mut query = db.prepare(
        "SELECT json_extract(anchor,'$.sentenceStartUtf16'), sentence FROM dictionary_context_anchors
         WHERE document_id=?1 OR document_id IN (SELECT document_id FROM dictionary_sources WHERE source_hash=?2)",
    ).map_err(storage_error)?;
    let rows = query.query_map(params![document.id, document.source_hash], |row| {
        Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, String>(1)?))
    }).map_err(storage_error)?;
    for row in rows {
        let (start, text) = row.map_err(storage_error)?;
        if !start.and_then(|start| usize::try_from(start).ok()).is_some_and(|start| {
            sentences.binary_search_by_key(&start, |sentence| sentence.start_utf16)
                .is_ok_and(|index| sentences[index].text == text)
        }) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn attach_closing_quotes(text: &str, id: &str, sentences: Vec<Sentence>) -> Vec<Sentence> {
    let mut joined: Vec<Sentence> = Vec::with_capacity(sentences.len());
    for mut next in sentences {
        let remainder = next.text.trim_start_matches(['»', '”', '›', '」', '』']);
        let quote_bytes = next.text.len() - remainder.len();
        if quote_bytes > 0 {
            if let Some(previous) = joined.last_mut().filter(|previous| {
                let gap = &text[previous.end_utf8..next.start_utf8];
                previous.text.ends_with(['.', '!', '?', '…']) &&
                    gap.chars().all(char::is_whitespace) && gap.bytes().filter(|&byte| byte == b'\n').count() <= 1
            }) {
                let end = next.start_utf8 + quote_bytes;
                previous.end_utf16 += text[previous.end_utf8..end].encode_utf16().count();
                previous.end_utf8 = end;
                previous.text = text[previous.start_utf8..end].to_owned();
                let remainder = remainder.trim_start();
                if remainder.is_empty() { continue; }
                let lifted = next.text.len() - remainder.len();
                next.start_utf16 += next.text[..lifted].encode_utf16().count();
                next.start_utf8 += lifted;
                next.text = remainder.to_owned();
                next.id = format!("{id}:{}", next.start_utf8);
            }
        }
        joined.push(next);
    }
    joined
}

pub(crate) fn mask_title_periods(boundaries: &mut [u8]) {
    let text = std::str::from_utf8(boundaries).expect("PDF sentence boundary text is UTF-8");
    let indices: Vec<_> = text.match_indices('.').filter_map(|(dot, _)| {
        let title = text[..dot].rsplit(|ch: char| !ch.is_alphabetic()).next()?;
        if !is_abbreviated_title(title) {
            return None;
        }
        let tail = &text[dot + 1..];
        let name = tail.trim_start();
        let gap = &tail[..tail.len() - name.len()];
        (!gap.is_empty() && !gap.contains(['\r', '\n']) && name.starts_with(char::is_uppercase))
            .then_some(dot)
    }).collect();
    for index in indices {
        boundaries[index] = b' ';
    }
}

pub(crate) fn is_abbreviated_title(word: &str) -> bool {
    matches!(word, "M" | "MM" | "Mme" | "Mmes" | "Mlle" | "Mlles" | "Dr" | "Pr" | "Me" | "Mgr" |
        "Mr" | "Mrs" | "Ms")
}

pub(crate) fn is_page_reference(abbreviation: &str, suffix: &str) -> bool {
    if !is_page_abbreviation(abbreviation) { return false; }
    let number = suffix.trim_start_matches(|ch: char| ch.is_whitespace() && !matches!(ch, '\n' | '\r'));
    number.len() < suffix.len() && number.starts_with(|ch: char| ch.is_ascii_digit())
}

fn is_page_abbreviation(word: &str) -> bool {
    matches!(word, "p" | "pp" | "P" | "PP" | "Pp")
}

pub(crate) fn printed_list_label(marker: &str, suffix: &str) -> bool {
    !is_page_reference(marker, suffix)
        && ((marker.len() == 1 && marker.as_bytes()[0].is_ascii_lowercase())
            || ((1..=3).contains(&marker.len()) && marker.bytes().all(|byte| byte.is_ascii_digit())))
}

pub(crate) fn list_marker_period(previous: &str, line: &str) -> Option<usize> {
    let body = line.trim_start_matches(|ch: char| ch.is_whitespace() || ch == '\u{fffc}');
    let token = body.split_whitespace().next()?;
    let marker = token.strip_suffix('.')?;
    let suffix = &body[token.len()..];
    let heading = suffix.trim_start_matches(|ch: char| ch.is_whitespace() && !matches!(ch, '\r' | '\n'));
    if !printed_list_label(marker, suffix) || !heading.starts_with(char::is_alphabetic) {
        return None;
    }
    // A citation can wrap onto the next physical line before its number.
    if wrapped_page_number(marker, previous) {
        return None;
    }
    Some(line.len() - body.len() + marker.len())
}

pub(crate) fn wrapped_page_number(marker: &str, previous: &str) -> bool {
    marker.starts_with(|ch: char| ch.is_ascii_digit())
        && previous.split_whitespace().last().and_then(|word| word.strip_suffix('.'))
            .is_some_and(is_page_abbreviation)
}

pub(crate) fn has_list_marker(text: &str) -> bool {
    std::iter::once("").chain(text.lines()).zip(text.lines())
        .any(|(previous, line)| list_marker_period(previous, line).is_some())
}

pub(crate) fn mask_page_reference_periods(boundaries: &mut [u8]) {
    let text = std::str::from_utf8(boundaries).expect("PDF sentence boundary text is UTF-8");
    let periods: Vec<_> = text.match_indices('.').filter_map(|(dot, _)| {
        let abbreviation = text[..dot].rsplit(|ch: char| !ch.is_alphanumeric() && ch != '_').next()?;
        is_page_reference(abbreviation, &text[dot + 1..]).then_some(dot)
    }).collect();
    for period in periods { boundaries[period] = b' '; }
}

fn storage_error(error: impl std::fmt::Display) -> String {
    format!("Не вдалося оновити межі речень. {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_quote_moves_to_the_previous_pdf_sentence_with_its_original_span() {
        let text = "– Faites votre métier... capitaine Dantès. »\nLe jeune homme donne ses ordres aux marins.";
        let sentences = Layout::ClosingQuotes.segment(text, "pdf");
        assert_eq!(sentences.last().unwrap().text, "Le jeune homme donne ses ordres aux marins.");
        assert!(sentences[..sentences.len()-1].last().unwrap().text.ends_with("Dantès. »"));
        for sentence in sentences {
            assert_eq!(&text[sentence.start_utf8..sentence.end_utf8], sentence.text);
            assert_eq!(sentence.start_utf16, text[..sentence.start_utf8].encode_utf16().count());
            assert_eq!(sentence.end_utf16, text[..sentence.end_utf8].encode_utf16().count());
        }
    }

    #[test]
    fn page_references_stay_in_the_sentence_without_crossing_paragraphs() {
        let text = "Consultez la fiche méthodologique 3, p. 189. En fonction du sujet, faites votre choix.\n\nVoir pp. 12–14. Puis revenez.\n\nVoir p.\n\n42. Nouvel exercice.";
        let sentences = Layout::PageReferences.segment(text, "pdf");
        let excerpts: Vec<_> = sentences.iter().map(|sentence| sentence.text.as_str()).collect();
        assert_eq!(&excerpts[..4], [
            "Consultez la fiche méthodologique 3, p. 189.", "En fonction du sujet, faites votre choix.",
            "Voir pp. 12–14.", "Puis revenez.",
        ]);
        assert_eq!(excerpts[4], "Voir p.");
        for sentence in sentences {
            assert_eq!(&text[sentence.start_utf8..sentence.end_utf8], sentence.text);
            assert_eq!(text[..sentence.end_utf8].encode_utf16().count(), sentence.end_utf16);
        }
        assert_eq!(Layout::from_storage(4).segment("Voir p. 189. Puis revenez.", "pdf").len(), 3);
    }

    #[test]
    fn printed_exercise_labels_stay_with_their_text_without_absorbing_a_new_item() {
        let text = "11. Préparez votre essai argumenté.\na. Analysez le sujet proposé p.\n35. Relevez les mots-clés :\nb. Pensez au style.\n\n12. Choisissez votre plan.";
        let sentences = Layout::ListItems.segment(text, "pdf");
        assert_eq!(sentences.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), [
            "11. Préparez votre essai argumenté.", "a. Analysez le sujet proposé p.\n35.",
            "Relevez les mots-clés :", "b. Pensez au style.", "12. Choisissez votre plan.",
        ]);
        let document = Document { text: text.into(), ..Document::default() };
        assert_eq!(crate::document_layout::pdf_list_label_ranges(&document, text, 0)
            .map(|range| &text[range]).collect::<Vec<_>>(), ["11.", "a.", "b.", "12."]);
        for sentence in sentences {
            assert_eq!(&text[sentence.start_utf8..sentence.end_utf8], sentence.text);
            assert_eq!(text[..sentence.start_utf8].encode_utf16().count(), sentence.start_utf16);
            assert_eq!(text[..sentence.end_utf8].encode_utf16().count(), sentence.end_utf16);
        }
    }

    #[test]
    fn upgrades_existing_pdf_references_only_when_anchors_survive() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reader.db");
        let mut store = crate::Store::open(&path).unwrap();
        let add_legacy = |store: &mut crate::Store, number: u16, protected| {
            let text = format!("Consultez la fiche, p. {number}. Puis revenez.");
            let line = crate::ocr::OcrLine {
                page: 0, text: text.clone(), x: 0.1, y: 0.2, width: 0.6, height: 0.02,
                rotation: None, correction: None, confidence: None, block_start: false,
                words: vec![], source_span: None,
            };
            let bytes = format!("%PDF-1.4\n% {number}\n");
            let mut doc = store.import_scanned_pdf("fiche.pdf", bytes.as_bytes(), "fr", vec![line], 1).unwrap();
            doc.pdf_reference_boundaries = false;
            doc.sentences = Layout::ClosingQuotes.segment(&doc.text, &doc.id);
            assert_eq!(doc.sentences.len(), 3);
            doc.position = 1;
            if protected { doc.bookmarks.push(doc.sentences[1].id.clone()); }
            store.connection.execute("UPDATE documents SET body=?2 WHERE id=?1",
                params![doc.id, serde_json::to_string(&doc).unwrap()]).unwrap();
            doc.id
        };
        let free = add_legacy(&mut store, 189, false);
        let bookmarked = add_legacy(&mut store, 190, true);
        store.connection.pragma_update(None, "user_version", 7).unwrap();
        drop(store);
        let docs = crate::Store::open(&path).unwrap().snapshot().unwrap().documents;
        let free = docs.iter().find(|doc| doc.id == free).unwrap();
        assert_eq!(free.sentences.len(), 2);
        assert_eq!(free.position, 0);
        let bookmarked = docs.iter().find(|doc| doc.id == bookmarked).unwrap();
        assert_eq!(bookmarked.sentences.len(), 3);
        assert_eq!(bookmarked.bookmarks, [bookmarked.sentences[1].id.clone()]);
    }

    #[test]
    fn french_title_is_not_the_end_of_a_pdf_sentence() {
        let text = "Son propriétaire, M. Morrel, attend sur le quai. Il regarde le Pharaon.\nM.\nMorrel sourit.";
        let sentences = Layout::Titles.segment(text, "pdf");
        let excerpts: Vec<_> = sentences.iter().map(|sentence| sentence.text.as_str()).collect();
        assert_eq!(excerpts, ["Son propriétaire, M. Morrel, attend sur le quai.", "Il regarde le Pharaon.", "M.\nMorrel sourit."]);
        for sentence in sentences {
            assert_eq!(&text[sentence.start_utf8..sentence.end_utf8], sentence.text);
        }
    }

    #[test]
    fn structured_pdf_splits_headings_and_dialogue_but_keeps_wrapped_lines() {
        let text = "CHAPITRE 1\nIl veut rejoindre\nson bateau\n– Je viens,\nmon ami !\n– Moi aussi.";
        let legacy = Layout::SoftLines.segment(text, "pdf");
        let structured = Layout::Structured.segment(text, "pdf");
        let excerpts: Vec<_> = structured.iter().map(|sentence| sentence.text.as_str()).collect();
        assert_eq!(excerpts, ["CHAPITRE 1", "Il veut rejoindre\nson bateau", "– Je viens,\nmon ami !", "– Moi aussi."]);
        assert!(structured.len() > legacy.len());
        for sentence in structured {
            assert_eq!(&text[sentence.start_utf8..sentence.end_utf8], sentence.text);
            assert_eq!(text[..sentence.start_utf8].encode_utf16().count(), sentence.start_utf16);
        }
    }

    #[test]
    fn wrapped_pdf_sentences_keep_source_offsets_and_saved_contexts() {
        let text = "Une idée ✨\ntrès claire. Puis on passe\r\nà la suite.\n\nНова сторінка.";
        let mut document = Document {
            id: "pdf".into(),
            format: "pdf".into(),
            text: text.into(),
            sentences: crate::segment(text, "pdf"),
            position: 1,
            reading_token_utf16: Some(5),
            ..Document::default()
        };

        let old_cursor = document.sentences[1].start_utf16 + 5;
        document.bookmarks.push(document.sentences[1].id.clone());
        assert!(!upgrade_document(&mut document));
        document.bookmarks.clear();
        assert!(upgrade_document(&mut document));
        assert_eq!(
            document
                .sentences
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>(),
            [
                "Une idée ✨\ntrès claire.",
                "Puis on passe\r\nà la suite.",
                "Нова сторінка.",
            ]
        );
        let selected = &document.sentences[document.position];
        assert_eq!(
            selected.start_utf16 + document.reading_token_utf16.unwrap(),
            old_cursor
        );
        for sentence in &document.sentences {
            assert_eq!(&text[sentence.start_utf8..sentence.end_utf8], sentence.text);
            assert_eq!(
                text[..sentence.start_utf8].encode_utf16().count(),
                sentence.start_utf16
            );
        }
    }
}
