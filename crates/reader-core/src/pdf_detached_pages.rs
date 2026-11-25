use crate::{ocr, pdf_sentences::Layout, Result, Sentence, Store};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use rusqlite::{params, Connection, OptionalExtension};

mod annotations;
mod find;
mod ocr_edit;
mod speech_resume;
mod translation;
pub(crate) mod word_break;
pub use translation::{DetachedPdfPassage, DetachedPdfTranslation, PdfPageTranslationRow, PdfSentenceRefresh, PdfTranslationSource};
pub(crate) use translation::canonical_ocr_search_projection;
pub use ocr_edit::DetachedPdfOcrPage;
pub use find::{DetachedPdfFindCursor, DetachedPdfFindPage, DetachedPdfSearchPage, DetachedPdfSearchPages};
pub use annotations::{DetachedPdfAnnotationChange, DetachedPdfAnnotationItem};

pub(crate) fn initialize(db: &mut Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS pdf_detached_pages (
            document_id TEXT NOT NULL REFERENCES file_originals(document_id) ON DELETE CASCADE,
            page INTEGER NOT NULL CHECK(page >= 0 AND page < 65535),
            ocr_language TEXT,
            text TEXT NOT NULL,
            lines TEXT,
            soft_lines INTEGER NOT NULL DEFAULT 5,
            PRIMARY KEY(document_id, page)
        );
        CREATE TABLE IF NOT EXISTS pdf_detached_positions (
            document_id TEXT NOT NULL,
            page INTEGER NOT NULL,
            sentence INTEGER NOT NULL CHECK(sentence >= 0),
            PRIMARY KEY(document_id,page),
            FOREIGN KEY(document_id,page) REFERENCES pdf_detached_pages(document_id,page) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS pdf_detached_speech_resume (
            document_id TEXT NOT NULL,
            page INTEGER NOT NULL,
            body TEXT NOT NULL,
            PRIMARY KEY(document_id,page),
            FOREIGN KEY(document_id,page) REFERENCES pdf_detached_pages(document_id,page) ON DELETE CASCADE
        );",
    )
    .map_err(database_error)?;
    annotations::initialize(db)?;
    upgrade_legacy_pages(db)
}

fn upgrade_legacy_pages(db: &mut Connection) -> Result<()> {
    let has_layout = |db: &Connection| {
        db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('pdf_detached_pages') WHERE name='soft_lines')",
            [], |row| row.get::<_, bool>(0)).map_err(database_error)
    };
    if has_layout(db)? {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(database_error)?;
    if !has_layout(&tx)? {
        tx.execute("ALTER TABLE pdf_detached_pages ADD COLUMN soft_lines INTEGER NOT NULL DEFAULT 0", [])
            .map_err(database_error)?;
        let pages = {
            let mut query = tx.prepare(
                "SELECT p.document_id,p.page FROM pdf_detached_pages p
                 WHERE NOT EXISTS(SELECT 1 FROM pdf_detached_speech_resume r WHERE r.document_id=p.document_id AND r.page=p.page)",
            ).map_err(database_error)?;
            let rows = query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u16>(1)?)))
                .map_err(database_error)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(database_error)?
        };
        for (id, page) in pages {
            let Some((text, _)) = read(&tx, &id, page)? else { continue; };
            let anchors = annotations::list(&tx, &id, page)?;
            let previous = crate::segment(&text, "");
            let current = crate::segment_pdf(&text, "");
            if !anchors_unchanged(&anchors, &previous, &current) {
                continue;
            }
            let saved: Option<usize> = tx.query_row(
                "SELECT sentence FROM pdf_detached_positions WHERE document_id=?1 AND page=?2",
                params![id, page], |row| Ok(row.get::<_, u32>(0)? as usize),
            ).optional().map_err(database_error)?;
            if let Some(position) = saved {
                let position = remap_position(position, &previous, &current);
                write_position(&tx, &id, page, position)?;
            }
            tx.execute("UPDATE pdf_detached_pages SET soft_lines=1 WHERE document_id=?1 AND page=?2",
                params![id, page]).map_err(database_error)?;
        }
    }
    tx.commit().map_err(database_error)
}

