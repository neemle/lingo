use crate::{Document, Result, Sentence};
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
}

impl Layout {
    pub(crate) fn from_storage(value: u8) -> Self {
        match value {
            3 => Self::Titles,
            2 => Self::Structured,
            1 => Self::SoftLines,
            _ => Self::Literal,
        }
    }

    pub(crate) fn segment(self, text: &str, id: &str) -> Vec<Sentence> {
        match self {
            Self::Titles => crate::segment_pdf_with_titles(text, id),
            Self::Structured => crate::segment_pdf_structured(text, id),
            Self::SoftLines => crate::segment_pdf(text, id),
            Self::Literal => crate::segment(text, id),
        }
    }
}

pub(crate) fn segment_document(document: &Document, text: &str) -> Vec<Sentence> {
    if document.format == "pdf" && document.pdf_abbreviation_boundaries {
        crate::segment_pdf_with_titles(text, &document.id)
    } else if document.format == "pdf" && document.pdf_structured_sentences {
        crate::segment_pdf_structured(text, &document.id)
    } else {
        segment(text, &document.id, document.pdf_soft_lines)
    }
}

pub(crate) fn position_at(sentences: &[Sentence], offset: usize) -> usize {
    sentences
        .partition_point(|sentence| sentence.end_utf16 <= offset)
        .min(sentences.len().saturating_sub(1))
}

/// The legacy sentence remains intact if changing it would alter a saved translation or annotation.
pub(crate) fn upgrade_document(document: &mut Document) -> bool {
    if document.format != "pdf" || document.pdf_abbreviation_boundaries {
        return false;
    }
    let candidates = [Layout::Titles, Layout::Structured, Layout::SoftLines];
    let previous = if document.pdf_structured_sentences { Layout::Structured }
        else if document.pdf_soft_lines { Layout::SoftLines } else { Layout::Literal };
    let Some((layout, sentences)) = candidates.into_iter()
        .take_while(|layout| *layout as u8 > previous as u8)
        .map(|layout| (layout, layout.segment(&document.text, &document.id)))
        .find(|(_, sentences)| preserves_anchors(document, sentences))
    else { return false; };

    if let Some(previous) = document.sentences.get(document.position) {
        let offset = previous.start_utf16 + document.reading_token_utf16.unwrap_or(0);
        document.position = position_at(&sentences, offset);
        document.reading_token_utf16 = document.reading_token_utf16.and_then(|_| {
            let new = sentences.get(document.position)?;
            (new.start_utf16 <= offset && offset < new.end_utf16).then(|| offset - new.start_utf16)
        });
    }
    document.sentences = sentences;
    document.pdf_soft_lines = true;
    document.pdf_structured_sentences = layout != Layout::SoftLines;
    document.pdf_abbreviation_boundaries = layout == Layout::Titles;
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
        let unchanged = original
            .iter()
            .find(|sentence| sentence.id == *id)
            .is_some_and(|old| {
                sentences
                    .binary_search_by_key(&old.start_utf16, |new| new.start_utf16)
                    .is_ok_and(|index| {
                        sentences[index].end_utf16 == old.end_utf16
                            && sentences[index].text == old.text
                    })
            });
        if !unchanged {
            return false;
        }
    }

    true
}

pub(crate) fn upgrade_documents(connection: &mut Connection) -> Result<()> {
    if connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
        .map_err(storage_error)?
        >= 4
    {
        return Ok(());
    }

    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    if tx
        .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
        .map_err(storage_error)?
        < 4
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
        crate::pdf_detached_pages::upgrade_sentence_layouts(&tx)?;
        tx.pragma_update(None, "user_version", 4)
            .map_err(storage_error)?;
    }
    tx.commit().map_err(storage_error)
}

pub(crate) fn mask_title_periods(boundaries: &mut [u8]) {
    let text = std::str::from_utf8(boundaries).expect("PDF sentence boundary text is UTF-8");
    let indices: Vec<_> = text.match_indices('.').filter_map(|(dot, _)| {
        let title = text[..dot].rsplit(|ch: char| !ch.is_alphabetic()).next()?;
        if !matches!(title, "M" | "MM" | "Mme" | "Mmes" | "Mlle" | "Mlles" | "Dr" | "Pr" | "Me" | "Mgr" |
            "Mr" | "Mrs" | "Ms") {
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

fn storage_error(error: impl std::fmt::Display) -> String {
    format!("Не вдалося оновити межі речень PDF. {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

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
