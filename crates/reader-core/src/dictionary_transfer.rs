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
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryImport {
    pub added_cards: usize,
    pub existing_cards: usize,
    pub added_reviews: usize,
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
    let metadata = source_metadata(db, document_id)?;
    let local_hash = metadata.as_ref().and_then(|(_, hash)| hash.as_deref());
    let Some(hash) = source_hash.or(local_hash) else {
        // Legacy collections can come from card context alone, without source metadata.
        if metadata.is_none() && collection_cards(db, Some(document_id), None)?.is_empty() {
            return Err("Джерело добірки для практики не знайдено.".into());
        }
        return Ok((document_id.to_owned(), None));
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
        .map_err(error)?
        .ok_or("Джерело добірки для практики не знайдено.")?;
    Ok((resolved, Some(hash.to_owned())))
}

impl Store {
    pub fn dictionary_collection(&self, document_id: Option<&str>) -> Result<Vec<Vocabulary>> {
        collection_cards(&self.connection, document_id, None)
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
        let entries = self.dictionary_collection(document_id)?;
        let (sources, reviews) = export_details(&tx, &entries)?;
        tx.commit().map_err(error)?;
        Ok(DictionaryArchive {
            version: VERSION,
            entries,
            reviews,
            sources,
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

    Ok((sources_for(db, entries)?, reviews))
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
            local.merge_accepted_translations(&card)?;
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
    let at_id: Option<String> = db
        .query_row(
            "SELECT body FROM vocabulary WHERE id=?1",
            [&incoming.id],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    if let Some(body) = at_id {
        let card: Vocabulary = serde_json::from_str(&body).map_err(error)?;
        let same_surface =
            card.surface_key(&surface(&card)) == incoming.surface_key(&surface(incoming));
        let same_word = card.surface_key(&card.word) == incoming.surface_key(&incoming.word);
        if card.sense.is_none() && plain(&card) != plain(incoming) && (same_surface || same_word) {
            return Ok(Some(promote(card, incoming)));
        }
    }
    if !plain(incoming) {
        let legacy_key = incoming.surface_key(&incoming.word);
        if let Some(card) = by_key(&legacy_key)? {
            return Ok(Some(promote(card, incoming)));
        }
    }
    Ok(None)
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

pub(crate) fn link_card_document(db: &Connection, card_id: &str, document_id: &str) -> Result<()> {
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
        },
    )?;
    db.execute(
        "INSERT OR IGNORE INTO dictionary_card_sources(card_id,document_id) VALUES (?1,?2)",
        params![card_id, document_id],
    )
    .map_err(error)?;
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