pub(crate) fn upgrade_sentence_layouts(db: &Connection, target: Layout, document_id: Option<&str>) -> Result<()> {
    let pages = {
        let mut statement = db.prepare(
            "SELECT p.document_id,p.page FROM pdf_detached_pages p
             WHERE p.document_id IN (
                 SELECT id FROM documents WHERE ?2 IS NULL UNION ALL SELECT ?2 WHERE ?2 IS NOT NULL
             )
             AND p.soft_lines<?1 AND (?1=4 OR (?1=5 AND p.soft_lines=4 AND instr(lower(p.text),'p.')>0)
                 OR (?1=6 AND p.soft_lines IN (4,5)))
             AND NOT EXISTS(
                 SELECT 1 FROM pdf_detached_speech_resume s WHERE s.document_id=p.document_id AND s.page=p.page
             ) AND (?1=4 OR NOT EXISTS(
                 SELECT 1 FROM dictionary_context_anchors a
                 WHERE a.document_id=p.document_id AND json_extract(a.anchor,'$.page')=p.page
             )
             )",
        ).map_err(database_error)?;
        let rows = statement.query_map(params![target as u8, document_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u16>(1)?)))
            .map_err(database_error)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(database_error)?
    };
    for (id, page) in pages {
        let (text, layout, saved): (String, u8, Option<u32>) = db.query_row(
            "SELECT p.text,p.soft_lines,c.sentence FROM pdf_detached_pages p
             LEFT JOIN pdf_detached_positions c ON c.document_id=p.document_id AND c.page=p.page
             WHERE p.document_id=?1 AND p.page=?2",
            params![id, page], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).map_err(database_error)?;
        if target == Layout::ListItems && !crate::pdf_sentences::has_list_marker(&text) {
            continue;
        }
        let prior = Layout::from_storage(layout);
        let before = prior.segment(&text, "");
        let after = target.segment(&text, "");
        if target == Layout::ListItems && before.iter().map(|s| (s.start_utf8, s.end_utf8))
            .eq(after.iter().map(|s| (s.start_utf8, s.end_utf8)))
        {
            continue;
        }
        if !anchors_unchanged(&annotations::list(db, &id, page)?, &before, &after) {
            continue;
        }
        if let Some(position) = saved {
            write_position(db, &id, page, remap_position(position as usize, &before, &after))?;
        }
        db.execute("UPDATE pdf_detached_pages SET soft_lines=?3 WHERE document_id=?1 AND page=?2",
            params![id, page, target as u8]).map_err(database_error)?;
    }
    Ok(())
}

fn anchors_unchanged(anchors: &[annotations::ArchivedAnnotation], before: &[Sentence], after: &[Sentence]) -> bool {
    anchors.iter().all(|anchor| {
        let index = |sentences: &[Sentence]| sentences.binary_search_by_key(&anchor.start_utf16, |s| s.start_utf16);
        match (index(before), index(after)) {
            (Ok(old), Ok(new)) => before[old].text == after[new].text,
            _ => false,
        }
    })
}

