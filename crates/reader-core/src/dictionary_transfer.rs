use crate::practice::{self, PracticeProgress};
use crate::review_schedule::MAX_INTERVAL_DAYS;
use crate::{valid_language, Document, Result, ReviewEvent, ReviewState, Store, Vocabulary};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryArchive {
    pub version: u32,
    pub entries: Vec<Vocabulary>,
    pub reviews: Vec<DictionaryReview>,
    pub sources: Vec<DictionarySource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub practice: Option<PracticeProgress>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryReview {
    pub id: String,
    #[serde(flatten)]
    pub event: ReviewEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionarySource {
    pub document_id: String,
    pub title: Option<String>,
    pub source_hash: Option<String>,
    #[serde(default)]
    pub entry_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<DictionaryExample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryExample {
    entry_id: String,
    surface: String,
    sentence: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    anchors: Vec<DictionarySentenceAnchor>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DictionarySourceSelection {
    pub(crate) start_utf16: usize,
    pub(crate) text: String,
}

impl DictionarySourceSelection {
    pub(crate) fn from_selection(start_utf16: usize, text: &str) -> Option<Self> {
        let trimmed = text.trim_start();
        let start_utf16 = start_utf16.checked_add(text[..text.len() - trimmed.len()].encode_utf16().count())?;
        let text = trimmed.trim_end().to_owned();
        let selection = Self { start_utf16, text };
        selection.is_valid(start_utf16).then_some(selection)
    }

    fn is_valid(&self, sentence_start: usize) -> bool {
        self.start_utf16 >= sentence_start
            && self.start_utf16 <= crate::MAX_IMPORT
            && !self.text.trim().is_empty()
            && self.text.len() <= 500
            && !self.text.contains('\0')
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DictionarySentenceAnchor {
    pub(crate) sentence_id: String,
    pub(crate) revision_id: String,
    pub(crate) page: Option<u16>,
    pub(crate) sentence_start_utf16: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selection: Option<DictionarySourceSelection>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DictionarySelection {
    pub(crate) revision_id: String,
    pub(crate) start_utf16: usize,
    pub(crate) reconstructed_text: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionarySourceLocation {
    document_id: String,
    title: String,
    sentence: String,
    sentence_id: String,
    revision_id: String,
    page: Option<u16>,
    position: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    selection: Option<DictionarySourceSelection>,
}

impl Store {
    pub fn dictionary_source_locations(&self, card_id: &str) -> Result<Vec<DictionarySourceLocation>> {
        let mut query = self.connection.prepare(
            "SELECT d.id, json_extract(d.body,'$.title'), a.sentence, a.anchor
             FROM dictionary_context_anchors a
             JOIN dictionary_sources s ON s.document_id=a.document_id
             JOIN documents d ON d.id=coalesce(
                 (SELECT id FROM documents WHERE source_hash=s.source_hash), s.document_id)
             WHERE a.card_id=?1
             ORDER BY d.id, a.sentence, a.anchor",
        ).map_err(error)?;
        let rows = query.query_map([card_id], |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?,
            row.get::<_, String>(2)?, row.get::<_, String>(3)?,
        ))).map_err(error)?;
        let mut locations = Vec::new();
        let mut seen = BTreeSet::new();
        let mut canonical = None;
        for row in rows {
            let (document, title, text, anchor) = row.map_err(error)?;
            let anchor: DictionarySentenceAnchor = serde_json::from_str(&anchor).map_err(error)?;
            let resolved = if let Some(page) = anchor.page {
                let Some(cached) = self.detached_pdf_sentences(&document, page)? else { continue; };
                location_in_revision(&anchor, &text, &cached.revision, &cached.sentences, Some(&cached.text))
            } else {
                if !canonical.as_ref().is_some_and(|loaded: &Document| loaded.id == document) {
                    canonical = Some(self.token_document(&document)?);
                }
                let loaded = canonical.as_ref().expect("document loaded");
                let Some(revision) = loaded.current_revision_id.as_deref() else { continue; };
                location_in_revision(&anchor, &text, revision, &loaded.sentences, loaded.pdf_soft_lines.then_some(loaded.text.as_str()))
            };
            if let Some((position, sentence_id, revision_id, selection)) = resolved {
                if seen.insert((document.clone(), anchor.page, position, selection.as_ref().map(|item| item.start_utf16))) {
                    locations.push(DictionarySourceLocation {
                        document_id: document, title, sentence: text, sentence_id, revision_id,
                        page: anchor.page, position, selection,
                    });
                }
            }
        }
        locations.sort_by(|left, right| {
            let start = |location: &DictionarySourceLocation|
                location.selection.as_ref().map(|selection| selection.start_utf16);
            (&left.title, &left.document_id, left.page, left.position, start(left))
                .cmp(&(&right.title, &right.document_id, right.page, right.position, start(right)))
        });
        Ok(locations)
    }
}

fn location_in_revision(
    anchor: &DictionarySentenceAnchor, text: &str, revision: &str, sentences: &[crate::Sentence],
    source: Option<&str>,
) -> Option<(usize, String, String, Option<DictionarySourceSelection>)> {
    let offset = anchor.sentence_start_utf16;
    let position = sentences.partition_point(|sentence| sentence.end_utf16 <= offset);
    let sentence = sentences.get(position)?;
    let same_sentence = offset == sentence.start_utf16 && sentence.text == text;
    if let Some(selection) = anchor.selection.as_ref().filter(|_| same_sentence || revision == anchor.revision_id) {
        let position = sentences.partition_point(|sentence| sentence.end_utf16 <= selection.start_utf16);
        if let Some(candidate) = sentences.get(position) {
            let resolved = selection.start_utf16.checked_sub(candidate.start_utf16)
                .and_then(|relative| crate::ocr::byte_at_utf16(&candidate.text, relative));
            if resolved.and_then(|start| candidate.text.get(start..)).is_some_and(|tail| tail.starts_with(&selection.text)) {
                return Some((position, candidate.id.clone(), revision.to_owned(), Some(selection.clone())));
            }
        }
    }
    if same_sentence {
        return Some((position, sentence.id.clone(), revision.to_owned(), None));
    }
    if revision != anchor.revision_id {
        return None;
    }
    // An unchanged PDF may have different sentence boundaries after reflow.
    let relative = offset.checked_sub(sentence.start_utf16)?;
    let start = sentence.start_utf8 + crate::ocr::byte_at_utf16(&sentence.text, relative)?;
    source?.get(start..)?.starts_with(text)
        .then(|| (position, sentence.id.clone(), revision.to_owned(), None))
}

pub(crate) fn reanchor_canonical_selections(
    db: &Connection, previous_revision: Option<&str>, document: &Document,
    before: &[crate::Sentence], old_pages: Option<&[Option<usize>]>, edits: &[crate::ocr::TranscriptEdit],
) -> Result<()> {
    let Some((previous, current)) = previous_revision.zip(document.current_revision_id.as_deref())
        .filter(|(previous, current)| previous != current)
        else { return Ok(()); };
    let address = (document.id.as_str(), None, previous, current);
    if !edits.is_empty() {
        return reanchor_selections(db, address, before, &document.sentences,
            |_, start, length| crate::ocr::map_unchanged_range_start(start, length, edits));
    }
    let Some((old_pages, new_pages)) = old_pages.zip(document.pdf_page_starts.as_deref())
        .filter(|(old, new)| old.len() == new.len())
        else { return Ok(()); };
    let matching = crate::ocr::matching_pdf_sentence(old_pages, new_pages, &document.sentences);
    reanchor_selections(db, address, before, &document.sentences, |sentence, start, _| {
        let next = matching(sentence)?;
        document.sentences[next].start_utf16.checked_add(start.checked_sub(sentence.start_utf16)?)
    })
}

pub(crate) fn selection_overlaps_layout_edits(
    db: &Connection, id: &str, page: Option<u16>, revision: &str, edits: &[crate::ocr::TranscriptEdit],
) -> Result<bool> {
    let mut query = db.prepare(
        "SELECT anchor FROM dictionary_context_anchors WHERE document_id=?1
         AND json_extract(anchor,'$.page') IS ?2 AND json_extract(anchor,'$.revisionId')=?3
         AND json_type(anchor,'$.selection')='object'",
    ).map_err(error)?;
    let rows = query.query_map(params![id, page, revision], |row| row.get::<_, String>(0)).map_err(error)?;
    for row in rows {
        let anchor: DictionarySentenceAnchor = serde_json::from_str(&row.map_err(error)?).map_err(error)?;
        if let Some(selection) = anchor.selection {
            let start = selection.start_utf16;
            if crate::ocr_layout::intersects_edits(start, start + selection.text.encode_utf16().count(), edits) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub(crate) fn reanchor_selections(
    db: &Connection,
    (id, page, before_revision, after_revision): (&str, Option<u16>, &str, &str),
    before: &[crate::Sentence],
    after: &[crate::Sentence],
    map: impl Fn(&crate::Sentence, usize, usize) -> Option<usize>,
) -> Result<()> {
    let mut query = db.prepare(
        "SELECT card_id, surface, sentence, anchor FROM dictionary_context_anchors
         WHERE document_id=?1 AND json_extract(anchor,'$.page') IS ?2
           AND json_extract(anchor,'$.revisionId')=?3
           AND json_type(anchor,'$.selection')='object'",
    ).map_err(error)?;
    let rows = query.query_map(params![id, page, before_revision], |row| Ok((
        row.get::<_, String>(0)?, row.get::<_, String>(1)?,
        row.get::<_, String>(2)?, row.get::<_, String>(3)?,
    ))).map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?;
    for (card, surface, text, encoded) in rows {
        let old: DictionarySentenceAnchor = serde_json::from_str(&encoded).map_err(error)?;
        let Some((position, _, _, Some(mut selection))) = location_in_revision(
            &old, &text, before_revision, before, None,
        ) else { continue; };
        let length = selection.text.encode_utf16().count();
        let Some(start) = map(&before[position], selection.start_utf16, length) else { continue; };
        let Some(sentence) = after.get(after.partition_point(|sentence| sentence.end_utf16 <= start))
            else { continue; };
        let matched = start.checked_sub(sentence.start_utf16)
            .and_then(|relative| crate::ocr::byte_at_utf16(&sentence.text, relative))
            .and_then(|byte| sentence.text.get(byte..))
            .is_some_and(|tail| tail.starts_with(&selection.text));
        if !matched || !valid_example(&surface, &sentence.text) { continue; }
        selection.start_utf16 = start;
        let updated = DictionarySentenceAnchor {
            sentence_id: sentence.id.clone(),
            revision_id: after_revision.to_owned(),
            page,
            sentence_start_utf16: sentence.start_utf16,
            selection: Some(selection),
        };
        write_context(db, &card, id, &surface, &sentence.text, &[updated])?;
        db.execute(
            "DELETE FROM dictionary_context_anchors
             WHERE card_id=?1 AND document_id=?2 AND surface=?3 AND sentence=?4 AND anchor=?5",
            params![card, id, surface, text, encoded],
        ).map_err(error)?;
    }
    Ok(())
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryImport {
    pub added_cards: usize,
    pub existing_cards: usize,
    pub added_reviews: usize,
    pub restored_practice: bool,
}

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS dictionary_sources (
            document_id TEXT PRIMARY KEY, title TEXT, source_hash TEXT
         );
         CREATE TABLE IF NOT EXISTS dictionary_card_sources (
            card_id TEXT NOT NULL REFERENCES vocabulary(id) ON DELETE CASCADE,
            document_id TEXT NOT NULL REFERENCES dictionary_sources(document_id) ON DELETE CASCADE,
            PRIMARY KEY (card_id, document_id)
         );
         CREATE INDEX IF NOT EXISTS dictionary_document_cards
            ON dictionary_card_sources(document_id, card_id);
         CREATE TABLE IF NOT EXISTS dictionary_card_contexts (
            card_id TEXT NOT NULL,
            document_id TEXT NOT NULL,
            surface TEXT NOT NULL,
            sentence TEXT NOT NULL,
            UNIQUE (card_id, document_id, surface, sentence),
            FOREIGN KEY (card_id, document_id) REFERENCES dictionary_card_sources(card_id, document_id)
                ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS dictionary_document_contexts
            ON dictionary_card_contexts(document_id, card_id);
         CREATE TABLE IF NOT EXISTS dictionary_context_anchors (
            card_id TEXT NOT NULL,
            document_id TEXT NOT NULL,
            surface TEXT NOT NULL,
            sentence TEXT NOT NULL,
            anchor TEXT NOT NULL,
            PRIMARY KEY (card_id, document_id, surface, sentence, anchor),
            FOREIGN KEY (card_id, document_id, surface, sentence)
                REFERENCES dictionary_card_contexts(card_id, document_id, surface, sentence) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS dictionary_review_origins (
            origin TEXT PRIMARY KEY,
            event_id INTEGER NOT NULL UNIQUE REFERENCES review_events(id) ON DELETE CASCADE
         );",
    )
    .map_err(error)
}

pub(crate) fn collection_cards(
    db: &Connection,
    document_id: Option<&str>,
    card_id: Option<&str>,
) -> Result<Vec<Vocabulary>> {
    // A single-card lookup can use the vocabulary primary key before checking membership.
    let card_match = if card_id.is_some() {
        "v.id = ?2"
    } else {
        "?2 IS NULL"
    };
    let mut statement = db
        .prepare(&format!(
            "WITH selected_hash(hash) AS (
                 SELECT source_hash FROM documents WHERE id = ?1
                 UNION
                 SELECT source_hash FROM dictionary_sources WHERE document_id = ?1
             ), source_ids(id) AS (
                 SELECT ?1
                 UNION SELECT id FROM documents WHERE source_hash IN selected_hash
                 UNION SELECT document_id FROM dictionary_sources WHERE source_hash IN selected_hash
             )
             SELECT v.body FROM vocabulary v
             WHERE {card_match}
               AND (?1 IS NULL
                OR json_extract(v.body, '$.documentId') IN source_ids
                OR json_extract(v.body, '$.analysis.documentId') IN source_ids
                OR json_extract(v.body, '$.expression.documentId') IN source_ids
                OR EXISTS (SELECT 1 FROM dictionary_card_sources c
                           WHERE c.card_id = v.id AND c.document_id IN source_ids))
             ORDER BY v.rowid DESC",
        ))
        .map_err(error)?;
    let rows = statement
        .query_map(params![document_id, card_id], |r| r.get::<_, String>(0))
        .map_err(error)?;
    rows.map(|row| serde_json::from_str(&row.map_err(error)?).map_err(error))
        .collect()
}

pub(crate) fn resolve_collection(
    db: &Connection,
    document_id: &str,
    source_hash: Option<&str>,
) -> Result<(String, Option<String>)> {
    find_collection(db, document_id, source_hash)?
        .ok_or_else(|| "Джерело добірки для практики не знайдено.".into())
}

pub(crate) fn find_collection(
    db: &Connection,
    document_id: &str,
    source_hash: Option<&str>,
) -> Result<Option<(String, Option<String>)>> {
    let metadata = source_metadata(db, document_id)?;
    let local_hash = metadata.as_ref().and_then(|(_, hash)| hash.as_deref());
    let Some(hash) = source_hash.or(local_hash) else {
        // Legacy collections can come from card context alone, without source metadata.
        if metadata.is_none() && collection_cards(db, Some(document_id), None)?.is_empty() {
            return Ok(None);
        }
        return Ok(Some((document_id.to_owned(), None)));
    };
    let resolved = db
        .query_row(
            "SELECT id FROM (
               SELECT id, 0 AS priority FROM documents WHERE source_hash=?1
               UNION ALL
               SELECT document_id, CASE WHEN document_id=?2 THEN 1 ELSE 2 END
               FROM dictionary_sources WHERE source_hash=?1
             ) ORDER BY priority, id LIMIT 1",
            params![hash, document_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(resolved.map(|id| (id, Some(hash.to_owned()))))
}

impl Store {
    pub fn dictionary_collection(&self, document_id: Option<&str>) -> Result<Vec<Vocabulary>> {
        let mut cards = collection_cards(&self.connection, document_id, None)?;
        if let Some(id) = document_id {
            apply_collection_contexts(&self.connection, id, &mut cards)?;
        }
        Ok(cards)
    }

    pub(crate) fn saved_selection_translation(
        &self, document_id: &str, language: &str, target: &str, surface: &str, context: &str,
    ) -> Result<Option<String>> {
        use crate::knowledge::word_key;
        if surface.len() > 500 {
            return Ok(None);
        }
        let key = word_key(surface);
        let mut query = self.connection.prepare_cached(
            "SELECT v.body, EXISTS(
                SELECT 1 FROM dictionary_card_contexts c
                JOIN dictionary_sources s ON s.document_id=c.document_id
                WHERE c.card_id=v.id AND c.sentence=?4 AND
                    (c.document_id=?3 OR s.source_hash=(SELECT source_hash FROM documents WHERE id=?3))
             ) AS matches_context
             FROM vocabulary v
             WHERE json_extract(v.body,'$.language')=?1 AND json_extract(v.body,'$.targetLanguage')=?2",
        ).map_err(error)?;
        let mut rows = query.query(params![language, target, document_id, context]).map_err(error)?;
        let mut contextual = std::collections::BTreeMap::new();
        let mut general = std::collections::BTreeMap::new();
        while let Some(row) = rows.next().map_err(error)? {
            let body: String = row.get(0).map_err(error)?;
            let card: Vocabulary = serde_json::from_str(&body).map_err(error)?;
            let forms = [
                Some(card.word.as_str()),
                card.analysis.as_ref().map(|analysis| analysis.surface.as_str()),
                card.expression.as_ref().map(|expression| expression.surface.as_str()),
                card.sense.as_ref().map(|sense| sense.surface.as_str()),
            ];
            if !forms.into_iter().flatten().any(|form| word_key(form) == key) {
                continue;
            }
            let same_context: bool = row.get(1).map_err(error)?;
            let candidates = if same_context { &mut contextual } else { &mut general };
            candidates.entry(word_key(&card.translation)).or_insert(card.translation);
        }
        let candidates = if contextual.is_empty() { general } else { contextual };
        Ok(if candidates.len() == 1 { candidates.into_values().next() } else { None })
    }

    pub fn dictionary_sources(&self) -> Result<Vec<DictionarySource>> {
        let tx = self.connection.unchecked_transaction().map_err(error)?;
        let entries = self.dictionary_collection(None)?;
        let sources = sources_for(&tx, &entries)?;
        tx.commit().map_err(error)?;
        Ok(sources)
    }

    pub fn export_dictionary(&self, document_id: Option<&str>) -> Result<DictionaryArchive> {
        let tx = self.connection.unchecked_transaction().map_err(error)?;
        let entries = collection_cards(&tx, document_id, None)?;
        let (sources, reviews) = export_details(&tx, &entries)?;
        let practice = if document_id.is_none() {
            practice::read(&tx)?.filter(|progress| progress.question.is_some())
        } else {
            None
        };
        tx.commit().map_err(error)?;
        Ok(DictionaryArchive {
            version: VERSION,
            entries,
            reviews,
            sources,
            practice,
        })
    }

    pub fn import_dictionary(&mut self, archive: DictionaryArchive) -> Result<DictionaryImport> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let (stats, _) = restore_archive(&tx, archive)?;
        tx.commit().map_err(error)?;
        Ok(stats)
    }
}

pub(crate) fn observed_card_forms(db: &Connection, card_id: &str) -> Result<Vec<String>> {
    let mut query = db.prepare(
        "SELECT DISTINCT surface FROM dictionary_card_contexts WHERE card_id=?1 ORDER BY surface",
    ).map_err(error)?;
    let rows = query.query_map([card_id], |row| row.get(0)).map_err(error)?;
    rows.collect::<rusqlite::Result<_>>().map_err(error)
}

pub(crate) fn apply_collection_contexts(db: &Connection, id: &str, cards: &mut [Vocabulary]) -> Result<()> {
    if cards.is_empty() {
        return Ok(());
    }
    let Some((document_id, hash)) = find_collection(db, id, None)? else {
        return Ok(());
    };
    let card_id = match cards {
        [card] => Some(card.id.as_str()),
        _ => None,
    };
    let mut query = db.prepare(
        "SELECT card_id, sentence FROM dictionary_card_contexts
         WHERE (?3 IS NULL OR card_id=?3) AND (
            document_id=?1 OR document_id IN (
                SELECT document_id FROM dictionary_sources WHERE source_hash=?2
            )
         ) ORDER BY rowid",
    ).map_err(error)?;
    let rows = query.query_map(params![document_id, hash, card_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }).map_err(error)?;
    let contexts = rows.collect::<rusqlite::Result<BTreeMap<_, _>>>().map_err(error)?;
    for card in cards {
        if let Some(sentence) = contexts.get(&card.id) {
            card.context.clone_from(sentence);
            card.document_id.clone_from(&document_id);
        }
    }
    Ok(())
}

pub(crate) fn export_details(
    db: &Connection,
    entries: &[Vocabulary],
) -> Result<(Vec<DictionarySource>, Vec<DictionaryReview>)> {
    let ids: BTreeSet<_> = entries.iter().map(|card| card.id.as_str()).collect();
    let reviews = review_rows(db)?
        .into_iter()
        .map(|(_, review)| review)
        .filter(|review| ids.contains(review.event.entry_id.as_str()))
        .collect();

    let mut sources = sources_for(db, entries)?;
    let mut query = db.prepare(
        "SELECT c.document_id, c.card_id, c.surface, c.sentence,
                (SELECT json_group_array(json(a.anchor)) FROM dictionary_context_anchors a
                 WHERE (a.card_id,a.document_id,a.surface,a.sentence) =
                       (c.card_id,c.document_id,c.surface,c.sentence))
         FROM dictionary_card_contexts c ORDER BY c.rowid",
    ).map_err(error)?;
    let rows = query.query_map([], |row| Ok((
        row.get::<_, String>(0)?,
        DictionaryExample { entry_id: row.get(1)?, surface: row.get(2)?, sentence: row.get(3)?, anchors: Vec::new() },
        row.get::<_, String>(4)?,
    ))).map_err(error)?;
    let mut examples = BTreeMap::<String, Vec<DictionaryExample>>::new();
    for row in rows {
        let (document_id, mut example, anchors) = row.map_err(error)?;
        if ids.contains(example.entry_id.as_str()) {
            example.anchors = serde_json::from_str(&anchors).map_err(error)?;
            example.anchors.sort();
            examples.entry(document_id).or_default().push(example);
        }
    }
    for source in &mut sources {
        source.examples = examples.remove(&source.document_id).unwrap_or_default();
    }
    Ok((sources, reviews))
}

fn sources_for(db: &Connection, entries: &[Vocabulary]) -> Result<Vec<DictionarySource>> {
    let ids: BTreeSet<_> = entries.iter().map(|card| card.id.as_str()).collect();
    let mut memberships = BTreeMap::<String, BTreeSet<String>>::new();
    for card in entries {
        for document_id in card_documents(card) {
            memberships
                .entry(document_id.into())
                .or_default()
                .insert(card.id.clone());
        }
    }
    {
        let mut statement = db
            .prepare("SELECT card_id, document_id FROM dictionary_card_sources")
            .map_err(error)?;
        let rows = statement
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(error)?;
        for row in rows {
            let (card, document) = row.map_err(error)?;
            if ids.contains(card.as_str()) {
                memberships.entry(document).or_default().insert(card);
            }
        }
    }
    memberships
        .into_iter()
        .map(|(document_id, cards)| {
            let (title, source_hash) = source_metadata(db, &document_id)?.unwrap_or_default();
            Ok(DictionarySource {
                document_id,
                title,
                source_hash,
                entry_ids: cards.into_iter().collect(),
                examples: Vec::new(),
            })
        })
        .collect()
}

fn restore_archive(
    tx: &Connection,
    archive: DictionaryArchive,
) -> Result<(DictionaryImport, BTreeMap<String, String>)> {
    validate_archive(&archive)?;
    let mut stats = DictionaryImport::default();

    let mut sources = BTreeMap::new();
    for source in &archive.sources {
        sources.insert(source.document_id.as_str(), resolve_source(tx, source)?);
    }
    let mut card_ids = BTreeMap::new();
    let mut imported = BTreeMap::<String, Vec<Vocabulary>>::new();
    for mut card in archive.entries {
        let source_id = card.id.clone();
        remap_card_sources(&mut card, &sources);
        card.accepted_translations =
            crate::card_learning::prepare_variants(&card.translation, card.accepted_translations)?;
        let key = card.word_key();
        if let Some(mut local) = matching_card(tx, &card, &key)? {
            if local.id != card.id || local.translation == card.translation {
                local.merge_accepted_translations(&card)?;
            }
            card_ids.insert(source_id, local.id.clone());
            write_card(tx, &local)?;
            imported.entry(local.id).or_default().push(card);
            stats.existing_cards += 1;
        } else {
            if tx
                .query_row("SELECT 1 FROM vocabulary WHERE id=?1", [&card.id], |_| {
                    Ok(())
                })
                .optional()
                .map_err(error)?
                .is_some()
            {
                card.id = Uuid::new_v4().to_string();
            }
            tx.execute(
                "INSERT INTO vocabulary (id,word_key,body) VALUES (?1,?2,?3)",
                params![card.id, key, serde_json::to_string(&card).map_err(error)?],
            )
            .map_err(error)?;
            card_ids.insert(source_id, card.id.clone());
            stats.added_cards += 1;
        }
    }

    for source in &archive.sources {
        for entry in &source.entry_ids {
            tx.execute(
                "INSERT OR IGNORE INTO dictionary_card_sources(card_id,document_id) VALUES (?1,?2)",
                params![card_ids[entry], sources[source.document_id.as_str()]],
            )
            .map_err(error)?;
        }
        for example in &source.examples {
            write_context(tx, &card_ids[&example.entry_id], &sources[source.document_id.as_str()],
                &example.surface, &example.sentence, &example.anchors)?;
        }
    }

    let mut existing_reviews = BTreeMap::new();
    let mut local_history = BTreeMap::<String, History>::new();
    let affected: BTreeSet<_> = card_ids.values().collect();
    for (row_id, review) in review_rows(tx)? {
        if affected.contains(&review.event.entry_id) {
            local_history
                .entry(review.event.entry_id.clone())
                .or_default()
                .record(&review.event);
        }
        existing_reviews.insert(review.id, (row_id, review.event));
    }
    let mut incoming_history = BTreeMap::<String, History>::new();
    let mut added_history = BTreeMap::<String, History>::new();
    for mut review in archive.reviews {
        let source_card = review.event.entry_id.clone();
        incoming_history
            .entry(source_card.clone())
            .or_default()
            .record(&review.event);
        review.event.entry_id = card_ids[&source_card].clone();
        if let Some((row_id, existing)) = existing_reviews.get(&review.id) {
            merge_review_details(tx, *row_id, existing, &review.event)?;
            continue;
        }
        tx.execute(
            "INSERT INTO review_events(entry_id,direction,grade,reviewed_at,answer,elapsed_ms)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                review.event.entry_id,
                review.event.direction,
                review.event.grade,
                review.event.reviewed_at,
                review.event.answer,
                review.event.elapsed_ms
            ],
        )
        .map_err(error)?;
        tx.execute(
            "INSERT INTO dictionary_review_origins(origin,event_id) VALUES (?1,?2)",
            params![review.id, tx.last_insert_rowid()],
        )
        .map_err(error)?;
        added_history
            .entry(review.event.entry_id.clone())
            .or_default()
            .record(&review.event);
        stats.added_reviews += 1;
    }

    for (local_id, incoming_cards) in imported {
        let body: String = tx
            .query_row(
                "SELECT body FROM vocabulary WHERE id=?1",
                [&local_id],
                |r| r.get(0),
            )
            .map_err(error)?;
        let mut local: Vocabulary = serde_json::from_str(&body).map_err(error)?;
        let mut before = local_history.get(&local.id).copied().unwrap_or_default();
        let added = added_history.get(&local.id).copied().unwrap_or_default();
        for incoming in incoming_cards {
            let incoming_history = incoming_history
                .get(&incoming.id)
                .copied()
                .unwrap_or_default();
            merge_state(
                &mut local.forward,
                incoming.forward,
                &mut before.forward,
                incoming_history.forward,
                added.forward,
            );
            merge_state(
                &mut local.reverse,
                incoming.reverse,
                &mut before.reverse,
                incoming_history.reverse,
                added.reverse,
            );
        }
        discard_stale_memory(&mut local.forward, before.forward, added.forward);
        discard_stale_memory(&mut local.reverse, before.reverse, added.reverse);
        write_card(tx, &local)?;
    }
    if let Some(progress) = archive.practice {
        stats.restored_practice = practice::restore_if_fresh(tx, progress, &card_ids)?;
    }
    Ok((stats, card_ids))
}

pub(crate) fn restore_backup_dictionary(
    tx: &Connection,
    entries: Vec<Vocabulary>,
    review_events: Vec<ReviewEvent>,
    review_origins: Vec<String>,
    sources: Vec<DictionarySource>,
    documents: &[Document],
) -> Result<BTreeMap<String, String>> {
    if !review_origins.is_empty() && review_origins.len() != review_events.len() {
        return Err("Ідентифікатори історії не збігаються з архівом".into());
    }
    let ids: BTreeSet<_> = entries.iter().map(|card| card.id.as_str()).collect();
    let mut signature_counts = BTreeMap::new();
    let mut reviews = Vec::new();
    for (index, event) in review_events.into_iter().enumerate() {
        if !matches!(event.direction.as_str(), "forward" | "reverse") || event.grade > 3 {
            return Err("Некоректна історія повторень".into());
        }
        if !ids.contains(event.entry_id.as_str()) {
            // Older full backups may have serialized reviews for a deleted card.
            continue;
        }
        let id = if let Some(id) = review_origins.get(index) {
            id.clone()
        } else {
            let ordinal = signature_counts
                .entry((
                    event.entry_id.clone(),
                    event.direction.clone(),
                    event.grade,
                    event.reviewed_at,
                ))
                .or_insert(0);
            *ordinal += 1;
            review_origin(&event, *ordinal)
        };
        reviews.push(DictionaryReview { id, event });
    }

    let mut by_source = BTreeMap::new();
    for source in sources {
        if by_source
            .insert(source.document_id.clone(), source)
            .is_some()
        {
            return Err("Повторюване джерело в архіві словника".into());
        }
    }
    let documents: BTreeMap<_, _> = documents.iter().map(|doc| (doc.id.as_str(), doc)).collect();
    for card in &entries {
        for id in card_documents(card) {
            let source = by_source.entry(id.to_owned()).or_insert_with(|| {
                let doc = documents.get(id);
                DictionarySource {
                    document_id: id.to_owned(),
                    title: doc.map(|doc| doc.title.clone()),
                    source_hash: doc.map(|doc| doc.source_hash.clone()),
                    entry_ids: Vec::new(),
                    examples: Vec::new(),
                }
            });
            source.entry_ids.push(card.id.clone());
        }
    }
    let sources = by_source
        .into_values()
        .map(|mut source| {
            source.entry_ids.sort_unstable();
            source.entry_ids.dedup();
            source
        })
        .collect();
    let (_, ids) = restore_archive(
        tx,
        DictionaryArchive {
            version: VERSION,
            entries,
            reviews,
            sources,
            practice: None,
        },
    )?;
    Ok(ids)
}

fn write_card(db: &Connection, card: &Vocabulary) -> Result<()> {
    db.execute(
        "UPDATE vocabulary SET word_key=?1, body=?2 WHERE id=?3",
        params![
            card.word_key(),
            serde_json::to_string(card).map_err(error)?,
            card.id
        ],
    )
    .map_err(error)?;
    Ok(())
}

fn matching_card(db: &Connection, incoming: &Vocabulary, key: &str) -> Result<Option<Vocabulary>> {
    let by_key = |key: &str| -> Result<Option<Vocabulary>> {
        let body: Option<String> = db
            .query_row(
                "SELECT body FROM vocabulary WHERE word_key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        body.map(|body| serde_json::from_str(&body).map_err(error))
            .transpose()
    };
    let at_id: Option<String> = db
        .query_row(
            "SELECT body FROM vocabulary WHERE id=?1",
            [&incoming.id],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    let at_id: Option<Vocabulary> = at_id
        .map(|body| serde_json::from_str(&body).map_err(error))
        .transpose()?;
    if let Some(card) = at_id
        .as_ref()
        .filter(|card| same_card_identity(card, incoming))
    {
        return Ok(Some(card.clone()));
    }
    if let Some(card) = by_key(key)? {
        return Ok(Some(card));
    }
    if incoming.sense.is_some() {
        return Ok(None);
    }
    let plain = |card: &Vocabulary| card.analysis.is_none() && card.expression.is_none();
    let surface = |card: &Vocabulary| {
        card.analysis
            .as_ref()
            .map(|a| a.surface.clone())
            .or_else(|| card.expression.as_ref().map(|e| e.surface.clone()))
            .unwrap_or_else(|| card.word.clone())
    };
    if let Some(card) = at_id {
        let same_surface =
            card.surface_key(&surface(&card)) == incoming.surface_key(&surface(incoming));
        let same_word = card.surface_key(&card.word) == incoming.surface_key(&incoming.word);
        if card.sense.is_none() && plain(&card) != plain(incoming) && (same_surface || same_word) {
            return Ok(Some(promote(card, incoming)));
        }
    }
    Ok(incoming.legacy_card_for_promotion(db)?.map(|card| promote(card, incoming)))
}

fn same_card_identity(local: &Vocabulary, incoming: &Vocabulary) -> bool {
    if local.language != incoming.language
        || local.target_language != incoming.target_language
        || local.word != incoming.word
    {
        return false;
    }
    match (&local.sense, &incoming.sense) {
        (Some(local), Some(incoming)) => return local.id == incoming.id,
        (None, None) => {}
        _ => return false,
    }
    match (&local.expression, &incoming.expression) {
        (Some(local), Some(incoming)) => return local.kind == incoming.kind,
        (None, None) => {}
        _ => return false,
    }
    match (&local.analysis, &incoming.analysis) {
        (Some(local), Some(incoming)) => local.part_of_speech == incoming.part_of_speech,
        (None, None) => true,
        _ => false,
    }
}

fn promote(mut local: Vocabulary, incoming: &Vocabulary) -> Vocabulary {
    if local.analysis.is_none()
        && local.expression.is_none()
        && (incoming.analysis.is_some() || incoming.expression.is_some())
    {
        local.word.clone_from(&incoming.word);
        local.analysis.clone_from(&incoming.analysis);
        local.expression.clone_from(&incoming.expression);
        if local.document_id != incoming.document_id || local.context != incoming.context {
            if let Some(analysis) = &mut local.analysis {
                analysis
                    .context
                    .get_or_insert_with(|| incoming.context.clone());
                analysis
                    .document_id
                    .get_or_insert_with(|| incoming.document_id.clone());
            }
            if let Some(expression) = &mut local.expression {
                expression
                    .context
                    .get_or_insert_with(|| incoming.context.clone());
                expression
                    .document_id
                    .get_or_insert_with(|| incoming.document_id.clone());
            }
        }
    }
    local
}

pub(crate) fn retain_removed_document_source(db: &Connection, id: &str) -> Result<()> {
    db.execute(
        "INSERT INTO dictionary_sources(document_id,title,source_hash)
         SELECT id, json_extract(body, '$.title'), source_hash FROM documents
         WHERE id=?1 AND (
             EXISTS (SELECT 1 FROM dictionary_card_sources WHERE document_id=?1)
             OR EXISTS (SELECT 1 FROM vocabulary WHERE ?1 IN (
                  json_extract(body, '$.documentId'),
                  json_extract(body, '$.analysis.documentId'),
                  json_extract(body, '$.expression.documentId')
             ))
         )
         ON CONFLICT(document_id) DO UPDATE SET
             title=excluded.title, source_hash=excluded.source_hash",
        [id],
    )
    .map_err(error)?;
    Ok(())
}

pub(crate) fn remember_card_context(
    db: &Connection, card_id: &str, document_id: &str, surface: &str, sentence: &str,
    anchor: Option<&DictionarySentenceAnchor>,
) -> Result<()> {
    let Some((title, source_hash)) = source_metadata(db, document_id)? else {
        return Ok(());
    };
    let document_id = resolve_source(
        db,
        &DictionarySource {
            document_id: document_id.to_owned(),
            title,
            source_hash,
            entry_ids: Vec::new(),
            examples: Vec::new(),
        },
    )?;
    db.execute(
        "INSERT OR IGNORE INTO dictionary_card_sources(card_id,document_id) VALUES (?1,?2)",
        params![card_id, document_id],
    ).map_err(error)?;
    if valid_example(surface, sentence) {
        let anchors = anchor.map_or_else(|| &[][..], std::slice::from_ref);
        write_context(db, card_id, &document_id, surface, sentence, anchors)?;
    }
    Ok(())
}

fn valid_example(surface: &str, sentence: &str) -> bool {
    !surface.trim().is_empty() && surface.len() <= 500
        && !sentence.trim().is_empty() && sentence.len() <= 64 * 1024
        && !surface.contains('\0') && !sentence.contains('\0')
}

fn write_context(
    db: &Connection, card_id: &str, document_id: &str, surface: &str, sentence: &str,
    anchors: &[DictionarySentenceAnchor],
) -> Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO dictionary_card_contexts(card_id,document_id,surface,sentence)
         VALUES (?1,?2,?3,?4)",
        params![card_id, document_id, surface, sentence],
    ).map_err(error)?;
    for anchor in anchors {
        let body = serde_json::to_string(anchor).map_err(error)?;
        db.execute(
            "INSERT OR IGNORE INTO dictionary_context_anchors(card_id,document_id,surface,sentence,anchor)
             VALUES (?1,?2,?3,?4,?5)",
            params![card_id, document_id, surface, sentence, body],
        ).map_err(error)?;
    }
    Ok(())
}

fn source_metadata(db: &Connection, id: &str) -> Result<Option<(Option<String>, Option<String>)>> {
    db.query_row(
        "SELECT json_extract(body, '$.title'), source_hash FROM documents WHERE id=?1
         UNION ALL SELECT title, source_hash FROM dictionary_sources WHERE document_id=?1 LIMIT 1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(error)
}

fn resolve_source(db: &Connection, source: &DictionarySource) -> Result<String> {
    let mut id = source.document_id.clone();
    if let Some(hash) = &source.source_hash {
        if let Some(local) = db
            .query_row(
                "SELECT id FROM documents WHERE source_hash=?1",
                [hash],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?
        {
            id = local;
        } else if source_metadata(db, &id)?
            .and_then(|(_, hash)| hash)
            .is_some_and(|local| local != *hash)
        {
            id = format!("dictionary:{hash}");
        }
    }
    db.execute(
        "INSERT INTO dictionary_sources(document_id,title,source_hash) VALUES (?1,?2,?3)
         ON CONFLICT(document_id) DO UPDATE SET
             title=COALESCE(dictionary_sources.title,excluded.title),
             source_hash=COALESCE(dictionary_sources.source_hash,excluded.source_hash)",
        params![id, source.title, source.source_hash],
    )
    .map_err(error)?;
    Ok(id)
}

fn card_documents(card: &Vocabulary) -> impl Iterator<Item = &str> {
    [
        Some(card.document_id.as_str()),
        card.analysis
            .as_ref()
            .and_then(|value| value.document_id.as_deref()),
        card.expression
            .as_ref()
            .and_then(|value| value.document_id.as_deref()),
    ]
    .into_iter()
    .flatten()
    .filter(|id| !id.is_empty())
}

fn remap_card_sources(card: &mut Vocabulary, sources: &BTreeMap<&str, String>) {
    let replace = |id: &mut String| {
        if let Some(local) = sources.get(id.as_str()) {
            id.clone_from(local);
        }
    };
    replace(&mut card.document_id);
    if let Some(id) = card
        .analysis
        .as_mut()
        .and_then(|value| value.document_id.as_mut())
    {
        replace(id);
    }
    if let Some(id) = card
        .expression
        .as_mut()
        .and_then(|value| value.document_id.as_mut())
    {
        replace(id);
    }
}

fn review_rows(db: &Connection) -> Result<Vec<(i64, DictionaryReview)>> {
    let mut statement = db
        .prepare(
            "SELECT e.entry_id,e.direction,e.grade,e.reviewed_at,o.origin,
                 ROW_NUMBER() OVER (
                     PARTITION BY e.entry_id,e.direction,e.grade,e.reviewed_at ORDER BY e.id
                 ),
                 e.id,e.answer,e.elapsed_ms
         FROM review_events e JOIN vocabulary v ON v.id=e.entry_id
         LEFT JOIN dictionary_review_origins o ON o.event_id=e.id ORDER BY e.id",
        )
        .map_err(error)?;
    let rows = statement
        .query_map([], |row| {
            let event = ReviewEvent {
                entry_id: row.get(0)?,
                direction: row.get(1)?,
                grade: row.get(2)?,
                reviewed_at: row.get(3)?,
                answer: row.get(7)?,
                elapsed_ms: row.get(8)?,
            };
            let origin: Option<String> = row.get(4)?;
            let ordinal: i64 = row.get(5)?;
            Ok((
                row.get(6)?,
                DictionaryReview {
                    id: origin.unwrap_or_else(|| review_origin(&event, ordinal)),
                    event,
                },
            ))
        })
        .map_err(error)?;
    rows.map(|row| row.map_err(error)).collect()
}

fn review_origin(event: &ReviewEvent, ordinal: i64) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!(
            "dictionary-review-v1:{}:{}:{}:{}:{ordinal}",
            event.entry_id, event.direction, event.grade, event.reviewed_at
        ))
    )
}

fn validate_archive(archive: &DictionaryArchive) -> Result<()> {
    if archive.version != VERSION {
        return Err("Невідома версія архіву словника".into());
    }
    let mut cards = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for entry in &archive.entries {
        valid_language(&entry.language)?;
        valid_language(&entry.target_language)?;
        if !identifier(&entry.id)
            || !cards.insert(entry.id.as_str())
            || !keys.insert(entry.word_key())
            || entry.word.trim().is_empty()
            || entry.word.len() > 500
            || entry.translation.trim().is_empty()
            || entry.translation.len() > 2000
            || [&entry.forward, &entry.reverse].iter().any(|state| {
                state.interval_days > MAX_INTERVAL_DAYS
                    || state.reviews == u32::MAX
                    || state
                        .fsrs
                        .is_some_and(|memory| !memory.is_valid(state.reviews))
            })
            || card_documents(entry).any(|id| !identifier(id))
        {
            return Err("Некоректна картка в архіві словника".into());
        }
        entry.validate_annotations()?;
    }
    if let Some(progress) = &archive.practice {
        let question = progress
            .question
            .as_ref()
            .ok_or("В архіві немає запитання практики.")?;
        if !cards.contains(question.card_id.as_str()) {
            return Err("В архіві немає картки для практики.".into());
        }
    }
    let mut sources = BTreeSet::new();
    for source in &archive.sources {
        if !identifier(&source.document_id)
            || !sources.insert(source.document_id.as_str())
            || source
                .source_hash
                .as_ref()
                .is_some_and(|hash| !digest(hash))
            || source
                .entry_ids
                .iter()
                .any(|id| !cards.contains(id.as_str()))
            || source.examples.iter().any(|example|
                !source.entry_ids.contains(&example.entry_id)
                    || !valid_example(&example.surface, &example.sentence)
                    || example.anchors.iter().any(|anchor|
                        !identifier(&anchor.sentence_id) || !identifier(&anchor.revision_id)
                            || anchor.page == Some(u16::MAX) || anchor.sentence_start_utf16 > crate::MAX_IMPORT
                            || anchor.selection.as_ref().is_some_and(|selection|
                                !selection.is_valid(anchor.sentence_start_utf16))))
        {
            return Err("Некоректне джерело карток в архіві словника".into());
        }
    }
    let mut reviews = BTreeSet::new();
    for review in &archive.reviews {
        let event = &review.event;
        if !digest(&review.id)
            || !reviews.insert(review.id.as_str())
            || !cards.contains(event.entry_id.as_str())
            || !matches!(event.direction.as_str(), "forward" | "reverse")
            || event.grade > 3
            || event.reviewed_at < 0
            || event
                .answer
                .as_deref()
                .is_some_and(|answer| !crate::review_schedule::valid_answer(answer))
            || event
                .elapsed_ms
                .is_some_and(|elapsed| elapsed > crate::review_schedule::MAX_ELAPSED_MS)
        {
            return Err("Некоректна історія повторень в архіві словника".into());
        }
    }
    Ok(())
}

fn merge_review_details(
    tx: &Connection,
    row_id: i64,
    local: &ReviewEvent,
    incoming: &ReviewEvent,
) -> Result<()> {
    if local.entry_id != incoming.entry_id
        || local.direction != incoming.direction
        || local.grade != incoming.grade
        || local.reviewed_at != incoming.reviewed_at
        || matches!((&local.answer, &incoming.answer), (Some(a), Some(b)) if a != b)
        || matches!((local.elapsed_ms, incoming.elapsed_ms), (Some(a), Some(b)) if a != b)
    {
        return Err("Конфлікт ідентифікаторів історії повторень".into());
    }
    // Older archives omit these values but carry the same review identity.
    if (local.answer.is_none() && incoming.answer.is_some())
        || (local.elapsed_ms.is_none() && incoming.elapsed_ms.is_some())
    {
        tx.execute(
            "UPDATE review_events SET answer=COALESCE(answer,?1), elapsed_ms=COALESCE(elapsed_ms,?2)
             WHERE id=?3",
            params![incoming.answer, incoming.elapsed_ms, row_id],
        )
        .map_err(error)?;
    }
    Ok(())
}

fn identifier(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 160 && !value.chars().any(char::is_control)
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Copy, Default)]
struct Direction {
    latest: Option<i64>,
    count: u32,
}

#[derive(Clone, Copy, Default)]
struct History {
    forward: Direction,
    reverse: Direction,
}

impl History {
    fn record(&mut self, event: &ReviewEvent) {
        let state = if event.direction == "forward" {
            &mut self.forward
        } else {
            &mut self.reverse
        };
        state.latest = state.latest.max(Some(event.reviewed_at));
        state.count = state.count.saturating_add(1);
    }
}

fn merge_state(
    local: &mut ReviewState,
    incoming: ReviewState,
    before: &mut Direction,
    incoming_history: Direction,
    added: Direction,
) {
    let count = local
        .reviews
        .max(incoming.reviews)
        .max(before.count.saturating_add(added.count));
    let use_incoming = if before.latest != incoming_history.latest {
        incoming_history.latest > before.latest
    } else {
        incoming.reviews > local.reviews
    };
    if use_incoming {
        *local = incoming;
        before.latest = incoming_history.latest;
    }
    local.reviews = count;
}

fn discard_stale_memory(state: &mut ReviewState, before: Direction, added: Direction) {
    let merged_count = before.count.saturating_add(added.count);
    if state
        .fsrs
        .is_some_and(|memory| memory.history_reviews != merged_count)
    {
        state.fsrs = None;
    }
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
