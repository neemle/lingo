use super::{annotations, database_error, page_revision, sqlite_offset};
use crate::{ocr, Result, Sentence, Store};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedPdfOcrPage {
    revision: String,
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
        let edit = Edit::between(
            line_start,
            line.reading_text(),
            correction.as_deref().unwrap_or(line.text.trim()),
        );
        line.correction = correction;
        let updated = save_ocr_edits(&tx, id, page, &old_text, previous.lines, &[edit])?;
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
        let uncorrected: Vec<_> = previous.lines.iter().map(|line| line.correction.is_none()).collect();
        if !reconcile(&mut previous.lines)? {
            return Ok(false);
        }
        let edits = previous.lines.iter().zip(uncorrected)
            .filter(|(line, was_raw)| *was_raw && line.correction.is_some())
            .map(|(line, _)| {
                let start = line.source_span.ok_or("Розпізнаний рядок не має позиції в тексті.")?.start_utf16;
                Ok(Edit::between(start, line.text.trim(), line.reading_text()))
            }).collect::<Result<Vec<_>>>()?;
        save_ocr_edits(&tx, id, page, &old_text, previous.lines, &edits)?;
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
        move_sentence_anchors(db, id, page, old_text, &text, edits)?;
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

fn ocr_page(text: &str, encoded: &str) -> Result<DetachedPdfOcrPage> {
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

fn move_sentence_anchors(
    db: &Connection,
    id: &str,
    page: u16,
    old: &str,
    new: &str,
    edits: &[Edit],
) -> Result<()> {
    let map_start = |offset| edits.iter().rev().fold(offset, |offset, edit| edit.map_start(offset));
    let layout = super::page_layout(db, id, page)?;
    let old_sentences = layout.segment(old, "");
    let new_sentences = layout.segment(new, "");
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