fn remap_position(position: usize, before: &[Sentence], after: &[Sentence]) -> usize {
    let offset = before.get(position).or_else(|| before.last()).map_or(0, |s| s.start_utf16);
    crate::pdf_sentences::position_at(after, offset)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedPdfSentence {
    pub sentence: Sentence,
    pub position: usize,
    pub total: usize,
    pub language: String,
    pub revision_id: String,
    pub bookmarked: bool,
    pub footnote: bool,
    pub note: Option<String>,
    pub reviewed_translations: std::collections::BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speech_resume: Option<crate::SpeechResume>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speech_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub narration_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pronunciation_text: Option<String>,
}

pub(crate) struct CachedSentences {
    pub(crate) language: String,
    pub(crate) text: String,
    pub(crate) saved: usize,
    pub(crate) sentences: Vec<Sentence>,
    pub(crate) revision: String,
}

impl Store {
    pub fn detached_pdf_sentence(
        &self, id: &str, page: u16, requested: Option<usize>,
    ) -> Result<Option<DetachedPdfSentence>> {
        self.select_detached_pdf_sentence(id, page, requested, None)
    }

    pub fn select_detached_pdf_sentence(
        &self, id: &str, page: u16, requested: Option<usize>, expected: Option<&str>,
    ) -> Result<Option<DetachedPdfSentence>> {
        self.select_detached_pdf_sentence_at(id, page, requested, expected, None)
    }

    pub fn select_detached_pdf_sentence_at(
        &self, id: &str, page: u16, requested: Option<usize>, expected: Option<&str>, source_offset: Option<usize>,
    ) -> Result<Option<DetachedPdfSentence>> {
        let Some(CachedSentences { language, text, saved, sentences, revision }) = self.detached_pdf_sentences(id, page)? else {
            return Ok(None);
        };
        let total = sentences.len();
        let from_offset = source_offset.map(|offset| {
            sentences.partition_point(|sentence| sentence.end_utf16 <= offset).min(total - 1)
        });
        let position = from_offset.or(requested).unwrap_or(saved).min(total - 1);
        let layout = self.detached_speech_layout(id, page, text, &language)?;
        let footnote = crate::document_layout::sentence_in_footnote(&layout.blocks, &sentences[position]);
        let (speech_text, narration_text) = if footnote { (None, None) } else {
            narration_text(&self.connection, id, page, &layout, &sentences[position])?
        };
        let checkpoint = speech_resume::read(&self.connection, id, page)?;
        let speech_resume = checkpoint.as_ref()
            .filter(|checkpoint| checkpoint.position == position && speech_resume::matches_original(checkpoint, &sentences))
            .and_then(|checkpoint| checkpoint.for_sentences(&sentences, &language, |_| speech_text.as_deref().or(narration_text.as_deref())));
        let sentence = sentences.into_iter().nth(position).ok_or("Речення PDF не знайдено.")?;
        if expected.is_some_and(|expected| sentence.id != expected) {
            return Err("Речення в результатах пошуку змінилося.".into());
        }
        if requested.is_some() || source_offset.is_some() {
            write_position(&self.connection, id, page, position)?;
        }
        let pronunciation_text = if !footnote && speech_text.is_none() {
            crate::speech::pdf_narration(&layout, &sentence, narration_text.as_deref().unwrap_or(&sentence.text))
        } else { None };
        let (bookmarked, note) = annotations::read(&self.connection, id, page, sentence.start_utf16)?;
        let reviewed_translations = annotations::reviewed(&self.connection, id, page, sentence.start_utf16)?;
        Ok(Some(DetachedPdfSentence {
            sentence, position, total, language, revision_id: revision, bookmarked, note, footnote,
            reviewed_translations, speech_resume: speech_resume.filter(|_| !footnote), speech_text, narration_text, pronunciation_text,
        }))
    }
    pub(crate) fn detached_pdf_speech_source(
        &self, id: &str, page: u16, sentence_id: &str, next: bool,
    ) -> Result<Option<(String, crate::speech::SpeechSource)>> {
        let CachedSentences { language, text, saved, sentences, .. } = self.detached_pdf_sentences(id, page)?
            .ok_or("Текст сторінки PDF недоступний.")?;
        let position = saved.min(sentences.len() - 1);
        if sentences[position].id != sentence_id {
            return Err("Позиція читання сторінки PDF змінилася.".into());
        }
        let start = position + usize::from(next);
        let layout = self.detached_speech_layout(id, page, text, &language)?;
        self.speech_on_detached_page(id, page, language, &sentences, &layout, start, next)
    }

    pub(crate) fn first_detached_pdf_speech_source(
        &self, id: &str, page: u16,
    ) -> Result<Option<(String, crate::speech::SpeechSource)>> {
        let Some(CachedSentences { language, text, sentences, .. }) = self.detached_pdf_sentences(id, page)? else {
            return Ok(None);
        };
        let layout = self.detached_speech_layout(id, page, text, &language)?;
        self.speech_on_detached_page(id, page, language, &sentences, &layout, 0, true)
    }

    fn detached_speech_layout(&self, id: &str, page: u16, text: String, language: &str) -> Result<crate::Document> {
        let lines = self.detached_pdf_ocr(id, page)?.map(|ocr| ocr.lines);
        Ok(crate::document_layout::detached_pdf_document(text, language, lines))
    }

    fn speech_on_detached_page(
        &self, id: &str, page: u16, language: String, sentences: &[Sentence],
        layout: &crate::Document, start: usize, skip_consumed: bool,
    ) -> Result<Option<(String, crate::speech::SpeechSource)>> {
        for sentence in sentences.iter().skip(start) {
            if crate::document_layout::sentence_in_footnote(&layout.blocks, sentence) {
                if !skip_consumed { break; }
                continue;
            }
            let (speech, narration) = narration_text(&self.connection, id, page, layout, sentence)?;
            let text = speech.unwrap_or_else(|| {
                let text = narration.unwrap_or_else(|| sentence.text.clone());
                crate::speech::pdf_narration(layout, sentence, &text).unwrap_or(text)
            });
            if crate::speech::has_narration_text(&text) {
                return Ok(Some((sentence.id.clone(), crate::speech::SpeechSource { language, text })));
            }
            if !skip_consumed {
                break;
            }
        }
        Ok(None)
    }

    pub(crate) fn detached_pdf_sentences(&self, id: &str, page: u16) -> Result<Option<CachedSentences>> {
        let result: Option<(String, String, usize, u8)> = self.connection.query_row(
            "SELECT p.text, json_extract(d.body, '$.language'), coalesce(c.sentence,0),p.soft_lines
             FROM pdf_detached_pages p JOIN documents d ON d.id=p.document_id
             LEFT JOIN pdf_detached_positions c ON c.document_id=p.document_id AND c.page=p.page
             WHERE p.document_id=?1 AND p.page=?2",
            params![id, page],
            |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, u32>(2)? as usize, row.get(3)?)),
        ).optional().map_err(database_error)?;
        let Some((text, language, saved, soft_lines)) = result else { return Ok(None); };
        let revision = page_revision(&text);
        let sentences = Layout::from_storage(soft_lines).segment(&text, &sentence_prefix(id, page, &revision));
        Ok((!sentences.is_empty()).then_some(CachedSentences { language, text, saved, sentences, revision }))
    }
}

