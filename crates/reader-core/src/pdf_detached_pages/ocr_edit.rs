use super::{annotations, database_error, page_revision, sqlite_offset};
use crate::{ocr, Result, Sentence, Store};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedPdfOcrPage {
    pub(crate) revision: String,
    pub(crate) lines: Vec<ocr::OcrLine>,
}

impl Store {
    pub fn detached_pdf_ocr(&self, id: &str, page: u16) -> Result<Option<DetachedPdfOcrPage>> {
        read_ocr(&self.connection, id, page)?
            .map(|(text, encoded)| ocr_page(&text, &encoded))
            .transpose()
    }

    pub fn focus_detached_pdf_ocr_word(
        &mut self,
        id: &str,
        page: u16,
        revision: &str,
        start_utf16: usize,
        word: &str,
    ) -> Result<()> {
        if word.is_empty() || word.len() > 4096 {
            return Err("Слово з оригіналу PDF недоступне.".into());
        }
        self.edit_document(id, |document, tx| {
            let (text, encoded) =
                read_ocr(tx, id, page)?.ok_or("На сторінці PDF немає розпізнаних рядків.")?;
            let frame = ocr_page(&text, &encoded)?;
            if frame.revision != revision {
                return Err("Розпізнаний текст сторінки змінився. Виберіть слово знову.".into());
            }
            let span = frame
                .lines
                .iter()
                .flat_map(|line| &line.words)
                .find_map(|candidate| {
                    candidate
                        .source_span
                        .filter(|span| span.start_utf16 == start_utf16 &&
                            text.get(span.start_utf8..span.end_utf8) == Some(word))
                })
                .ok_or("Слово більше не відповідає тексту сторінки PDF.")?;
            let sentences = super::page_layout(tx, id, page)?.segment(&text, "");
            let position =
                sentences.partition_point(|sentence| sentence.end_utf16 <= span.start_utf16);
            if !sentences.get(position).is_some_and(|sentence| {
                sentence.start_utf16 <= span.start_utf16 && span.end_utf16 <= sentence.end_utf16
            }) {
                return Err("Слово не вдалося прив’язати до речення PDF.".into());
            }

            crate::pdf_reading_position::set_page(tx, document, page, true)?;
            super::write_position(tx, id, page, position)?;
            Ok(false)
        })
    }

