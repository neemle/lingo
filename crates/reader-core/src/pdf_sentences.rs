use crate::{Document, Result, Sentence};
use rusqlite::{params, Connection, TransactionBehavior};

pub(crate) fn segment(text: &str, id: &str, soft_lines: bool) -> Vec<Sentence> {
    if soft_lines {
        crate::segment_pdf(text, id)
    } else {
        crate::segment(text, id)
    }
}

pub(crate) fn position_at(sentences: &[Sentence], offset: usize) -> usize {
    sentences
        .partition_point(|sentence| sentence.end_utf16 <= offset)
        .min(sentences.len().saturating_sub(1))
}

/// The legacy sentence remains intact if changing it would alter a saved translation or annotation.
pub(crate) fn upgrade_document(document: &mut Document) -> bool {
    if document.format != "pdf" || document.pdf_soft_lines {
        return false;
    }
    let sentences = crate::segment_pdf(&document.text, &document.id);
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

    if let Some(previous) = original.get(document.position) {
        let offset = previous.start_utf16 + document.reading_token_utf16.unwrap_or(0);
        document.position = position_at(&sentences, offset);
        document.reading_token_utf16 = document.reading_token_utf16.and_then(|_| {
            let new = sentences.get(document.position)?;
            (new.start_utf16 <= offset && offset < new.end_utf16).then(|| offset - new.start_utf16)
        });
    }
    document.sentences = sentences;
    document.pdf_soft_lines = true;
    true
}

pub(crate) fn upgrade_documents(connection: &mut Connection) -> Result<()> {
    if connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
        .map_err(storage_error)?
        >= 2
    {
        return Ok(());
    }

    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    if tx
        .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
        .map_err(storage_error)?
        < 2
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
        tx.pragma_update(None, "user_version", 2)
            .map_err(storage_error)?;
    }
    tx.commit().map_err(storage_error)
}

fn storage_error(error: impl std::fmt::Display) -> String {
    format!("Не вдалося оновити межі речень PDF. {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

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