fn write_position(db: &Connection, id: &str, page: u16, position: usize) -> Result<()> {
    db.execute(
        "INSERT INTO pdf_detached_positions (document_id,page,sentence)
         SELECT document_id,page,?3 FROM pdf_detached_pages WHERE document_id=?1 AND page=?2
         ON CONFLICT(document_id,page) DO UPDATE SET sentence=excluded.sentence",
        params![id, page, sqlite_offset(position)?],
    ).map_err(database_error)?;
    Ok(())
}

pub(crate) fn has_local_edits(db: &Connection, id: &str, page: u16) -> Result<bool> {
    if annotations::has_annotations(db, id, page)? {
        return Ok(true);
    }
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pdf_detached_pages, json_each(lines)
         WHERE document_id=?1 AND page=?2 AND json_type(value, '$.correction')='text')",
        params![id, page], |row| row.get(0),
    ).map_err(database_error)
}

/// A present row with no language contains a native text layer.
pub(crate) fn source_language(db: &Connection, id: &str, page: u16) -> Result<Option<Option<String>>> {
    db.query_row(
        "SELECT ocr_language FROM pdf_detached_pages WHERE document_id=?1 AND page=?2",
        params![id, page],
        |row| row.get(0),
    )
    .optional()
    .map_err(database_error)
}

fn narration_text(
    db: &Connection, id: &str, page: u16, layout: &crate::Document, sentence: &Sentence,
) -> Result<(Option<String>, Option<String>)> {
    let speech = annotations::speech_text(db, id, page, sentence.start_utf16)?;
    let narration = if speech.is_none() {
        word_break::narration(db, id, page, layout, sentence)?
    } else {
        None
    };
    Ok((speech, narration))
}

pub(crate) fn read(db: &Connection, id: &str, page: u16) -> Result<Option<(String, Option<String>)>> {
    db.query_row(
        "SELECT text,ocr_language FROM pdf_detached_pages WHERE document_id=?1 AND page=?2",
        params![id, page],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(database_error)
}

pub(crate) fn write_text_layer(db: &Connection, id: &str, page: u16, text: &str) -> Result<()> {
    write(db, id, page, text, None, None, Layout::ListItems)
}

pub(crate) fn replace_native_layer(
    db: &Connection, id: &str, page: u16, expected: &str, language: &str,
    lines: &[ocr::OcrLine], page_count: u16,
) -> Result<bool> {
    let native: bool = db.query_row(
        "SELECT EXISTS (SELECT 1 FROM pdf_detached_pages
         WHERE document_id=?1 AND page=?2 AND ocr_language IS NULL AND lines IS NULL AND text=?3)",
        params![id, page, expected], |row| row.get(0),
    ).map_err(database_error)?;
    if !native || has_local_edits(db, id, page)? || speech_resume::read(db, id, page)?.is_some() {
        return Ok(false);
    }
    let edit = ocr::TranscriptEdit { start_utf16: 0, old_len: expected.encode_utf16().count(), new_len: 0 };
    if crate::dictionary_transfer::selection_overlaps_layout_edits(
        db, id, Some(page), &page_revision(expected), &[edit],
    )? {
        return Ok(false);
    }
    write_ocr(db, id, page, language, lines, page_count)?;
    Ok(true)
}