    pub fn correct_detached_pdf_ocr(
        &mut self,
        id: &str,
        page: u16,
        revision: &str,
        index: usize,
        correction: Option<String>,
    ) -> Result<DetachedPdfOcrPage> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let (old_text, encoded) =
            read_ocr(&tx, id, page)?.ok_or("На сторінці PDF немає розпізнаних рядків.")?;
        let mut previous = ocr_page(&old_text, &encoded)?;
        if previous.revision != revision {
            return Err("Розпізнаний текст сторінки змінився. Відкрийте рядок знову.".into());
        }
        let line = previous
            .lines
            .get_mut(index)
            .ok_or("Розпізнаний рядок не знайдено.")?;
        let correction = correction.map(|text| text.trim().to_owned());
        if line.correction == correction {
            return Ok(previous);
        }
        let line_start = line
            .source_span
            .ok_or("Розпізнаний рядок не має позиції в тексті.")?
            .start_utf16;
        let edits = Edit::for_line(
            line_start,
            line.reading_text(),
            correction.as_deref().unwrap_or(line.text.trim()),
        );
        line.correction = correction;
        let updated = save_ocr_edits(&tx, id, page, &old_text, previous.lines, &edits)?;
        tx.commit().map_err(database_error)?;
        Ok(updated)
    }

    pub(crate) fn reconcile_detached_pdf_ocr(
        &mut self, id: &str, page: u16, revision: &str, language: &str,
        reconcile: impl FnOnce(&mut [ocr::OcrLine]) -> Result<bool>,
    ) -> Result<bool> {
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let (old_text, encoded) =
            read_ocr(&tx, id, page)?.ok_or("На сторінці PDF немає розпізнаних рядків.")?;
        let mut previous = ocr_page(&old_text, &encoded)?;
        let current_language: String = tx.query_row(
            "SELECT json_extract(body, '$.language') FROM documents WHERE id=?1",
            [id], |row| row.get(0),
        ).map_err(database_error)?;
        if previous.revision != revision || current_language != language {
            return Err("Текст або мова PDF змінилися. Відкрийте сторінку знову.".into());
        }
        let previous_lines: Vec<_> = previous.lines.iter().map(|line| line.reading_text().to_owned()).collect();
        if !reconcile(&mut previous.lines)? {
            return Ok(false);
        }
        let mut edits = Vec::new();
        for (line, old) in previous.lines.iter().zip(previous_lines.iter().map(String::as_str))
            .filter(|(line, old)| line.reading_text() != *old)
        {
            let start = line.source_span.ok_or("Розпізнаний рядок не має позиції в тексті.")?.start_utf16;
            edits.extend(Edit::for_line(start, old, line.reading_text()));
        }
        save_ocr_edits(&tx, id, page, &old_text, previous.lines, &edits)?;
        tx.commit().map_err(database_error)?;
        Ok(true)
    }

    pub(crate) fn upgrade_detached_ocr_layout(&mut self, id: &str, page: u16, revision: &str, reference: &str) -> Result<bool> {
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(database_error)?;
        let current: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM documents WHERE id=?1 AND json_extract(body, '$.currentRevisionId')=?2)",
            params![id, revision], |row| row.get(0),
        ).map_err(database_error)?;
        if !current { return Ok(false); }
        let Some((old, encoded)) = read_ocr(&tx, id, page)? else { return Ok(false); };
        if old != reference { return Ok(false); }
        let lines = ocr_page(&old, &encoded)?.lines;
        let Some((rebuilt, _, edits)) = crate::ocr_layout::upgrade_cached_page(&lines, &old)
            else { return Ok(false); };
        let layout = super::page_layout(&tx, id, page)?;
        let sentences = layout.segment(&old, "");
        let protected = annotations::list(&tx, id, page)?.iter().any(|item| {
            let sentence = sentence_at(&sentences, item.start_utf16).and_then(|index| sentences.get(index));
            sentence.is_none_or(|sentence|
                crate::ocr_layout::intersects_edits(sentence.start_utf16, sentence.end_utf16, &edits))
        });
        let has_resume: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pdf_detached_speech_resume WHERE document_id=?1 AND page=?2)",
            params![id, page], |row| row.get(0),
        ).map_err(database_error)?;
        if protected || has_resume ||
            crate::dictionary_transfer::selection_overlaps_layout_edits(&tx, id, Some(page), &page_revision(&old), &edits)? {
            return Ok(false);
        }
        let edits: Vec<_> = edits.into_iter().map(|edit| Edit {
            start: edit.start_utf16,
            old_end: edit.start_utf16 + edit.old_len,
            new_end: edit.start_utf16 + edit.new_len,
        }).collect();
        save_ocr_edits(&tx, id, page, &old, rebuilt, &edits)?;
        tx.commit().map_err(database_error)?;
        Ok(true)
    }

    pub(crate) fn supplement_detached_pdf_ocr(
        &mut self, id: &str, page: u16, revision: &str, language: &str, incoming: Vec<ocr::OcrLine>,
    ) -> Result<bool> {
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let (text, encoded) = read_ocr(&tx, id, page)?.ok_or("Розпізнаний текст сторінки недоступний.")?;
        let previous = ocr_page(&text, &encoded)?;
        let current_language: String = tx.query_row(
            "SELECT json_extract(body, '$.language') FROM documents WHERE id=?1",
            [id], |row| row.get(0),
        ).map_err(database_error)?;
        if previous.revision != revision || current_language != language {
            return Err("Текст або мова PDF змінилися. Відкрийте сторінку знову.".into());
        }
        let Some((merged, edits)) = crate::ocr_layout::supplement_page(&previous.lines, incoming)?
            else { return Ok(false); };
        let edits: Vec<_> = edits.into_iter().map(|edit| Edit {
            start: edit.start_utf16,
            old_end: edit.start_utf16 + edit.old_len,
            new_end: edit.start_utf16 + edit.new_len,
        }).collect();
        save_ocr_edits(&tx, id, page, &text, merged, &edits)?;
        tx.commit().map_err(database_error)?;
        Ok(true)
    }
}