pub(crate) fn repair_cached_text_order(
    db: &Connection, id: &str, page: u16, reference: &str, corrected: &str, edits: &[ocr::TranscriptEdit],
    moves: &[ocr::TranscriptMove],
) -> Result<bool> {
    let current: Option<(Option<i64>, u8)> = db.query_row(
        "SELECT saved.sentence, p.soft_lines FROM pdf_detached_pages p
         LEFT JOIN pdf_detached_positions saved ON saved.document_id=p.document_id AND saved.page=p.page
         WHERE p.document_id=?1 AND p.page=?2 AND p.ocr_language IS NULL AND p.lines IS NULL AND p.text=?3",
        params![id, page, reference], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(database_error)?;
    let Some((saved, layout)) = current else { return Ok(false); };
    let layout = Layout::from_storage(layout);
    let old_revision = page_revision(reference);
    let new_revision = page_revision(corrected);
    let before = layout.segment(reference, &sentence_prefix(id, page, &old_revision));
    let after = layout.segment(corrected, &sentence_prefix(id, page, &new_revision));
    let find = |old: &Sentence| {
        if let Some(index) = ocr::preserved_sentence_index(old, &after, edits, moves) {
            return Some(index);
        }
        let start = ocr::map_transcript_offset(old.start_utf16, edits)?;
        let end = ocr::map_transcript_offset(old.end_utf16, edits)?;
        let index = after.binary_search_by_key(&start, |sentence| sentence.start_utf16).ok()?;
        (after[index].end_utf16 == end).then_some(index)
    };
    let mut anchors = annotations::list(db, id, page)?;
    for anchor in &mut anchors {
        let Ok(old) = before.binary_search_by_key(&anchor.start_utf16, |sentence| sentence.start_utf16)
            else { return Ok(false); };
        let Some(index) = find(&before[old]) else { return Ok(false); };
        anchor.start_utf16 = after[index].start_utf16;
    }
    anchors.sort_unstable_by_key(|anchor| anchor.start_utf16);
    if anchors.windows(2).any(|pair| pair[0].start_utf16 == pair[1].start_utf16) {
        return Ok(false);
    }
    crate::dictionary_transfer::reanchor_selections(
        db, (id, Some(page), &old_revision, &new_revision), &before, &after,
        |_, start, length| ocr::map_preserved_range_start(start, length, edits, moves),
    )?;
    write(db, id, page, corrected, None, None, layout)?;
    if !anchors.is_empty() { annotations::replace_page(db, id, page, &anchors)?; }
    if let Some(sentence) = saved.and_then(|position| before.get(position as usize)) {
        let position = find(sentence).unwrap_or_else(|| {
            let offset = ocr::map_transcript_offset(sentence.start_utf16, edits).unwrap_or(sentence.start_utf16);
            crate::pdf_sentences::position_at(&after, offset)
        });
        write_position(db, id, page, position)?;
    }
    speech_resume::remap_page(db, id, page, &before, &after, |sentence|
        ocr::map_preserved_range_start(sentence.start_utf16, sentence.end_utf16 - sentence.start_utf16, edits, moves))?;
    Ok(true)
}

pub(crate) fn write_ocr(
    db: &Connection, id: &str, page: u16, language: &str, lines: &[ocr::OcrLine], page_count: u16,
) -> Result<()> {
    let text = ocr::recognized_lines(lines, page_count)?;
    let payload = serde_json::to_string(lines).map_err(database_error)?;
    let layout = if let Some((previous, _)) = read(db, id, page)? {
        if previous != text {
            ocr_edit::reanchor_replaced_ocr(db, id, page, &previous, &text)?;
        }
        page_layout(db, id, page)?
    } else {
        Layout::ListItems
    };
    write(db, id, page, &text, Some(language), Some(&payload), layout)
}

fn write(
    db: &Connection, id: &str, page: u16, text: &str, language: Option<&str>, lines: Option<&str>, layout: Layout,
) -> Result<()> {
    db.execute(
        "INSERT INTO pdf_detached_pages (document_id, page, ocr_language, text, lines, soft_lines)
         VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(document_id,page) DO UPDATE SET
            ocr_language=excluded.ocr_language, text=excluded.text, lines=excluded.lines",
        params![id, page, language, text, lines, layout as u8],
    )
    .map_err(database_error)?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ArchivedPage {
    pub page: u16,
    pub text: String,
    pub language: Option<String>,
    pub lines: Option<Vec<ocr::OcrLine>>,
    pub position: Option<usize>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    soft_lines: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    structured_sentences: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    abbreviation_boundaries: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    quote_boundaries: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    reference_boundaries: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    list_item_boundaries: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<annotations::ArchivedAnnotation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    speech_resume: Option<speech_resume::ArchivedSpeechResume>,
}

impl ArchivedPage {
    fn layout(&self) -> Layout {
        if self.list_item_boundaries {
            Layout::ListItems
        } else if self.reference_boundaries {
            Layout::PageReferences
        } else if self.quote_boundaries {
            Layout::ClosingQuotes
        } else if self.abbreviation_boundaries {
            Layout::Titles
        } else if self.structured_sentences {
            Layout::Structured
        } else {
            Layout::from_storage(self.soft_lines as u8)
        }
    }

    fn set_layout(&mut self, layout: Layout) {
        self.soft_lines = layout != Layout::Literal;
        self.structured_sentences = (layout as u8) >= Layout::Structured as u8;
        self.abbreviation_boundaries = (layout as u8) >= Layout::Titles as u8;
        self.quote_boundaries = (layout as u8) >= Layout::ClosingQuotes as u8;
        self.reference_boundaries = (layout as u8) >= Layout::PageReferences as u8;
        self.list_item_boundaries = layout == Layout::ListItems;
    }

    pub(crate) fn validate(&self, document: &crate::Document, page_count: u16) -> Result<()> {
        if self.page >= page_count || self.text.trim().is_empty()
            || self.text.len() > crate::MAX_IMPORT || self.text.contains('\0')
            || document.pdf_page_starts.as_ref().and_then(|starts| starts.get(usize::from(self.page)))
                != Some(&None)
            || self.position.is_some_and(|position| !has_sentence(&self.text, position, self.layout()))
        {
            return Err("Некоректна окрема сторінка PDF у резервній копії.".into());
        }

        if !self.annotations.is_empty() {
            let sentences = self.layout().segment(&self.text, "");
            if self.annotations.windows(2).any(|pair| pair[0].start_utf16 >= pair[1].start_utf16) {
                return Err("Некоректний порядок приміток до сторінки PDF.".into());
            }
            for item in &self.annotations {
                if (!item.bookmarked && item.note.is_none() && item.translations.is_empty() && item.speech_text.is_none())
                    || sentences.binary_search_by_key(&item.start_utf16, |sentence| sentence.start_utf16).is_err()
                {
                    return Err("Примітка до сторінки PDF не має речення.".into());
                }
                if let Some(note) = &item.note {
                    annotations::validate_note(note)?;
                    if note.trim().is_empty() {
                        return Err("Порожня нотатка до сторінки PDF.".into());
                    }
                }
                if let Some(text) = &item.speech_text {
                    crate::validate_speech_text(text)?;
                    if text.trim().is_empty() {
                        return Err("Порожній текст озвучення PDF у резервній копії.".into());
                    }
                }
                for (target, text) in &item.translations {
                    crate::valid_language(target)?;
                    crate::validate_translation(text)?;
                    if text.trim().is_empty() {
                        return Err("Некоректний переклад речення PDF у резервній копії.".into());
                    }
                }
            }
        }
        if let Some(checkpoint) = &self.speech_resume {
            let prefix = sentence_prefix(&document.id, self.page, &page_revision(&self.text));
            let sentences = self.layout().segment(&self.text, &prefix);
            if !speech_resume::matches_original(checkpoint, &sentences)
                || checkpoint.for_archive(&sentences, &document.language,
                    |start| annotations::archived_speech_text(&self.annotations, start)).is_none()
            {
                return Err("Некоректна позиція озвучення PDF у резервній копії.".into());
            }
        }
        match (&self.language, &self.lines) {
            (None, None) => Ok(()),
            (Some(language), Some(lines)) if lines.iter().all(|line| line.page == self.page) =>
            {
                crate::valid_language(language)?;
                if ocr::recognized_lines(lines, page_count)? != self.text {
                    return Err("Текст сторінки PDF не відповідає збереженому OCR.".into());
                }
                Ok(())
            }
            _ => Err("Некоректне джерело тексту сторінки PDF.".into()),
        }
    }
}

fn has_sentence(text: &str, position: usize, layout: Layout) -> bool {
    layout.segment(text, "").get(position).is_some()
}

fn page_layout(db: &Connection, id: &str, page: u16) -> Result<Layout> {
    db.query_row("SELECT soft_lines FROM pdf_detached_pages WHERE document_id=?1 AND page=?2",
        params![id, page], |row| row.get(0)).map(Layout::from_storage).map_err(database_error)
}

pub(crate) fn has_boundary_speech_resume(db: &Connection) -> Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pdf_detached_speech_resume WHERE json_type(body, '$.boundaryText')='text')",
        [], |row| row.get(0),
    ).map_err(database_error)
}

pub(crate) fn has_cached_pages(db: &Connection, id: &str) -> Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pdf_detached_pages WHERE document_id=?1)",
        [id],
        |row| row.get(0),
    )
    .map_err(database_error)
}

pub(crate) fn visit_cached_pages(
    db: &Connection, id: &str, mut visit: impl FnMut(ArchivedPage) -> Result<()>,
) -> Result<()> {
    let mut query = db.prepare(
        "SELECT p.page,p.text,p.ocr_language,p.lines,c.sentence,json_extract(d.body,'$.language'),p.soft_lines
         FROM pdf_detached_pages p
         JOIN documents d ON d.id=p.document_id
         LEFT JOIN pdf_detached_positions c ON c.document_id=p.document_id AND c.page=p.page
         WHERE p.document_id=?1 ORDER BY p.page",
    ).map_err(database_error)?;
    let mut rows = query.query([id]).map_err(database_error)?;
    while let Some(row) = rows.next().map_err(database_error)? {
        let encoded: Option<String> = row.get(3).map_err(database_error)?;
        let text: String = row.get(1).map_err(database_error)?;
        let position = row.get::<_, Option<u32>>(4).map_err(database_error)?.map(|value| value as usize);
        let page = row.get(0).map_err(database_error)?;
        let language: String = row.get(5).map_err(database_error)?;
        let layout = Layout::from_storage(row.get(6).map_err(database_error)?);
        let annotations = annotations::list(db, id, page)?;
        let checkpoint = speech_resume::read(db, id, page)?.filter(|checkpoint| {
            let prefix = sentence_prefix(id, page, &page_revision(&text));
            let sentences = layout.segment(&text, &prefix);
            speech_resume::matches_original(checkpoint, &sentences)
                && checkpoint.for_archive(&sentences, &language,
                    |start| annotations::archived_speech_text(&annotations, start)).is_some()
        });
        visit(ArchivedPage {
            page,
            speech_resume: checkpoint,
            annotations,
            position: position.filter(|&position| has_sentence(&text, position, layout)),
            soft_lines: layout != Layout::Literal,
            structured_sentences: (layout as u8) >= Layout::Structured as u8,
            abbreviation_boundaries: (layout as u8) >= Layout::Titles as u8,
            quote_boundaries: (layout as u8) >= Layout::ClosingQuotes as u8,
            reference_boundaries: (layout as u8) >= Layout::PageReferences as u8,
            list_item_boundaries: layout == Layout::ListItems,
            text,
            language: row.get(2).map_err(database_error)?,
            lines: encoded.as_deref().map(serde_json::from_str).transpose().map_err(database_error)?,
        })?;
    }
    Ok(())
}