fn save_ocr_edits(
    db: &Connection, id: &str, page: u16, old_text: &str, mut lines: Vec<ocr::OcrLine>, edits: &[Edit],
) -> Result<DetachedPdfOcrPage> {
    let text = ocr::recognized_lines(&lines, page.checked_add(1).ok_or("Некоректна сторінка PDF.")?)?;
    for line in &mut lines {
        line.source_span = None;
        for word in &mut line.words {
            word.source_span = None;
        }
    }
    let encoded = serde_json::to_string(&lines).map_err(database_error)?;
    if old_text != text {
        move_sentence_anchors(db, id, page, old_text, &text, Some(edits))?;
    }
    db.execute(
        "UPDATE pdf_detached_pages SET text=?3,lines=?4 WHERE document_id=?1 AND page=?2",
        params![id, page, text, encoded],
    ).map_err(database_error)?;
    ocr_page(&text, &encoded)
}

fn read_ocr(db: &Connection, id: &str, page: u16) -> Result<Option<(String, String)>> {
    db.query_row(
        "SELECT text,lines FROM pdf_detached_pages WHERE document_id=?1 AND page=?2 AND lines IS NOT NULL",
        params![id, page], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(database_error)
}

pub(super) fn ocr_page(text: &str, encoded: &str) -> Result<DetachedPdfOcrPage> {
    let mut lines: Vec<ocr::OcrLine> = serde_json::from_str(encoded).map_err(database_error)?;
    ocr::refresh_line_provenance(text, &mut lines, None);
    Ok(DetachedPdfOcrPage {
        revision: page_revision(encoded),
        lines,
    })
}

struct Edit {
    start: usize,
    old_end: usize,
    new_end: usize,
}

impl Edit {
    fn for_line(start: usize, old: &str, new: &str) -> Vec<Self> {
        ocr::word_replacement_edits(start, old, new).map_or_else(
            || vec![Self::between(start, old, new)],
            |edits| edits.into_iter().map(|edit| Self {
                start: edit.start_utf16,
                old_end: edit.start_utf16 + edit.old_len,
                new_end: edit.start_utf16 + edit.new_len,
            }).collect(),
        )
    }

    fn between(line_start: usize, old: &str, new: &str) -> Self {
        let prefix: usize = old
            .chars()
            .zip(new.chars())
            .take_while(|(a, b)| a == b)
            .map(|(ch, _)| ch.len_utf8())
            .sum();
        let suffix: usize = old[prefix..]
            .chars()
            .rev()
            .zip(new[prefix..].chars().rev())
            .take_while(|(a, b)| a == b)
            .map(|(ch, _)| ch.len_utf16())
            .sum();
        Self {
            start: line_start + old[..prefix].encode_utf16().count(),
            old_end: line_start + old.encode_utf16().count() - suffix,
            new_end: line_start + new.encode_utf16().count() - suffix,
        }
    }

    fn map_start(&self, offset: usize) -> usize {
        if offset < self.start {
            offset
        } else if offset >= self.old_end {
            self.new_end + (offset - self.old_end)
        } else {
            self.start + (offset - self.start).min(self.new_end - self.start)
        }
    }
}

fn sentence_at(sentences: &[Sentence], offset: usize) -> Option<usize> {
    let index = sentences.partition_point(|sentence| sentence.end_utf16 <= offset);
    sentences
        .get(index)
        .map(|_| index)
        .or_else(|| sentences.len().checked_sub(1))
}

pub(super) fn reanchor_replaced_ocr(db: &Connection, id: &str, page: u16, old: &str, new: &str) -> Result<()> {
    move_sentence_anchors(db, id, page, old, new, None)
}

fn replacement_edits(old: &str, new: &str, before: &[Sentence], after: &[Sentence]) -> Vec<Edit> {
    use std::collections::HashMap;

    let mut old_counts = HashMap::new();
    for sentence in before {
        *old_counts.entry(sentence.text.as_str()).or_insert(0usize) += 1;
    }
    let mut new_sentences = HashMap::new();
    for sentence in after {
        new_sentences.entry(sentence.text.as_str())
            .and_modify(|entry| *entry = None)
            .or_insert(Some(sentence));
    }
    let mut edits = Vec::new();
    let (mut old_end, mut old_offset, mut new_end) = (0, 0, 0);
    for sentence in before {
        let key = sentence.text.as_str();
        let Some(Some(matching)) = new_sentences.get(key) else { continue; };
        if old_counts[key] != 1 || matching.start_utf8 < new_end {
            continue;
        }
        let previous = &old[old_end..sentence.start_utf8];
        let refreshed = &new[new_end..matching.start_utf8];
        if previous != refreshed {
            edits.extend(Edit::for_line(old_offset, previous, refreshed));
        }
        (old_end, old_offset, new_end) = (sentence.end_utf8, sentence.end_utf16, matching.end_utf8);
    }
    if old[old_end..] != new[new_end..] {
        edits.extend(Edit::for_line(old_offset, &old[old_end..], &new[new_end..]));
    }
    edits
}

fn move_sentence_anchors(
    db: &Connection,
    id: &str,
    page: u16,
    old: &str,
    new: &str,
    edits: Option<&[Edit]>,
) -> Result<()> {
    let layout = super::page_layout(db, id, page)?;
    let old_revision = page_revision(old);
    let new_revision = page_revision(new);
    let old_sentences = layout.segment(old, &super::sentence_prefix(id, page, &old_revision));
    let new_sentences = layout.segment(new, &super::sentence_prefix(id, page, &new_revision));
    let replacement;
    let edits = match edits {
        Some(edits) => edits,
        None => {
            replacement = replacement_edits(old, new, &old_sentences, &new_sentences);
            &replacement
        }
    };
    let map_start = |offset| edits.iter().rev().fold(offset, |offset, edit| edit.map_start(offset));
    crate::dictionary_transfer::reanchor_selections(
        db, (id, Some(page), &old_revision, &new_revision), &old_sentences, &new_sentences,
        |_, start, length| {
            let end = start.checked_add(length)?;
            (!edits.iter().any(|edit| edit.start < end && edit.old_end > start)).then(|| map_start(start))
        },
    )?;
    let saved: Option<usize> = db
        .query_row(
            "SELECT sentence FROM pdf_detached_positions WHERE document_id=?1 AND page=?2",
            params![id, page],
            |row| Ok(row.get::<_, u32>(0)? as usize),
        )
        .optional()
        .map_err(database_error)?;
    if let Some(position) = saved {
        let old_start = old_sentences
            .get(position)
            .or_else(|| old_sentences.last())
            .map_or(0, |sentence| sentence.start_utf16);
        let position = sentence_at(&new_sentences, map_start(old_start)).unwrap_or(0);
        db.execute(
            "UPDATE pdf_detached_positions SET sentence=?3 WHERE document_id=?1 AND page=?2",
            params![id, page, sqlite_offset(position)?],
        )
        .map_err(database_error)?;
    }

    let items = annotations::list(db, id, page)?;
    if items.is_empty() {
        return Ok(());
    }
    let mut moved: BTreeMap<usize, annotations::ArchivedAnnotation> = BTreeMap::new();
    for mut item in items {
        let index = sentence_at(&new_sentences, map_start(item.start_utf16))
            .ok_or("Після виправлення примітка залишилася без речення.")?;
        let start = new_sentences[index].start_utf16;
        item.start_utf16 = start;
        if let Some(existing) = moved.get_mut(&start) {
            existing.bookmarked |= item.bookmarked;
            if let Some(note) = item.note {
                if let Some(previous) = &mut existing.note {
                    if *previous != note {
                        previous.push_str("\n\n");
                        previous.push_str(&note);
                        annotations::validate_note(previous)?;
                    }
                } else {
                    existing.note = Some(note);
                }
            }
            if let Some(text) = item.speech_text {
                if existing
                    .speech_text
                    .as_ref()
                    .is_some_and(|previous| previous != &text)
                {
                    return Err("Виправлення об’єднує речення з різними варіантами вимови.".into());
                }
                existing.speech_text = Some(text);
            }
            for (target, text) in item.translations {
                if existing
                    .translations
                    .get(&target)
                    .is_some_and(|previous| previous != &text)
                {
                    return Err(
                        "Виправлення об’єднує речення з різними збереженими перекладами.".into(),
                    );
                }
                existing.translations.insert(target, text);
            }
        } else {
            moved.insert(start, item);
        }
    }
    db.execute(
        "DELETE FROM pdf_detached_annotations WHERE document_id=?1 AND page=?2",
        params![id, page],
    )
    .map_err(database_error)?;
    db.execute(
        "DELETE FROM pdf_detached_translations WHERE document_id=?1 AND page=?2",
        params![id, page],
    )
    .map_err(database_error)?;
    db.execute(
        "DELETE FROM pdf_detached_speech_text WHERE document_id=?1 AND page=?2",
        params![id, page],
    )
    .map_err(database_error)?;
    annotations::merge(db, id, page, &moved.into_values().collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{file_originals, pdf_detached_pages::DetachedPdfAnnotationChange};

    #[test]
    fn replacement_edits_keep_a_unique_sentence_between_changed_regions() {
        let old = "Le vieux phare est ouvert. Le marin rentre. Le bateau part.";
        let new = "Intro. La vieille tour est fermée. Le marin rentre. Un voilier a quitté le port.";
        let before = crate::pdf_sentences::Layout::ClosingQuotes.segment(old, "old");
        let after = crate::pdf_sentences::Layout::ClosingQuotes.segment(new, "new");
        let edits = replacement_edits(old, new, &before, &after);
        let start = old[..old.find("marin").unwrap()].encode_utf16().count();
        let mapped = edits.iter().rev().fold(start, |offset, edit| edit.map_start(offset));
        assert_eq!(mapped, new[..new.find("marin").unwrap()].encode_utf16().count());
        assert!(edits.iter().all(|edit| edit.old_end <= start || edit.start >= start + 5));
    }

    #[test]
    fn supplemental_ocr_keeps_a_detached_note_and_the_saved_correction() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("reader.db")).unwrap();
        let original = root.path().join("scan.pdf");
        std::fs::write(&original, "%PDF-1.4\n").unwrap();
        let prepared = file_originals::prepare_pdf_import(
            store.pdf_original_root(), "scan.pdf", &original, "en", vec![String::new()], vec![],
        ).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        let mut first = ocr::OcrLine {
            rotation: None,
            page: 0, text: "An erly paragraph.".into(), confidence: Some(0.95),
            x: 0.1, y: 0.1, width: 0.75, height: 0.04, correction: None,
            block_start: false, words: vec![], source_span: None,
        };
        let mut last = first.clone();
        last.text = "The final paragraph.".into();
        last.block_start = true;
        last.y = 0.5;
        super::super::write_ocr(&store.connection, &document.id, 0, "en", &[first.clone(), last], 1).unwrap();
        let frame = store.detached_pdf_ocr(&document.id, 0).unwrap().unwrap();
        let frame = store.correct_detached_pdf_ocr(
            &document.id, 0, &frame.revision, 0, Some("An early paragraph.".into()),
        ).unwrap();
        let last = store.select_detached_pdf_sentence(&document.id, 0, Some(1), None).unwrap().unwrap();
        store.update_detached_pdf_annotation(
            &document.id, 0, &last.sentence.id, DetachedPdfAnnotationChange::Note { text: "A note".into() },
        ).unwrap();
        first.text = "A recovered line.".into();
        first.y = 0.3;
        first.words = ["A", "recovered", "line."].into_iter().scan(0, |offset, text| {
            let word = ocr::OcrWord { text: text.into(), raw_start_utf16: Some(*offset),
                x: 0.1 + *offset as f64 / 40.0, y: first.y, width: 0.1, height: first.height, source_span: None };
            *offset += text.len() + 1;
            Some(word)
        }).collect();
        assert!(store.supplement_pdf_ocr_page(
            &document.id, 0, &frame.revision, "en", true, vec![first],
        ).unwrap());
        let updated = store.detached_pdf_ocr(&document.id, 0).unwrap().unwrap();
        assert_eq!(updated.lines.len(), 3);
        assert_eq!(updated.lines[0].reading_text(), "An early paragraph.");
        let saved = store.detached_pdf_sentence(&document.id, 0, None).unwrap().unwrap();
        assert_eq!(saved.sentence.text, "The final paragraph.");
        assert_eq!(saved.note.as_deref(), Some("A note"));
    }

    #[test]
    fn several_word_repairs_keep_the_saved_sentence_and_its_note() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("reader.db")).unwrap();
        let original = directory.path().join("scan.pdf");
        std::fs::write(&original, "%PDF-1.4\n").unwrap();
        let prepared = file_originals::prepare_pdf_import(
            store.pdf_original_root(), "scan.pdf", &original, "en", vec![String::new()], vec![],
        ).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        let initial = "A magc book has a magc cover and a story. A rnagic story follows.";
        let corrected = "A magic book has a magic cover and a story. A magic story follows.";
        let line = ocr::OcrLine {
            rotation: None,
            block_start: false,
            page: 0, text: initial.into(),
            x: 0.1, y: 0.1, width: 0.8, height: 0.1, confidence: Some(0.6),
            correction: None, words: vec![], source_span: None,
        };
        super::super::write_ocr(&store.connection, &document.id, 0, "en", &[line], 1).unwrap();
        let selected = store.select_detached_pdf_sentence(&document.id, 0, Some(1), None).unwrap().unwrap();
        assert_eq!(selected.sentence.text, "A rnagic story follows.");
        store.update_detached_pdf_annotation(
            &document.id, 0, &selected.sentence.id,
            DetachedPdfAnnotationChange::Note { text: "Remember this sentence".into() },
        ).unwrap();
        let mut save = |text| store.save_detached_pdf_word(&document.id, 0,
            serde_json::from_value(serde_json::json!({
                "startUtf16": initial.rfind(text).unwrap(), "selectedText": text,
                "revisionId": selected.revision_id, "translation": "переклад", "target": "uk",
            })).unwrap()).unwrap();
        let unchanged = save("story");
        let altered = save("rnagic");
        let frame = store.detached_pdf_ocr(&document.id, 0).unwrap().unwrap();
        store.reconcile_detached_pdf_ocr(&document.id, 0, &frame.revision, "en", |lines| {
            lines[0].correction = Some(corrected.into());
            Ok(true)
        }).unwrap();
        let saved = store.detached_pdf_sentence(&document.id, 0, None).unwrap().unwrap();
        assert_eq!(saved.sentence.text, "A magic story follows.");
        assert_eq!(saved.note.as_deref(), Some("Remember this sentence"));
        let links = serde_json::to_value(store.dictionary_source_locations(&unchanged.id).unwrap()).unwrap();
        assert_eq!(links.as_array().unwrap().len(), 1);
        assert_eq!(links[0]["sentenceId"], saved.sentence.id);
        assert_eq!(links[0]["selection"]["startUtf16"], corrected.rfind("story").unwrap());
        assert!(store.dictionary_source_locations(&altered.id).unwrap().is_empty());
    }
}