/// Keep a local page and cursor when present. A cursor only transfers to identical text.
pub(crate) fn adopt_cached_page(db: &Connection, document: &crate::Document, mut page: ArchivedPage) -> Result<()> {
    if document.pdf_page_starts.as_ref().and_then(|starts| starts.get(usize::from(page.page)))
        != Some(&None)
    {
        return Ok(());
    }
    if let Some((text, _)) = read(db, &document.id, page.page)? {
        if text != page.text {
            return Ok(());
        }
        let local_mode = page_layout(db, &document.id, page.page)?;
        if local_mode != page.layout() {
            let from = page.layout().segment(&text, "");
            let to = local_mode.segment(&text, "");
            if !anchors_unchanged(&page.annotations, &from, &to) {
                return Err("Архів і локальна копія PDF мають різні межі для збережених приміток.".into());
            }
            page.position = page.position.map(|position| remap_position(position, &from, &to));
            page.speech_resume = page.speech_resume.and_then(|checkpoint| checkpoint.remap(&from, &to));
            page.set_layout(local_mode);
        }
    } else {
        if (page.layout() as u8) < Layout::ClosingQuotes as u8 && page.speech_resume.is_none() {
            let previous = page.layout().segment(&page.text, "");
            let current = Layout::ClosingQuotes.segment(&page.text, "");
            if anchors_unchanged(&page.annotations, &previous, &current) {
                page.position = page.position.map(|position| remap_position(position, &previous, &current));
                page.set_layout(Layout::ClosingQuotes);
            }
        }
        let payload = page.lines.as_ref().map(serde_json::to_string).transpose().map_err(database_error)?;
        write(db, &document.id, page.page, &page.text, page.language.as_deref(), payload.as_deref(), page.layout())?;
        db.execute(
            "DELETE FROM pdf_ocr_completed_pages WHERE document_id=?1 AND page=?2",
            params![document.id, page.page],
        ).map_err(database_error)?;
        db.execute(
            "DELETE FROM pdf_ocr_requests WHERE document_id=?1 AND page=?2",
            params![document.id, page.page],
        ).map_err(database_error)?;
    }
    annotations::merge(db, &document.id, page.page, &page.annotations)?;
    let layout = page.layout();
    if let Some(checkpoint) = page.speech_resume {
        let prefix = sentence_prefix(&document.id, page.page, &page_revision(&page.text));
        let sentences = layout.segment(&page.text, &prefix);
        let annotations = annotations::list(db, &document.id, page.page)?;
        let speech_text = |start| annotations::archived_speech_text(&annotations, start);
        let local_valid = speech_resume::read(db, &document.id, page.page)?.is_some_and(|local| {
            speech_resume::matches_original(&local, &sentences)
                && local.for_archive(&sentences, &document.language, speech_text).is_some()
        });
        if !local_valid {
            if let Some(checkpoint) = speech_resume::rebind(checkpoint, &sentences, &document.language, speech_text) {
                speech_resume::write(db, &document.id, page.page, &checkpoint)?;
            }
        }
    }
    if let Some(position) = page.position {
        db.execute(
            "INSERT OR IGNORE INTO pdf_detached_positions (document_id,page,sentence) VALUES (?1,?2,?3)",
            params![document.id, page.page, sqlite_offset(position)?],
        ).map_err(database_error)?;
    }
    Ok(())
}

fn page_revision(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn sentence_prefix(document_id: &str, page: u16, revision: &str) -> String {
    format!("{document_id}:{page}:{revision}")
}

fn sqlite_offset(value: usize) -> Result<u32> {
    u32::try_from(value).map_err(database_error)
}

fn database_error(error: impl std::fmt::Display) -> String {
    format!("Помилка кешу сторінки PDF. {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrades_and_archives_page_references_on_cached_detached_pages() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reader.db");
        let original = root.path().join("book.pdf");
        std::fs::write(&original, b"%PDF-1.4\n").unwrap();
        let mut store = Store::open(&path).unwrap();
        let prepared = crate::file_originals::prepare_pdf_import(
            store.pdf_original_root(), "book.pdf", &original, "fr", vec![String::new()], vec![],
        ).unwrap();
        let doc = store.import_pdf_file(prepared).unwrap();
        let text = "Consultez la fiche, p. 189. Puis revenez.";
        write(&store.connection, &doc.id, 0, text, None, None, Layout::ClosingQuotes).unwrap();
        write_position(&store.connection, &doc.id, 0, 2).unwrap();
        store.connection.pragma_update(None, "user_version", 7).unwrap();
        drop(store);

        let store = Store::open(&path).unwrap();
        assert!(page_layout(&store.connection, &doc.id, 0).unwrap() == Layout::PageReferences);
        let current = store.detached_pdf_sentence(&doc.id, 0, None).unwrap().unwrap();
        assert_eq!(current.position, 1);
        assert_eq!(current.sentence.text, "Puis revenez.");
        visit_cached_pages(&store.connection, &doc.id, |page| {
            let restored: ArchivedPage = serde_json::from_str(&serde_json::to_string(&page).unwrap()).unwrap();
            assert!(restored.layout() == Layout::PageReferences);
            assert_eq!(restored.position, Some(1));
            Ok(())
        }).unwrap();
    }

    #[test]
    fn portable_restore_upgrades_detached_exercise_boundaries_and_keeps_reading_position() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("book.pdf");
        let archive = root.path().join("library.zip");
        std::fs::write(&original, b"%PDF-1.4\n").unwrap();
        let mut source = Store::open(&root.path().join("source.db")).unwrap();
        let prepared = crate::file_originals::prepare_pdf_import(
            source.pdf_original_root(), "book.pdf", &original, "fr", vec![String::new()], vec![],
        ).unwrap();
        let document = source.import_pdf_file(prepared).unwrap();
        let text = "11. Préparez votre essai.";
        write(&source.connection, &document.id, 0, text, None, None, Layout::PageReferences).unwrap();
        let legacy = source.detached_pdf_sentence(&document.id, 0, Some(1)).unwrap().unwrap();
        assert_eq!(legacy.sentence.text, "Préparez votre essai.");
        source.export_backup_file(&archive).unwrap();

        let mut target = Store::open(&root.path().join("target.db")).unwrap();
        target.restore_backup_file(&archive).unwrap();
        let restored = target.detached_pdf_sentence(&document.id, 0, None).unwrap().unwrap();
        assert_eq!(restored.sentence.text, text);
        assert_eq!(restored.position, 0);
        assert!(page_layout(&target.connection, &document.id, 0).unwrap() == Layout::ListItems);
    }

    #[test]
    fn backup_restores_detached_audio_after_an_earlier_sentence_boundary_changes() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("book.pdf");
        let archive = root.path().join("library.zip");
        std::fs::write(&original, b"%PDF-1.4\n").unwrap();
        let import = |store: &mut Store| {
            let prepared = crate::file_originals::prepare_pdf_import(
                store.pdf_original_root(), "book.pdf", &original, "fr",
                vec![String::new(); 2], vec![],
            ).unwrap();
            store.import_pdf_file(prepared).unwrap()
        };
        let text = "Le marchand, M. Morrel, attend sur le quai. Il regarde le bateau.";
        let spoken = "Il regarde le bateau.";
        let page = 1;

        let mut source = Store::open(&root.path().join("source.db")).unwrap();
        let archived = import(&mut source);
        write(&source.connection, &archived.id, page, text, None, None, Layout::Structured).unwrap();
        let old_position = Layout::Structured.segment(text, "").iter().position(|s| s.text == spoken).unwrap();
        source.set_pdf_reading_page(&archived.id, page).unwrap();
        let old_sentence = source.detached_pdf_sentence(&archived.id, page, Some(old_position)).unwrap().unwrap().sentence;
        source.set_detached_pdf_speech_resume(&archived.id, page, &old_sentence.id, Some(crate::SpeechResume {
            sentence_id: old_sentence.id.clone(),
            text_hash: format!("{:x}", Sha256::digest(spoken.as_bytes())),
            offset_utf16: 3,
            model: None,
            boundary_text: None,
        })).unwrap();
        source.export_backup_file(&archive).unwrap();

        let mut destination = Store::open(&root.path().join("destination.db")).unwrap();
        let local = import(&mut destination);
        assert_ne!(local.id, archived.id);
        write_text_layer(&destination.connection, &local.id, page, text).unwrap();
        let new_position = Layout::ClosingQuotes.segment(text, "").iter().position(|s| s.text == spoken).unwrap();
        assert_ne!(old_position, new_position);
        destination.restore_backup_file(&archive).unwrap();

        assert_eq!(destination.pdf_reading_page(&local.id).unwrap(), Some(page));
        let resumed = destination.detached_pdf_sentence(&local.id, page, None).unwrap().unwrap();
        assert_eq!(resumed.position, new_position);
        assert_eq!(resumed.sentence.text, spoken);
        let checkpoint = resumed.speech_resume.unwrap();
        assert_eq!(checkpoint.sentence_id, resumed.sentence.id);
        assert_eq!(checkpoint.offset_utf16, 3);
    }
}
