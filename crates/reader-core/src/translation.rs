//! Routes translations through models already installed on the device.

use crate::{pdf_detached_pages::word_break, valid_language, Document, Result, Store};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{cmp::Ordering, collections::{BTreeMap, HashMap, HashSet}, ops::{Range, RangeInclusive}};
use unicode_normalization::UnicodeNormalization;

const MAX_BATCH_BYTES: usize = 64 * 1024;
const CACHE_ROWS: i64 = 128;
const LIVE_CACHE_ROWS: i64 = 100_000;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranslationCacheEntry {
    key: String,
    source_hash: String,
    segments: Vec<String>,
}

impl TranslationCacheEntry {
    fn valid(&self) -> bool {
        let digest = |text: &str| {
            text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        digest(&self.key) && digest(&self.source_hash) && valid_segments(&self.segments)
    }
}

pub type TranslationCacheLinks = BTreeMap<String, Vec<String>>;

pub(crate) fn initialize_cache_ownership(db: &mut Connection) -> Result<()> {
    fn owner_key(db: &Connection) -> Result<Option<i64>> {
        db.query_row(
            "SELECT pk FROM pragma_table_info('translation_cache_owners') WHERE name='document_id'",
            [], |row| row.get(0),
        ).optional().map_err(|error| error.to_string())
    }

    if owner_key(db)? == Some(2) {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let migrating = owner_key(&tx)? == Some(0);
    if migrating {
        tx.execute_batch("ALTER TABLE translation_cache_owners RENAME TO translation_cache_owners_legacy")
            .map_err(|error| error.to_string())?;
    }
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS translation_cache_owners (
            cache_key TEXT NOT NULL REFERENCES translation_cache(key) ON DELETE CASCADE,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            PRIMARY KEY(cache_key, document_id)
        );
        CREATE INDEX IF NOT EXISTS translation_cache_owners_document
            ON translation_cache_owners(document_id);",
    ).map_err(|error| error.to_string())?;
    if migrating {
        tx.execute_batch(
            "INSERT INTO translation_cache_owners(cache_key, document_id)
             SELECT cache_key, document_id FROM translation_cache_owners_legacy;
             DROP TABLE translation_cache_owners_legacy;",
        ).map_err(|error| error.to_string())?;
    }
    tx.commit().map_err(|error| error.to_string())
}

pub(crate) fn export_cache(db: &Connection) -> Result<(Vec<TranslationCacheEntry>, TranslationCacheLinks)> {
    let mut query = db.prepare(
        "SELECT cache.key, cache.body FROM translation_cache cache
         WHERE EXISTS(SELECT 1 FROM translation_cache_owners WHERE cache_key=cache.key)
         ORDER BY cache.rowid DESC LIMIT ?1",
    ).map_err(|error| error.to_string())?;
    let rows = query.query_map([CACHE_ROWS], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }).map_err(|error| error.to_string())?;
    let mut owners = db.prepare(
        "SELECT docs.source_hash FROM translation_cache_owners owner
         JOIN documents docs ON docs.id=owner.document_id
         WHERE owner.cache_key=?1 ORDER BY docs.source_hash",
    ).map_err(|error| error.to_string())?;
    let mut entries = Vec::new();
    let mut links = TranslationCacheLinks::new();
    for row in rows {
        let (key, body) = row.map_err(|error| error.to_string())?;
        let Ok(segments) = serde_json::from_str(&body) else { continue };
        let mut hashes = owners.query_map([&key], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        let Some(source_hash) = hashes.next().transpose().map_err(|error| error.to_string())? else { continue };
        let entry = TranslationCacheEntry { key, source_hash, segments };
        if entry.valid() {
            let additional = hashes.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|error| error.to_string())?;
            if !additional.is_empty() {
                links.insert(entry.key.clone(), additional);
            }
            entries.push(entry);
        }
    }
    Ok((entries, links))
}

pub(crate) fn validate_cache(
    entries: &[TranslationCacheEntry], links: &TranslationCacheLinks, documents: &[Document],
) -> Result<()> {
    let invalid = || "Некоректний кеш перекладу в резервній копії.".to_owned();
    if entries.len() > CACHE_ROWS as usize || links.len() > entries.len() {
        return Err(invalid());
    }
    let source_hashes: HashSet<_> = documents.iter().map(|doc| doc.source_hash.as_str()).collect();
    let mut seen = HashSet::with_capacity(entries.len());
    for entry in entries {
        if !entry.valid() || !seen.insert(entry.key.as_str()) || !source_hashes.contains(entry.source_hash.as_str()) {
            return Err(invalid());
        }
        if let Some(additional) = links.get(&entry.key) {
            if additional.len() >= source_hashes.len() {
                return Err(invalid());
            }
            let mut owners = HashSet::from([entry.source_hash.as_str()]);
            if additional.iter().any(|hash| !source_hashes.contains(hash.as_str()) || !owners.insert(hash.as_str())) {
                return Err(invalid());
            }
        }
    }
    if links.keys().any(|key| !seen.contains(key.as_str())) {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn restore_cache(
    db: &Connection, entries: Vec<TranslationCacheEntry>, links: &TranslationCacheLinks,
) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut query = db.prepare("SELECT key FROM translation_cache").map_err(|error| error.to_string())?;
    let existing = query.query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<rusqlite::Result<HashSet<_>>>()
        .map_err(|error| error.to_string())?;
    let available = (LIVE_CACHE_ROWS as usize).saturating_sub(existing.len());
    let chosen: Vec<_> = entries.iter()
        .filter(|entry| !existing.contains(&entry.key))
        .take(available)
        .collect();
    let mut insert = db.prepare("INSERT INTO translation_cache(key,body) VALUES (?1,?2)")
        .map_err(|error| error.to_string())?;
    // Restore the selected archive entries in their original recency order.
    for entry in chosen.into_iter().rev() {
        let body = serde_json::to_string(&entry.segments).map_err(|error| error.to_string())?;
        insert.execute(params![entry.key, body]).map_err(|error| error.to_string())?;
    }
    let mut owner = db.prepare(
        "INSERT OR IGNORE INTO translation_cache_owners(cache_key,document_id)
         SELECT cache.key, docs.id FROM translation_cache cache, documents docs
         WHERE cache.key=?1 AND docs.source_hash=?2",
    ).map_err(|error| error.to_string())?;
    for entry in entries {
        for hash in std::iter::once(&entry.source_hash).chain(links.get(&entry.key).into_iter().flatten()) {
            owner.execute(params![entry.key, hash]).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelEdge {
    pub source: String,
    pub target: String,
    /// Identifier pinned to an installed model and its tokenizer/configuration.
    pub revision: String,
    /// Comparable measured quality on a 0–1000 scale. None means unevaluated.
    pub quality: Option<u16>,
}

impl ModelEdge {
    fn same_package(&self, other: &Self) -> bool {
        self.source == other.source && self.target == other.target && self.revision == other.revision
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Route {
    Direct(ModelEdge),
    Pivot { first: ModelEdge, second: ModelEdge },
}

impl Route {
    pub fn pivot_language(&self) -> Option<&str> {
        match self {
            Self::Direct(_) => None,
            Self::Pivot { first, .. } => Some(&first.target),
        }
    }

    pub(crate) fn without_quality(&self) -> Self {
        let mut route = self.clone();
        match &mut route {
            Self::Direct(edge) => edge.quality = None,
            Self::Pivot { first, second } => {
                first.quality = None;
                second.quality = None;
            }
        }
        route
    }
}

pub fn resolve_route(installed: &[ModelEdge], source: &str, target: &str) -> Result<Option<Route>> {
    Ok(ranked_routes(installed, source, target)?.into_iter().next())
}

pub(crate) fn ranked_routes(installed: &[ModelEdge], source: &str, target: &str) -> Result<Vec<Route>> {
    valid_language(source)?;
    valid_language(target)?;
    if source == target {
        return Err("Мова перекладу має відрізнятися від мови джерела".into());
    }
    for edge in installed {
        valid_language(&edge.source)?;
        valid_language(&edge.target)?;
        if edge.source == edge.target
            || edge.revision.trim().is_empty()
            || edge.quality > Some(1000)
        {
            return Err("Некоректні дані мовного пакета".into());
        }
    }

    let mut routes: Vec<_> = catalog_routes(installed, source, target).collect();
    routes.sort_by(|left, right| compare_routes(right, left));
    Ok(routes)
}

fn compare_routes(left: &Route, right: &Route) -> Ordering {
    match (left, right) {
        (Route::Direct(left), Route::Direct(right)) => left
            .quality
            .cmp(&right.quality)
            .then_with(|| right.revision.cmp(&left.revision)),
        (Route::Direct(_), Route::Pivot { .. }) => Ordering::Greater,
        (Route::Pivot { .. }, Route::Direct(_)) => Ordering::Less,
        (
            Route::Pivot { first: left_first, second: left_second },
            Route::Pivot { first: right_first, second: right_second },
        ) => {
            let rank = |first: &ModelEdge, second: &ModelEdge| {
                (first.quality.zip(second.quality).map(|(a, b)| a.min(b)), first.target == "en")
            };
            rank(left_first, left_second)
                .cmp(&rank(right_first, right_second))
                .then_with(|| right_first.target.cmp(&left_first.target))
                .then_with(|| right_first.revision.cmp(&left_first.revision))
                .then_with(|| right_second.revision.cmp(&left_second.revision))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationPair {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationReadiness {
    pub ready: bool,
    pub pivot_language: Option<String>,
    pub missing: Vec<TranslationPair>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationRouteStatus {
    pub source: String,
    pub target: String,
    pub ready: bool,
    pub pivot_language: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranslationSelection {
    pub(crate) page: Option<u16>,
    pub(crate) revision_id: String,
    pub(crate) start_utf16: usize,
    pub(crate) text: String,
}

pub enum SelectedTranslation {
    Model { translation: AlignedTranslation, source_text: String },
    Cardinal { text: String, source_text: String },
}

pub(crate) fn route_readiness(
    catalog: &[ModelEdge],
    installed: &[ModelEdge],
    source: &str,
    target: &str,
) -> Result<TranslationReadiness> {
    if let Some(route) = resolve_route(installed, source, target)? {
        return Ok(TranslationReadiness {
            ready: true,
            pivot_language: route.pivot_language().map(str::to_owned),
            missing: Vec::new(),
        });
    }

    let pair = |edge: &ModelEdge| TranslationPair {
        source: edge.source.clone(),
        target: edge.target.clone(),
    };
    if let Some(edge) = catalog
        .iter()
        .find(|edge| edge.source == source && edge.target == target)
    {
        return Ok(TranslationReadiness {
            ready: false,
            pivot_language: None,
            missing: vec![pair(edge)],
        });
    }
    let (pivot_language, missing) = catalog
        .iter()
        .filter(|edge| edge.source == source)
        .flat_map(|first| {
            catalog
                .iter()
                .filter(move |second| second.source == first.target && second.target == target)
                .map(move |second| (first, second))
        })
        .map(|(first, second)| {
            let missing: Vec<_> = [first, second]
                .into_iter()
                .filter(|edge| !installed.iter().any(|present| present.same_package(edge)))
                .map(pair)
                .collect();
            (first.target.clone(), missing)
        })
        .min_by(|(left, left_missing), (right, right_missing)| {
            (left_missing.len(), left != "en", left).cmp(&(
                right_missing.len(),
                right != "en",
                right,
            ))
        })
        .map(|(pivot, missing)| (Some(pivot), missing))
        .unwrap_or_default();
    Ok(TranslationReadiness {
        ready: false,
        pivot_language,
        missing,
    })
}

#[derive(Debug)]
pub enum TranslationBatchError {
    Deferred { message: String, completed: Vec<Result<String>> },
    Failed(String),
}

impl TranslationBatchError {
    pub(crate) fn deferred(message: String) -> Self {
        Self::Deferred { message, completed: Vec::new() }
    }
}

impl From<String> for TranslationBatchError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<TranslationBatchError> for String {
    fn from(error: TranslationBatchError) -> Self {
        match error {
            TranslationBatchError::Deferred { message, .. } | TranslationBatchError::Failed(message) => message,
        }
    }
}

pub trait TranslationProvider {
    /// Inspect the local inventory without downloading or loading models for inference.
    fn installed_models(&self) -> Result<Vec<ModelEdge>>;

    /// Low-priority jobs can yield after the first attempt.
    fn allow_alternative_routes(&self) -> bool {
        true
    }

    /// Return one target string per source segment, preserving the segment order.
    /// Providers receive context in the input language of this model edge.
    fn translate(
        &mut self,
        model: &ModelEdge,
        segments: &[String],
        context: Option<&str>,
        settings: TranslationSettings,
    ) -> Result<Vec<String>>;

    /// Independent inputs may fail separately. A deferral stops work across all routes.
    /// Its completed results form a prefix, including any individual failures.
    fn translate_independent(
        &mut self,
        model: &ModelEdge,
        segments: &[String],
        settings: TranslationSettings,
    ) -> std::result::Result<Vec<Result<String>>, TranslationBatchError> {
        checked_inference(self, model, segments, None, settings)
            .map(|output| output.into_iter().map(Ok).collect())
            .map_err(TranslationBatchError::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AlignedTranslation {
    pub route: Route,
    /// Each entry stays paired to its source. A target entry may contain several sentences.
    pub segments: Vec<String>,
    pub joined_lines: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alignment: Option<TranslationAlignment>,
}

impl AlignedTranslation {
    fn from_route(route: Route, segments: Vec<String>) -> Self {
        Self { route, segments, joined_lines: false, alignment: None }
    }

    pub(crate) fn attach_source_sentences(&mut self, sentence_ids: Vec<String>) {
        self.joined_lines = sentence_ids.len() > 1;
        self.alignment = self
            .segments
            .first()
            .map(|text| TranslationAlignment::from_text(&sentence_ids, text));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AlignmentKind {
    #[serde(rename = "1:1")]
    OneToOne,
    #[serde(rename = "1:N")]
    OneToMany,
    #[serde(rename = "N:1")]
    ManyToOne,
    #[serde(rename = "N:M")]
    ManyToMany,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationAlignment {
    pub kind: AlignmentKind,
    /// The source sentences submitted together, in document order.
    pub source_sentence_ids: Vec<String>,
    /// Spans in the returned target text, ordered by appearance.
    pub target_sentences: Vec<crate::ocr::OcrSpan>,
}

impl TranslationAlignment {
    fn from_text(source_ids: &[String], text: &str) -> Self {
        let target_sentences: Vec<_> = crate::segment(text, "target")
            .into_iter()
            .map(|sentence| crate::ocr::OcrSpan {
                start_utf8: sentence.start_utf8,
                end_utf8: sentence.end_utf8,
                start_utf16: sentence.start_utf16,
                end_utf16: sentence.end_utf16,
            })
            .collect();
        let kind = match (source_ids.len(), target_sentences.len()) {
            (1, 1) => AlignmentKind::OneToOne,
            (1, _) => AlignmentKind::OneToMany,
            (_, 1) => AlignmentKind::ManyToOne,
            _ => AlignmentKind::ManyToMany,
        };
        Self {
            kind,
            source_sentence_ids: source_ids.to_vec(),
            target_sentences,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParagraphTranslation {
    pub units: Vec<ParagraphUnit>,
    pub pivot_language: Option<String>,
    pub more_before: bool,
    pub more_after: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParagraphUnit {
    pub sentence_ids: Vec<String>,
    pub translation: Option<String>,
    pub reviewed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pivot_language: Option<String>,
    #[serde(skip)]
    pub(crate) pdf_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alignment: Option<TranslationAlignment>,
}

impl ParagraphUnit {
    fn set_translation(&mut self, text: String) {
        self.alignment = Some(TranslationAlignment::from_text(&self.sentence_ids, &text));
        self.translation = Some(text);
    }
}

pub struct CachedTranslation {
    pub paragraph: ParagraphTranslation,
    pub sentence: Option<AlignedTranslation>,
}

struct ParagraphPlan {
    result: ParagraphTranslation,
    pending: Vec<(usize, String)>,
}

#[derive(Clone, Copy)]
pub(crate) enum PageBoundaries {
    Canonical,
    Projected,
}

pub(crate) fn translate_paragraph(
    store: &mut Store,
    provider: &mut impl TranslationProvider,
    document: &crate::Document,
    selected: usize,
    target: &str,
    boundaries: PageBoundaries,
) -> std::result::Result<ParagraphTranslation, TranslationBatchError> {
    let ParagraphPlan {
        mut result,
        pending,
    } = paragraph_plan(store, document, selected, target, boundaries, UnitGrouping::Sentence)?;
    if !pending.is_empty() {
        let (indices, input): (Vec<_>, Vec<_>) = pending.into_iter().unzip();
        let translated = store.translate_units(provider, &document.id, &document.language, target, &input)?;
        result.pivot_language = translated.preferred_pivot;
        for (index, translation) in indices.into_iter().zip(translated.segments) {
            if let Some(translation) = translation {
                let unit = &mut result.units[index];
                if unit.sentence_ids.contains(&document.sentences[selected].id) {
                    result.pivot_language = translation.pivot_language.clone();
                }
                unit.set_translation(translation.text);
                unit.pivot_language = translation.pivot_language;
            }
        }
    }
    Ok(result)
}

pub(crate) fn translate_pdf_page(
    store: &mut Store,
    provider: &mut impl TranslationProvider,
    document: &Document,
    physical: Range<usize>,
    target: &str,
    boundaries: PageBoundaries,
    canceled: &dyn Fn() -> bool,
) -> std::result::Result<Vec<ParagraphUnit>, TranslationBatchError> {
    let positions: HashMap<_, _> = physical.clone()
        .map(|index| (document.sentences[index].id.as_str(), index))
        .collect();
    let mut covered = HashSet::new();
    let mut units = Vec::new();
    let mut pending = Vec::new();
    for selected in physical {
        if canceled() {
            return Err(TranslationBatchError::deferred("Переклад сторінки скасовано.".into()));
        }
        if covered.contains(&selected) { continue; }
        let plan = paragraph_plan(store, document, selected, target, boundaries, UnitGrouping::Sentence)?;
        let mut input: HashMap<_, _> = plan.pending.into_iter().collect();
        for (index, unit) in plan.result.units.into_iter().enumerate() {
            let mut unseen = false;
            for id in &unit.sentence_ids {
                if let Some(&position) = positions.get(id.as_str()) {
                    unseen |= covered.insert(position);
                }
            }
            if !unseen { continue; }
            if let Some(text) = input.remove(&index).filter(|text| !text.trim().is_empty()) {
                pending.push((units.len(), text));
            }
            units.push(unit);
        }
    }

    // Length expansion in the target language must fit within the provider's response limit.
    const PAGE_BATCH_BYTES: usize = MAX_BATCH_BYTES / 4;
    let mut pending = pending.into_iter().peekable();
    while pending.peek().is_some() {
        if canceled() {
            return Err(TranslationBatchError::deferred("Переклад сторінки скасовано.".into()));
        }
        let mut batch = Vec::new();
        let mut indices = Vec::new();
        let mut bytes = 0;
        while let Some((_, text)) = pending.peek() {
            if !batch.is_empty() && (batch.len() == 32 || bytes + text.len() > PAGE_BATCH_BYTES) {
                break;
            }
            let (index, text) = pending.next().expect("pending page input");
            bytes += text.len();
            batch.push(text);
            indices.push(index);
        }
        let translated = store.translate_units(provider, &document.id, &document.language, target, &batch)?;
        for (index, translation) in indices.into_iter().zip(translated.segments) {
            if let Some(translation) = translation {
                units[index].set_translation(translation.text);
                units[index].pivot_language = translation.pivot_language;
            }
        }
    }
    Ok(units)
}

pub(crate) fn cached_translation(
    store: &Store,
    installed: &[ModelEdge],
    catalog: &[ModelEdge],
    document: &crate::Document,
    selected: usize,
    target: &str,
) -> Result<CachedTranslation> {
    let routes = CacheRoutes::resolve(installed, catalog, &document.language, target)?;
    routes.cached_window(store, document, selected, target, PageBoundaries::Canonical)
}

pub(crate) struct CacheRoutes {
    preferred: Option<Route>,
    installed_count: usize,
    candidates: Vec<Route>,
}

impl CacheRoutes {
    pub(crate) fn resolve(
        installed: &[ModelEdge],
        catalog: &[ModelEdge],
        source: &str,
        target: &str,
    ) -> Result<Self> {
        let mut candidates = ranked_routes(installed, source, target)?;
        let installed_count = candidates.len();
        let preferred = candidates.first().cloned();
        let installed_identities: Vec<_> = candidates.iter().map(Route::without_quality).collect();
        candidates.extend(
            catalog_routes(catalog, source, target)
                .filter(|route| !installed_identities.contains(route)),
        );
        Ok(Self {
            preferred,
            installed_count,
            candidates,
        })
    }

    pub(crate) fn cached_window(
        &self, store: &Store, document: &Document, position: usize, target: &str, boundaries: PageBoundaries,
    ) -> Result<CachedTranslation> {
        cached_translation_with_routes(store, self, document, position, target, CacheScope::Window, boundaries)
    }

    pub(crate) fn cached_pdf_row(
        &self, store: &Store, document: &Document, position: usize, target: &str,
        boundaries: PageBoundaries, grouping: UnitGrouping,
    ) -> Result<Option<ParagraphUnit>> {
        let active = &document.sentences[position].id;
        let plan = paragraph_plan(store, document, position, target, boundaries, grouping)?;
        let (source, ids) = sentence_source(store, document, position, target, boundaries)?;
        let input = (prepare_translation_input(&document.language, target, source.clone()), ids);
        let cached = read_cached_plan(store, self, active, target, CacheScope::Active, plan, &input)?;
        let unit = cached.paragraph.units.into_iter()
            .find(|unit| unit.translation.is_some() && unit.sentence_ids.contains(active));
        Ok(unit.or_else(|| cached.sentence.map(|aligned| ParagraphUnit {
            sentence_ids: input.1,
            translation: aligned.segments.into_iter().next(),
            reviewed: false,
            pivot_language: aligned.route.pivot_language().map(str::to_owned),
            pdf_source: Some(source),
            alignment: aligned.alignment,
        })))
    }

    pub(crate) fn cached_active(
        &self, store: &Store, document: &crate::Document, position: usize, target: &str, for_search: bool,
    ) -> Result<CachedTranslation> {
        let scope = if for_search { CacheScope::Search } else { CacheScope::Active };
        cached_translation_with_routes(store, self, document, position, target, scope, PageBoundaries::Projected)
    }

    pub(crate) fn cached_standalone(
        &self, store: &Store, text: &str, target: &str,
    ) -> Result<Option<(String, Option<String>)>> {
        Ok(self.cached_sentence(store, text, target)?.map(|cached| {
            (
                cached.segments.into_iter().next().expect("one cached segment"),
                cached.route.pivot_language().map(str::to_owned),
            )
        }))
    }

    pub(crate) fn cached_sentence(
        &self,
        store: &Store,
        text: &str,
        target: &str,
    ) -> Result<Option<AlignedTranslation>> {
        self.cached_contextual_selection(store, text, None, target)
    }

    pub(crate) fn cached_contextual_selection(
        &self, store: &Store, text: &str, context: Option<&str>, target: &str,
    ) -> Result<Option<AlignedTranslation>> {
        if text.len() > MAX_BATCH_BYTES {
            return Ok(None);
        }
        let cached = self.cached_aligned(store, &[text], context, target, TranslationSettings::default())?;
        Ok(cached.map(|(translation, _)| translation))
    }

    fn cached_aligned(
        &self,
        store: &Store,
        input: &[impl Serialize + AsRef<str>],
        context: Option<&str>,
        target: &str,
        settings: TranslationSettings,
    ) -> Result<Option<(AlignedTranslation, String)>> {
        for route in &self.candidates {
            let key = cache_key(route, input, context, settings)?;
            if let Some(segments) = store.cached_segments(&key, input.len(), target)? {
                return Ok(Some((AlignedTranslation::from_route(route.clone(), segments), key)));
            }
        }
        Ok(None)
    }
}

#[derive(Clone, Copy)]
enum CacheScope {
    Window,
    Active,
    Search,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitGrouping {
    Sentence,
    LegacyContext,
}

fn cached_translation_with_routes(
    store: &Store,
    routes: &CacheRoutes,
    document: &crate::Document,
    selected: usize,
    target: &str,
    scope: CacheScope,
    boundaries: PageBoundaries,
) -> Result<CachedTranslation> {
    let active_id = &document.sentences[selected].id;
    let aligned_plan = paragraph_plan(store, document, selected, target, boundaries, UnitGrouping::Sentence)?;
    let input = sentence_input(store, document, selected, target, boundaries)?;
    let aligned = read_cached_plan(store, routes, active_id, target, scope, aligned_plan, &input)?;
    if (routes.preferred.is_some() && !matches!(scope, CacheScope::Search)) ||
        aligned.sentence.is_some() || aligned.paragraph.units.iter().any(|unit|
            unit.translation.is_some() && unit.sentence_ids.contains(active_id))
    {
        return Ok(aligned);
    }
    let legacy_plan = paragraph_plan(store, document, selected, target, boundaries, UnitGrouping::LegacyContext)?;
    let legacy = read_cached_plan(store, routes, active_id, target, scope, legacy_plan, &input)?;
    if legacy.paragraph.units.iter().any(|unit|
        unit.translation.is_some() && unit.sentence_ids.contains(active_id))
    {
        return Ok(legacy);
    }
    Ok(aligned)
}

fn read_cached_plan(
    store: &Store, routes: &CacheRoutes, active_id: &str, target: &str, scope: CacheScope,
    ParagraphPlan { result: mut paragraph, pending }: ParagraphPlan,
    input: &(String, Vec<String>),
) -> Result<CachedTranslation> {
    paragraph.pivot_language = routes
        .preferred
        .as_ref()
        .and_then(Route::pivot_language)
        .map(str::to_owned);

    let active_reviewed = paragraph
        .units
        .iter()
        .any(|unit| unit.reviewed && unit.sentence_ids.iter().any(|id| id == active_id));
    let standalone = (!active_reviewed && !input.0.is_empty() && input.0.len() <= MAX_BATCH_BYTES).then_some(input);
    let active = |unit: &ParagraphUnit| unit.sentence_ids.iter().any(|id| id == active_id);
    let active_input = pending.iter().find(|(index, _)| active(&paragraph.units[*index]));
    let cached_unit = |route: &Route, text: &str| -> Result<Option<String>> {
        Ok(store.cached_segments(&unit_cache_key(route, text)?, 1, target)?
            .and_then(|mut segments| segments.pop()))
    };
    let mut sentence = None;
    let mut anchored_route = None;
    for route in &routes.candidates {
        if let Some((index, text)) = active_input {
            if let Some(translation) = cached_unit(route, text)? {
                let unit = &mut paragraph.units[*index];
                unit.set_translation(translation);
                unit.pivot_language = route.pivot_language().map(str::to_owned);
                anchored_route = Some(route);
                break;
            }
        }
        if let Some((text, sentence_ids)) = standalone {
            if let Some(translation) = cached_unit(route, text)? {
                let mut aligned = AlignedTranslation {
                    route: route.clone(),
                    segments: vec![translation],
                    joined_lines: false,
                    alignment: None,
                };
                aligned.attach_source_sentences(sentence_ids.clone());
                sentence = Some(aligned);
                anchored_route = Some(route);
                break;
            }
        }
    }
    if let Some(route) = anchored_route {
        paragraph.pivot_language = route.pivot_language().map(str::to_owned);
    }
    if matches!(scope, CacheScope::Window) {
        // Prefer the active sentence's route when it has neighboring translations.
        // Fill missing neighbors from other cached routes with their own provenance.
        let neighbors = anchored_route.into_iter().chain(
            routes.candidates.iter().filter(|route| Some(*route) != anchored_route)
        );
        for route in neighbors {
            for (index, text) in &pending {
                let unit = &mut paragraph.units[*index];
                if unit.translation.is_some() || active(unit) {
                    continue;
                }
                if let Some(translation) = cached_unit(route, text)? {
                    unit.set_translation(translation);
                    unit.pivot_language = route.pivot_language().map(str::to_owned);
                }
            }
        }
    }
    Ok(CachedTranslation {
        paragraph,
        sentence,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationSearchPage {
    pub matches: Vec<TranslationSearchMatch>,
    pub next_start: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct TranslationSearchMatch {
    pub position: usize,
    pub text: String,
}

pub(crate) fn search_cached_translation(
    store: &Store,
    installed: &[ModelEdge],
    catalog: &[ModelEdge],
    document: &crate::Document,
    target: &str,
    query: &str,
    start: usize,
) -> Result<TranslationSearchPage> {
    const SCAN_LIMIT: usize = 128;
    const MATCH_LIMIT: usize = 30;

    let routes = CacheRoutes::resolve(installed, catalog, &document.language, target)?;
    let query = normalize_translation_search(query.trim());
    let mut page = TranslationSearchPage {
        matches: Vec::new(),
        next_start: None,
    };
    let len = document.sentences.len();
    if query.is_empty() || start >= len {
        return Ok(page);
    }

    let end = len.min(start.saturating_add(SCAN_LIMIT));
    let mut previous_unit = None;
    // The preceding position lets us suppress a repeated contextual unit at a page boundary.
    let scan_start = start.saturating_sub(1);
    for position in scan_start..end {
        let active_id = &document.sentences[position].id;
        let cached = cached_translation_with_routes(
            store,
            &routes,
            document,
            position,
            target,
            CacheScope::Search, PageBoundaries::Canonical,
        )?;
        let active_unit = cached
            .paragraph
            .units
            .into_iter()
            .find(|unit| unit.sentence_ids.contains(active_id));
        let contextual =
            active_unit.and_then(|unit| unit.translation.map(|text| (unit.sentence_ids, text)));
        let repeated = contextual.is_some() && contextual == previous_unit;
        let active = contextual
            .as_ref()
            .map(|(_, text)| text.as_str())
            .or_else(|| {
                cached
                    .sentence
                    .as_ref()?
                    .segments
                    .first()
                    .map(String::as_str)
            });
        if let Some(text) = active.filter(|text| {
            position >= start && !repeated && normalize_translation_search(text).contains(&query)
        }) {
            page.matches.push(TranslationSearchMatch {
                position,
                text: text.to_owned(),
            });
            if page.matches.len() == MATCH_LIMIT {
                page.next_start = (position + 1 < len).then_some(position + 1);
                return Ok(page);
            }
        }
        previous_unit = contextual;
    }
    page.next_start = (end < len).then_some(end);
    Ok(page)
}

fn normalize_translation_search(text: &str) -> String {
    text.nfc()
        .flat_map(|character| match character {
            '’' | 'ʼ' => '\''.to_lowercase(),
            character => character.to_lowercase(),
        })
        .collect()
}

fn catalog_routes<'a>(
    catalog: &'a [ModelEdge],
    source: &'a str,
    target: &'a str,
) -> impl Iterator<Item = Route> + 'a {
    let direct = catalog
        .iter()
        .filter(move |edge| edge.source == source && edge.target == target)
        .cloned()
        .map(Route::Direct);
    let pivots = catalog
        .iter()
        .filter(move |first| first.source == source && first.target != target)
        .flat_map(move |first| {
            catalog
                .iter()
                .filter(move |second| second.source == first.target && second.target == target)
                .map(move |second| Route::Pivot {
                    first: first.clone(),
                    second: second.clone(),
                })
        });
    direct.chain(pivots)
}

fn paragraph_plan(
    store: &Store, document: &Document, selected: usize, target: &str, boundaries: PageBoundaries,
    grouping: UnitGrouping,
) -> Result<ParagraphPlan> {
    valid_language(target)?;
    if document.language == target {
        return Err("Мова перекладу має відрізнятися від мови джерела".into());
    }

    const CONTEXT_SENTENCES: usize = 4;
    const CONTEXT_GROUP: usize = 3;
    const CONTEXT_BYTES: usize = 768;
    let sentences = &document.sentences;
    let skip = |index| crate::document_layout::skip_in_translation(document, &sentences[index]);
    if skip(selected) {
        return Ok(ParagraphPlan {
            result: ParagraphTranslation { units: Vec::new(), pivot_language: None, more_before: false, more_after: false },
            pending: Vec::new(),
        });
    }
    let paragraph = document.paragraph_for_sentence(&sentences[selected]);
    let mut first = sentences
        .partition_point(|sentence| sentence.end_utf8 <= paragraph.start)
        .min(selected);
    let mut last = sentences
        .partition_point(|sentence| sentence.start_utf8 < paragraph.end)
        .max(selected + 1);
    let reviews = document.reviewed_translations.get(target);
    let reviewed = |index: usize| reviews.and_then(|entries| entries.get(&sentences[index].id));
    let can_wrap = |left: usize, right: usize| {
        (document.format == "pdf" || document.ocr_lines.is_some())
            && !skip(left) && !skip(right)
            && reviewed(left).is_none()
            && reviewed(right).is_none()
            && soft_wrap(document, left, right)
    };
    if first > 0 && page_word_break(document, first - 1, first) && can_wrap(first - 1, first) {
        first -= 1;
    }
    if last < sentences.len() && page_word_break(document, last - 1, last) && can_wrap(last - 1, last)
    {
        last += 1;
    }
    let mut start = first.max(selected.saturating_sub(CONTEXT_SENTENCES));
    let mut end = last.min(selected + CONTEXT_SENTENCES + 1);
    if grouping == UnitGrouping::LegacyContext {
        start -= (start - first) % CONTEXT_GROUP;
        end = last.min(end + (CONTEXT_GROUP - (end - first) % CONTEXT_GROUP) % CONTEXT_GROUP);
    }
    // A nearby window boundary must not cut a strictly continuing physical line.
    while start > first && start + 11 > selected && can_wrap(start - 1, start) {
        start -= 1;
    }
    while end < last && end - selected < 12 && can_wrap(end - 1, end) {
        end += 1;
    }

    let mut units = Vec::new();
    let mut pending = Vec::new();
    let mut active_unit = 0;
    let mut index = start;
    while index < end {
        if skip(index) {
            index += 1;
            continue;
        }
        let mut next = index + 1;
        while next < end
            && !skip(next)
            && next - index < 12
            && reviewed(index).is_none()
            && reviewed(next).is_none()
        {
            let bytes = sentences[next]
                .end_utf8
                .saturating_sub(sentences[index].start_utf8);
            let continues_context = grouping == UnitGrouping::LegacyContext && next - index < CONTEXT_GROUP
                && (next - first) % CONTEXT_GROUP != 0
                && bytes <= CONTEXT_BYTES;
            if !(continues_context || (bytes <= 1024 && can_wrap(next - 1, next))) {
                break;
            }
            next += 1;
        }
        if index <= selected && selected < next {
            active_unit = units.len();
        }
        let review = reviewed(index).cloned();
        let pdf_source = if review.is_none() {
            let input = inference_span(document, index, next - 1)
                .ok_or("Межі абзацу пошкоджені.")?;
            let input = match boundaries {
                PageBoundaries::Canonical => with_adjacent_pdf_text(store, document, index, next - 1, target, input)?,
                PageBoundaries::Projected => input,
            };
            let pdf_source = (document.format == "pdf").then(|| input.clone());
            let input = prepare_translation_input(&document.language, target, input);
            pending.push((units.len(), input));
            pdf_source
        } else {
            (document.format == "pdf").then(|| sentences[index].text.clone())
        };
        let mut unit = ParagraphUnit {
            sentence_ids: sentences[index..next]
                .iter()
                .map(|sentence| sentence.id.clone())
                .collect(),
            reviewed: review.is_some(),
            pivot_language: None,
            translation: None,
            pdf_source,
            alignment: None,
        };
        if let Some(text) = review {
            unit.set_translation(text);
        }
        units.push(unit);
        index = next;
    }
    let mut budget = MAX_BATCH_BYTES;
    pending.sort_by_key(|(index, _)| (index.abs_diff(active_unit), *index));
    pending.retain(|(_, input)| {
        if input.len() > budget {
            return false;
        }
        budget -= input.len();
        true
    });
    pending.sort_by_key(|(index, _)| *index);
    Ok(ParagraphPlan {
        result: ParagraphTranslation {
            units,
            pivot_language: None,
            more_before: first < start,
            more_after: end < last,
        },
        pending,
    })
}

#[cfg(test)]
fn input_for_sentence(document: &crate::Document, selected: usize) -> (String, bool) {
    let (text, range) = sentence_span_input(document, selected);
    (text, range.start() != range.end())
}

fn sentence_span_input(
    document: &Document,
    selected: usize,
) -> (String, RangeInclusive<usize>) {
    const MAX_JOINED_BYTES: usize = 1024;
    const MAX_FRAGMENTS: usize = 12;
    let active = &document.sentences[selected];
    if crate::document_layout::skip_in_translation(document, active) {
        return (String::new(), selected..=selected);
    }
    if document.format != "pdf" && document.ocr_lines.is_none() {
        return (active.text.clone(), selected..=selected);
    }
    let within_budget = |left: usize, right: usize| {
        right - left < MAX_FRAGMENTS
            && document.sentences[right]
                .end_utf8
                .saturating_sub(document.sentences[left].start_utf8)
                <= MAX_JOINED_BYTES
    };
    let mut start = selected;
    let mut end = selected;
    while start > 0 && within_budget(start - 1, end) && soft_wrap(document, start - 1, start) {
        start -= 1;
    }
    while end + 1 < document.sentences.len()
        && within_budget(start, end + 1)
        && soft_wrap(document, end, end + 1)
    {
        end += 1;
    }
    if let Some(input) = inference_span(document, start, end) {
        (input, start..=end)
    } else {
        (active.text.clone(), selected..=selected)
    }
}

pub(crate) fn sentence_input(
    store: &Store, document: &Document, selected: usize, target: &str, boundaries: PageBoundaries,
) -> Result<(String, Vec<String>)> {
    let (input, ids) = sentence_source(store, document, selected, target, boundaries)?;
    Ok((prepare_translation_input(&document.language, target, input), ids))
}

pub(crate) fn sentence_source(
    store: &Store, document: &Document, selected: usize, target: &str, boundaries: PageBoundaries,
) -> Result<(String, Vec<String>)> {
    let (input, range) = sentence_span_input(document, selected);
    let input = match boundaries {
        PageBoundaries::Canonical => with_adjacent_pdf_text(store, document, *range.start(), *range.end(), target, input)?,
        PageBoundaries::Projected => input,
    };
    let ids = document.sentences[range].iter().map(|sentence| sentence.id.clone()).collect();
    Ok((input, ids))
}

pub(crate) fn prepare_translation_input(source: &str, target: &str, text: String) -> String {
    if source == "fr" && target == "uk" {
        let digits = crate::french_cardinals::digits_in_text(&text);
        crate::marian_target_uk::clarify_french_instructions(&digits)
    } else {
        text
    }
}

fn with_adjacent_pdf_text(
    store: &Store, document: &Document, first: usize, last: usize, target: &str, input: String,
) -> Result<String> {
    if document.pdf_page_starts.is_none() || input.is_empty() || input.len() > 1024 {
        return Ok(input);
    }
    let reviews = document.reviewed_translations.get(target);
    let unreviewed = |index: usize| {
        let sentence = &document.sentences[index];
        reviews.is_none_or(|reviews| !reviews.contains_key(&sentence.id)).then_some(sentence)
    };
    let (previous, next) = word_break::canonical_edge_fragments(
        store, document, Some(target), unreviewed(first), unreviewed(last),
    )?;
    if previous.is_none() && next.is_none() {
        return Ok(input);
    }
    let detached = Document::default();
    let normalize = |fragment: String| normalize_ocr_input(&detached, &fragment, 0);
    let previous = previous.map(normalize);
    let next = next.map(normalize);
    if let Some(joined) = word_break::join_fragments(&input, previous.as_deref(), next.as_deref()) {
        if joined.text.len() <= 1024 {
            return Ok(joined.text);
        }
    }
    Ok(input)
}

fn inference_span(document: &crate::Document, first: usize, last: usize) -> Option<String> {
    let first = &document.sentences[first];
    let last = &document.sentences[last];
    let span = document.text.get(first.start_utf8..last.end_utf8)?;
    if document.format != "pdf" && document.ocr_lines.is_none() {
        return Some(if first.id == last.id {
            first.text.clone()
        } else {
            span.split_whitespace().collect::<Vec<_>>().join(" ")
        });
    }
    let normalized = if first.id == last.id && !span.contains('\n') {
        let cleaned = crate::document_layout::mask_pdf_translation_input(document, span, first.start_utf8);
        cleaned.map_or_else(|| first.text.clone(), |text| normalize_ocr_input(document, &text, first.start_utf16))
    } else {
        normalize_pdf_translation_input(document, span, first.start_utf8, first.start_utf16)
    };
    let prose = normalized.trim_start_matches(['»', '”', '›', '」', '』']).trim_start();
    Some(prose.to_owned())
}

pub(crate) fn normalize_pdf_translation_input(
    document: &Document, text: &str, start_utf8: usize, start_utf16: usize,
) -> String {
    let input = crate::document_layout::mask_pdf_translation_input(document, text, start_utf8);
    normalize_ocr_input(document, input.as_deref().unwrap_or(text), start_utf16)
}

pub(crate) fn normalize_ocr_input(
    document: &crate::Document,
    span: &str,
    mut offset: usize,
) -> String {
    let mut normalized = String::with_capacity(span.len());
    let mut rest = span;
    while !rest.is_empty() {
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, separator_and_tail) = rest.split_at(word_end);
        let tail = separator_and_tail.trim_start();
        let gap = &separator_and_tail[..separator_and_tail.len() - tail.len()];
        let word_end_utf16 = offset + word.encode_utf16().count();
        let tail_start_utf16 = word_end_utf16 + gap.encode_utf16().count();
        let line_breaks = gap.bytes().filter(|&byte| byte == b'\n').count();
        let continued = word_hyphen(word).filter(|_| {
            tail.starts_with(char::is_lowercase)
                && (line_breaks == 1
                    || line_breaks == 2
                        && adjacent_pdf_page(document, word_end_utf16, tail_start_utf16))
        });
        let drop_cap = line_breaks == 1
            && word.chars().count() == 1
            && word.starts_with(char::is_uppercase)
            && tail.starts_with(char::is_alphabetic)
            && split_ocr_drop_cap(document, offset, word_end_utf16, tail_start_utf16);
        if let Some(prefix) = continued {
            let next = tail.split(|ch: char| !ch.is_alphabetic()).next().unwrap_or("");
            let inversion = document.language == "fr" && match next {
                // "ce" is also a common line-ending suffix, as in "exerci-\nce".
                "ce" => matches!(prefix.to_lowercase().as_str(),
                    "est" | "était" | "sera" | "serait" | "fut" | "fût" | "sont" | "étaient"),
                "il" | "ils" | "elle" | "elles" | "on" | "tu" | "vous" | "nous" | "je" => true,
                _ => false,
            };
            let compound = prefix.trim_start_matches(['-', '\u{2010}']).contains(['-', '\u{2010}']);
            normalized.push_str(if inversion || compound { word } else { prefix });
        } else if drop_cap {
            normalized.push_str(word);
        } else {
            normalized.push_str(word);
            if !tail.is_empty() {
                normalized.push(' ');
            }
        }
        rest = tail;
        offset = tail_start_utf16;
    }
    normalized
}

fn split_ocr_drop_cap(document: &Document, start: usize, end: usize, tail: usize) -> bool {
    document.ocr_lines.as_ref().is_some_and(|lines| {
        lines.windows(2).any(|pair| {
            let [cap, body] = pair else { return false };
            let (Some(cap_span), Some(body_span)) = (cap.source_span, body.source_span) else {
                return false;
            };
            let gap = body.x - (cap.x + cap.width);
            cap.page == body.page
                && cap_span.start_utf16 == start
                && cap_span.end_utf16 == end
                && body_span.start_utf16 == tail
                && (1.35 * body.height..=5.0 * body.height).contains(&cap.height)
                && (-0.15 * body.height..=0.55 * body.height).contains(&gap)
                && (cap.y - 0.20 * body.height..=cap.y + 0.85 * body.height).contains(&body.y)
        })
    })
}

pub(crate) fn word_hyphen(word: &str) -> Option<&str> {
    word.strip_suffix(['-', '\u{00ad}', '\u{2010}'])
        .filter(|prefix| prefix.ends_with(char::is_alphabetic))
}

fn adjacent_pdf_page(document: &crate::Document, left: usize, right: usize) -> bool {
    document.pdf_page_starts.as_ref().is_some_and(|starts| {
        starts.windows(2).any(|pair| {
            pair[0].is_some_and(|start| start < left) && pair[1] == Some(right)
        })
    })
}

fn page_word_break(document: &crate::Document, left: usize, right: usize) -> bool {
    let previous = &document.sentences[left];
    let next = &document.sentences[right];
    word_hyphen(&previous.text).is_some()
        && next.text.starts_with(char::is_lowercase)
        && adjacent_pdf_page(document, previous.end_utf16, next.start_utf16)
        && document.text.get(previous.end_utf8..next.start_utf8).is_some_and(|gap| {
            gap.chars().all(char::is_whitespace)
                && gap.bytes().filter(|&byte| byte == b'\n').count() == 2
        })
}

fn soft_wrap(document: &crate::Document, left: usize, right: usize) -> bool {
    if [left, right].into_iter().any(|index|
        crate::document_layout::skip_in_translation(document, &document.sentences[index]))
    {
        return false;
    }
    if page_word_break(document, left, right) {
        return true;
    }
    let previous = &document.sentences[left];
    let next = &document.sentences[right];
    let Some(gap) = document.text.get(previous.end_utf8..next.start_utf8) else {
        return false;
    };
    if gap.bytes().filter(|&byte| byte == b'\n').count() != 1
        || gap
            .chars()
            .any(|ch| !matches!(ch, ' ' | '\t' | '\r' | '\n' | '\u{a0}'))
    {
        return false;
    }
    let previous_end = previous.text.trim_end_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '"' | '\'' | '’' | '”' | '»' | ')' | ']')
    });
    !previous_end.ends_with(['.', '!', '?', ':', ';', '…', '。', '！', '？'])
        && next.text.starts_with(char::is_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn shared_translation_cache_survives_legacy_migration_backup_and_document_deletion() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("library.sqlite");
        let mut store = Store::open(&path).unwrap();
        let first = store.import("first.txt", b"Le bateau arrive.", "fr").unwrap();
        let second = store.import("second.txt", b"Le bateau arrive. Le marin part.", "fr").unwrap();
        let route = Route::Direct(ModelEdge {
            source: "fr".into(), target: "uk".into(), revision: "test".into(), quality: None,
        });
        let input = ["Le bateau arrive.".to_owned()];
        let key = unit_cache_key(&route, &input[0]).unwrap();
        let translated = ["Корабель прибуває.".to_owned()];
        store.save_cached_segments([(key.clone(), translated.as_slice())]).unwrap();

        store.connection.execute_batch(
            "DROP TABLE translation_cache_owners;
             CREATE TABLE translation_cache_owners (
                 cache_key TEXT PRIMARY KEY REFERENCES translation_cache(key) ON DELETE CASCADE,
                 document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE
             );",
        ).unwrap();
        store.connection.execute(
            "INSERT INTO translation_cache_owners VALUES (?1, ?2)", params![key, first.id],
        ).unwrap();
        drop(store);

        let mut store = Store::open(&path).unwrap();
        store.record_translation_cache_use(&second.id, &route, &input).unwrap();
        store.save_cached_segments([(key.clone(), translated.as_slice())]).unwrap();
        let (entries, links) = export_cache(&store.connection).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(links[&key].len(), 1);
        let mut bad_links = links;
        bad_links.insert(key.clone(), vec![entries[0].source_hash.clone()]);
        assert!(validate_cache(&entries, &bad_links, &[first.clone(), second.clone()]).is_err());

        let archive = root.path().join("library.zip");
        store.export_backup_file(&archive).unwrap();
        let restored_root = tempfile::tempdir().unwrap();
        let restored_path = restored_root.path().join("library.sqlite");
        let mut restored = Store::open(&restored_path).unwrap();
        restored.restore_backup_file(&archive).unwrap();
        assert_eq!(export_cache(&restored.connection).unwrap().1[&key].len(), 1);

        let original_owner = entries[0].source_hash.clone();
        let restored_document = restored.snapshot().unwrap().documents.into_iter()
            .find(|doc| doc.source_hash == original_owner).unwrap();
        assert!(restored.delete_document(&restored_document.id).unwrap());
        let (entries, links) = export_cache(&restored.connection).unwrap();
        assert_eq!(entries.len(), 1);
        assert_ne!(entries[0].source_hash, original_owner);
        assert_eq!(entries[0].segments, translated);
        assert!(links.is_empty());
    }

    fn french_uk_route_with_invalid_assets(store: &Store) -> Route {
        let package = store.translation_packages().into_iter()
            .find(|package| (package.source, package.target) == ("fr", "uk")).unwrap();
        let directory = store.model_directory.join(package.repository);
        std::fs::create_dir_all(&directory).unwrap();
        // Route discovery checks file sizes. Verification rejects the test bytes before inference.
        for asset in &package.assets {
            std::fs::File::create(directory.join(asset.name)).unwrap().set_len(asset.bytes).unwrap();
        }
        Route::Direct(ModelEdge {
            source: package.source.into(), target: package.target.into(),
            revision: package.revision.into(), quality: None,
        })
    }

    #[test]
    fn pdf_page_batches_missing_units_across_paragraphs_and_reuses_the_cache() {
        #[derive(Default)]
        struct Provider(Vec<Vec<String>>);

        impl TranslationProvider for Provider {
            fn installed_models(&self) -> Result<Vec<ModelEdge>> {
                Ok(vec![ModelEdge {
                    source: "fr".into(), target: "uk".into(), revision: "test".into(), quality: None,
                }])
            }

            fn translate(&mut self, _: &ModelEdge, segments: &[String], _: Option<&str>,
                _: TranslationSettings) -> Result<Vec<String>> {
                self.0.push(segments.to_vec());
                Ok(segments.iter().map(|_| "Збережене речення.".into()).collect())
            }
        }

        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let document = store.import("Page.txt", "Le bateau arrive.\n\nLe marin part.\n\nLe port est ouvert.".as_bytes(), "fr").unwrap();
        store.set_reviewed_translation(&document.id, &document.sentences[0].id,
            "uk", "Корабель прибуває.").unwrap();
        let document = store.translation_document(&document.id).unwrap();
        let mut provider = Provider::default();
        for _ in 0..2 {
            let rows = translate_pdf_page(&mut store, &mut provider, &document, 0..3,
                "uk", PageBoundaries::Canonical, &|| false).unwrap();
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].translation.as_deref(), Some("Корабель прибуває."));
            assert!(rows.iter().all(|row| row.translation.is_some()));
        }
        assert_eq!(provider.0, [vec!["Le marin part.".to_owned(), "Le port est ouvert.".to_owned()]]);
    }

    #[test]
    fn paragraph_keeps_direct_sentences_and_reuses_only_the_needed_pivot() {
        #[derive(Default)]
        struct Provider {
            pivot_installed: bool,
            deferred: bool,
            calls: Vec<(String, Vec<String>)>,
        }

        impl TranslationProvider for Provider {
            fn installed_models(&self) -> Result<Vec<ModelEdge>> {
                let pairs = if self.pivot_installed {
                    &[("fr", "uk"), ("fr", "en"), ("en", "uk")][..]
                } else {
                    &[("fr", "uk")][..]
                };
                Ok(pairs.iter().map(|&(source, target)| ModelEdge {
                    source: source.into(),
                    target: target.into(),
                    revision: "test".into(),
                    quality: None,
                }).collect())
            }

            fn translate(&mut self, _: &ModelEdge, _: &[String], _: Option<&str>,
                _: TranslationSettings) -> Result<Vec<String>> {
                unreachable!("reader batches use independent inputs");
            }

            fn translate_independent(&mut self, edge: &ModelEdge, input: &[String],
                _: TranslationSettings) -> std::result::Result<Vec<Result<String>>, TranslationBatchError> {
                self.calls.push((format!("{}-{}", edge.source, edge.target), input.to_vec()));
                if self.deferred {
                    return Err(TranslationBatchError::deferred("foreground is waiting".into()));
                }
                Ok(input.iter().map(|text| match (edge.source.as_str(), edge.target.as_str(), text.as_str()) {
                    ("fr", "uk", "Le bateau arrive.") => Ok("Корабель прибуває.".into()),
                    ("fr", "uk", "Le marin part.") => Err("invalid target language".into()),
                    ("fr", "uk", "Le port est ouvert.") => Ok("Порт відкритий.".into()),
                    ("fr", "en", "Le marin part.") => Ok("The sailor leaves.".into()),
                    ("en", "uk", "The sailor leaves.") => Ok("Моряк вирушає.".into()),
                    (_, _, text) => panic!("unexpected model input: {text}"),
                }).collect())
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("reader.db");
        let mut store = Store::open(&database).unwrap();
        let document = store.import("reader.txt", b"Le bateau arrive. Le marin part. Le port est ouvert.", "fr").unwrap();
        let mut provider = Provider::default();
        let partial = translate_paragraph(&mut store, &mut provider, &document, 1, "uk", PageBoundaries::Canonical).unwrap();
        assert_eq!(partial.units.iter().map(|unit| unit.translation.as_deref()).collect::<Vec<_>>(),
            [Some("Корабель прибуває."), None, Some("Порт відкритий.")]);
        assert_eq!(provider.calls.len(), 1);
        provider.calls.clear();
        provider.pivot_installed = true;
        provider.deferred = true;
        let deferred = translate_paragraph(&mut store, &mut provider, &document, 1, "uk", PageBoundaries::Canonical);
        assert!(matches!(deferred, Err(TranslationBatchError::Deferred { message, .. }) if message == "foreground is waiting"));
        assert_eq!(provider.calls, [("fr-uk".into(), vec!["Le marin part.".into()])]);
        provider.calls.clear();
        provider.deferred = false;

        let full = translate_paragraph(&mut store, &mut provider, &document, 1, "uk", PageBoundaries::Canonical).unwrap();
        assert_eq!(full.pivot_language.as_deref(), Some("en"));
        assert_eq!(full.units[1].translation.as_deref(), Some("Моряк вирушає."));
        assert_eq!(full.units.iter().map(|unit| unit.pivot_language.as_deref()).collect::<Vec<_>>(),
            [None, Some("en"), None]);
        assert_eq!(provider.calls, [
            ("fr-uk".into(), vec!["Le marin part.".into()]),
            ("fr-en".into(), vec!["Le marin part.".into()]),
            ("en-uk".into(), vec!["The sailor leaves.".into()]),
        ]);
        assert_eq!(store.connection.query_row(
            "SELECT count(*) FROM translation_cache_owners WHERE document_id=?1",
            [&document.id], |row| row.get::<_, u32>(0),
        ).unwrap(), 3);

        drop(store);
        let mut reopened = Store::open(&database).unwrap();
        provider.calls.clear();
        let models = provider.installed_models().unwrap();
        let cached = cached_translation(&reopened, &models, &models, &document, 1, "uk").unwrap();
        let full = serde_json::to_value(full).unwrap();
        assert_eq!(serde_json::to_value(cached.paragraph).unwrap(), full);
        let fresh_request = translate_paragraph(&mut reopened, &mut provider, &document, 1, "uk", PageBoundaries::Canonical).unwrap();
        assert_eq!(serde_json::to_value(fresh_request).unwrap(), full);
        assert!(provider.calls.is_empty());
    }

    #[test]
    fn completed_target_sentences_survive_a_direct_or_pivot_deferral() {
        struct Provider {
            models: Vec<ModelEdge>,
            suspend: bool,
            target_calls: Vec<Vec<String>>,
        }
        impl TranslationProvider for Provider {
            fn installed_models(&self) -> Result<Vec<ModelEdge>> { Ok(self.models.clone()) }

            fn translate(&mut self, _: &ModelEdge, _: &[String], _: Option<&str>,
                _: TranslationSettings) -> Result<Vec<String>> { unreachable!() }

            fn translate_independent(&mut self, edge: &ModelEdge, input: &[String],
                _: TranslationSettings) -> std::result::Result<Vec<Result<String>>, TranslationBatchError> {
                if edge.target == "en" {
                    return Ok(input.iter().map(|segment|
                        if segment == "B" && self.suspend { Err("missing pivot".into()) }
                        else { Ok(segment.clone()) }).collect());
                }
                self.target_calls.push(input.to_vec());
                let mut translated = input.iter().map(|segment| Ok(format!("Переклад {segment}")));
                if self.suspend {
                    self.suspend = false;
                    let completed = translated.by_ref().take(2).collect();
                    return Err(TranslationBatchError::Deferred { message: "foreground".into(), completed });
                }
                Ok(translated.collect())
            }
        }

        for pivot in [false, true] {
            let edge = |source: &str, target: &str| ModelEdge {
                source: source.into(), target: target.into(), revision: "test".into(), quality: None,
            };
            let models = if pivot { vec![edge("fr", "en"), edge("en", "uk")] }
                else { vec![edge("fr", "uk")] };
            let mut provider = Provider { models, suspend: true, target_calls: Vec::new() };
            let mut store = Store::open(Path::new(":memory:")).unwrap();
            let document = store.import("page.txt", b"Bonjour le monde.", "fr").unwrap();
            let input = ["A", "B", "C", "D"].map(str::to_owned);
            assert!(matches!(store.translate_units(&mut provider, &document.id, "fr", "uk", &input),
                Err(TranslationBatchError::Deferred { .. })));
            let resumed = store.translate_units(&mut provider, &document.id, "fr", "uk", &input).unwrap();
            assert_eq!(provider.target_calls[1], if pivot { ["B", "D"] } else { ["C", "D"] }.map(str::to_owned));
            for (segment, translated) in input.iter().zip(resumed.segments) {
                assert_eq!(translated.unwrap().text, format!("Переклад {segment}"));
            }
        }
    }

    #[test]
    fn contextual_cache_remains_available_after_fallback_packages_are_removed() {
        struct Provider {
            installed: Vec<ModelEdge>,
            calls: usize,
        }
        impl TranslationProvider for Provider {
            fn installed_models(&self) -> Result<Vec<ModelEdge>> {
                Ok(self.installed.clone())
            }

            fn translate(
                &mut self, _: &ModelEdge, _: &[String], _: Option<&str>, _: TranslationSettings,
            ) -> Result<Vec<String>> {
                self.calls += 1;
                Err("test inference unavailable".into())
            }
        }

        let model = |source: &str, target: &str| ModelEdge {
            source: source.into(),
            target: target.into(),
            revision: "test".into(),
            quality: None,
        };
        let catalog = [model("fr", "uk"), model("fr", "en"), model("en", "uk")];
        let fallback = Route::Pivot { first: catalog[1].clone(), second: catalog[2].clone() };
        let mut provider = Provider { installed: catalog.to_vec(), calls: 0 };
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let document = store.import("reader.txt", b"Il prend la page.", "fr").unwrap();
        let input = ["la page".to_owned()];
        let request = |context| TranslationRequest {
            source: "fr",
            target: "uk",
            segments: &input,
            context: Some(context),
            settings: TranslationSettings::default(),
        };
        let context = "Il prend la page.";
        let key = cache_key(&fallback, &input, Some(context), TranslationSettings::default()).unwrap();
        store.save_cached_segments([(key.clone(), &["сторінка".into()][..])]).unwrap();
        for installed in [catalog.len(), 1, 0] {
            provider.installed.truncate(installed);
            let result = store.translate_aligned_with_catalog(
                &mut provider, request(context), &catalog, Some(&document.id),
            ).unwrap().unwrap();
            assert_eq!(result.route, fallback);
            assert_eq!(result.segments, ["сторінка"]);
        }
        assert_eq!(provider.calls, 0);
        let owner: String = store.connection.query_row(
            "SELECT document_id FROM translation_cache_owners WHERE cache_key=?1", [&key], |row| row.get(0),
        ).unwrap();
        assert_eq!(owner, document.id);
        assert!(store.translate_aligned_with_catalog(
            &mut provider, request("Elle tourne la page."), &catalog, Some(&document.id),
        ).unwrap().is_none());
    }

    #[test]
    fn offline_passage_comparison_reuses_the_full_source_and_cached_translation() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("library.sqlite")).unwrap();
        let source = "Le bateau arrive. Le marin part.";
        let document = store.import("reader.txt", source.as_bytes(), "fr").unwrap();
        let sentence = &document.sentences[1].id;
        let route = french_uk_route_with_invalid_assets(&store);
        let input = [source.to_owned()];
        let translated = ["Корабель прибуває. Моряк вирушає.".to_owned()];
        let key = cache_key(&route, &input, None, TranslationSettings::default()).unwrap();
        store.save_cached_segments([(key, translated.as_slice())]).unwrap();
        let comparison = store.translate_offline_passage(&document.id, None, sentence, "uk").unwrap().unwrap();
        assert_eq!(comparison.source_text, source);
        assert_eq!(comparison.text, translated[0]);
        assert!(comparison.pivot_language.is_none());
        assert!(store.translate_offline_passage(&document.id, None, sentence, "fr").is_err());
    }

    #[test]
    fn pdf_page_translation_reports_the_pivot_of_the_cached_row() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("library.sqlite")).unwrap();
        let document = store.import_scanned_pdf(
            "scan.pdf", b"%PDF-1.4\n", "fr",
            vec![crate::ocr::OcrLine {
                block_start: false,
                page: 0, text: "Le bateau arrive. Le marin part.".into(),
                x: 0.1, y: 0.8, width: 0.8, height: 0.1, confidence: Some(1.0),
                correction: None, words: Vec::new(), source_span: None,
            }], 1,
        ).unwrap();
        let direct = french_uk_route_with_invalid_assets(&store);
        let catalog = store.translation_packages();
        let edge = |source: &str, target: &str| {
            let package = catalog.iter().find(|package| {
                (package.source, package.target) == (source, target)
            }).unwrap();
            ModelEdge {
                source: source.into(), target: target.into(),
                revision: package.revision.into(), quality: None,
            }
        };
        let pivot = Route::Pivot { first: edge("fr", "en"), second: edge("en", "uk") };
        let key = |route: &Route, index: usize| unit_cache_key(route, &document.sentences[index].text).unwrap();
        store.save_cached_segments([
            (key(&direct, 0), &["Корабель прибуває.".into()][..]),
            (key(&pivot, 1), &["Моряк вирушає.".into()][..]),
        ]).unwrap();
        assert!(store.translation_readiness("fr", "uk").unwrap().ready);
        let rows = serde_json::to_value(store.pdf_page_translation(&document.id, 0, "uk", true).unwrap()).unwrap();
        assert_eq!(rows[0]["text"], "Корабель прибуває.");
        assert_eq!(rows[0]["sourceSentenceId"], document.sentences[0].id);
        assert!(rows[0].get("pivotLanguage").is_none());
        assert_eq!(rows[1]["text"], "Моряк вирушає.");
        assert_eq!(rows[1]["sourceSentenceId"], document.sentences[1].id);
        assert_eq!(rows[1]["pivotLanguage"], "en");
    }

    #[test]
    fn rendered_pdf_prefers_aligned_cache_over_an_overlapping_legacy_group() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("library.sqlite")).unwrap();
        let document = store.import_scanned_pdf(
            "scan.pdf", b"%PDF-1.4\n", "fr",
            vec![crate::ocr::OcrLine {
                block_start: false,
                page: 0, text: "Le bateau arrive. Le marin part.".into(),
                x: 0.1, y: 0.1, width: 0.8, height: 0.1, confidence: None,
                correction: None, words: Vec::new(), source_span: None,
            }], 1,
        ).unwrap();
        let route = french_uk_route_with_invalid_assets(&store);
        let legacy = paragraph_plan(&store, &document, 0, "uk", PageBoundaries::Canonical, UnitGrouping::LegacyContext)
            .unwrap();
        assert_eq!(legacy.result.units[0].sentence_ids.len(), 2);
        let legacy_key = unit_cache_key(&route, &legacy.pending[0].1).unwrap();
        store.save_cached_segments([(legacy_key, &["Старий груповий переклад.".into()][..])]).unwrap();
        let grouped = store.pdf_page_translation(&document.id, 0, "uk", true).unwrap();
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].text.as_deref(), Some("Старий груповий переклад."));

        let sentence = &document.sentences[1];
        let sentence_key = unit_cache_key(&route, &sentence.text).unwrap();
        store.save_cached_segments([(sentence_key, &["Моряк вирушає.".into()][..])]).unwrap();
        assert!(store.remove_translation_package("fr", "uk").unwrap());
        assert!(!store.translation_readiness("fr", "uk").unwrap().ready);
        let rows = serde_json::to_value(store.pdf_page_translation(&document.id, 0, "uk", true).unwrap()).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 2);
        assert!(rows[0]["text"].is_null());
        assert_eq!(rows[1]["text"], "Моряк вирушає.");
        assert_eq!(rows[1]["sourceText"], sentence.text);
        assert_eq!(rows[1]["startUtf16"], sentence.start_utf16);
    }

    #[test]
    fn removed_pdf_review_restores_machine_cache_and_rewinds_background_translation() {
        use crate::pdf_detached_pages::DetachedPdfAnnotationChange;
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("library.sqlite")).unwrap();
        let input = root.path().join("book.pdf");
        std::fs::write(&input, b"%PDF-1.4\n").unwrap();
        let prepared = crate::file_originals::prepare_pdf_import(
            store.pdf_original_root(), "book.pdf", &input, "fr",
            vec!["Le bateau arrive.".into()], vec![],
        ).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        let sentence = &document.sentences[0];
        let route = french_uk_route_with_invalid_assets(&store);
        store.save_cached_segments([(
            unit_cache_key(&route, &sentence.text).unwrap(), &["Корабель прибуває.".into()][..],
        )]).unwrap();
        let revision = document.current_revision_id.as_deref().unwrap();
        let complete_background_pass = |store: &Store| {
            store.connection.execute(
                "INSERT OR REPLACE INTO background_translation_progress
                 (document_id,target,revision_id,cursor,incomplete,resume_at)
                 VALUES (?1,'uk',?2,1,0,NULL)",
                params![document.id, revision],
            ).unwrap();
        };
        let check_restored = |store: &mut Store| {
            let rows = store.pdf_page_translation(&document.id, 0, "uk", true).unwrap();
            assert_eq!(rows[0].text.as_deref(), Some("Корабель прибуває."));
            assert_eq!(store.connection.query_row(
                "SELECT cursor,resume_at,ocr_generation FROM background_translation_progress
                 WHERE document_id=?1 AND target='uk'",
                [&document.id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
            ).unwrap(), (0, 0, 1));
        };
        let checked = "Судно прибуло.";
        store.set_reviewed_translation(&document.id, &sentence.id, "uk", checked).unwrap();
        assert_eq!(store.pdf_page_translation(&document.id, 0, "uk", true).unwrap()[0].text.as_deref(), Some(checked));
        complete_background_pass(&store);
        store.set_reviewed_translation(&document.id, &sentence.id, "uk", "").unwrap();
        check_restored(&mut store);

        crate::pdf_detached_pages::write_text_layer(&store.connection, &document.id, 0, &document.text).unwrap();
        let detached = store.detached_pdf_sentences(&document.id, 0).unwrap().unwrap();
        let sentence = &detached.sentences[0];
        let edit = |text: &str| DetachedPdfAnnotationChange::Translation { target: "uk".into(), text: text.into() };
        store.update_detached_pdf_annotation(&document.id, 0, &sentence.id, edit(checked)).unwrap();
        assert_eq!(store.pdf_page_translation(&document.id, 0, "uk", true).unwrap()[0].text.as_deref(), Some(checked));
        complete_background_pass(&store);
        store.update_detached_pdf_annotation(&document.id, 0, &sentence.id, edit("")).unwrap();
        check_restored(&mut store);
    }

    #[test]
    fn rendered_pdf_keeps_cached_rows_when_page_inference_fails() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("library.sqlite")).unwrap();
        let document = store.import_scanned_pdf(
            "scan.pdf", b"%PDF-1.4\n", "fr",
            vec![crate::ocr::OcrLine {
                block_start: false,
                page: 0, text: "Le bateau arrive. Le marin part.".into(),
                x: 0.1, y: 0.8, width: 0.8, height: 0.1, confidence: Some(1.0),
                correction: None, words: Vec::new(), source_span: None,
            }], 1,
        ).unwrap();
        assert_eq!(document.sentences.len(), 2);
        let route = french_uk_route_with_invalid_assets(&store);
        assert!(store.translation_readiness("fr", "uk").unwrap().ready);
        assert!(store.pdf_page_translation(&document.id, 0, "uk", false).is_err());

        let first = &document.sentences[0];
        store.save_cached_segments([(
            unit_cache_key(&route, &first.text).unwrap(), &["Корабель прибуває.".into()][..],
        )]).unwrap();
        let rows = serde_json::to_value(store.pdf_page_translation(&document.id, 0, "uk", false).unwrap()).unwrap();
        assert_eq!(rows[0]["text"], "Корабель прибуває.");
        assert_eq!(rows[0]["sourceText"], first.text);
        assert_eq!(rows[0]["startUtf16"], first.start_utf16);
        assert_eq!(rows[0]["endUtf16"], first.end_utf16);
        assert!(rows[1]["text"].is_null());
        assert!(rows[1].get("sourceText").is_none());
        let background = store.pdf_page_translation_with_priority(&document.id, 0, "uk", false, true).unwrap();
        assert_eq!(background[0].text.as_deref(), Some("Корабель прибуває."));
        assert!(background[1].text.is_none());
    }

    #[test]
    fn background_translation_retries_interrupted_text_and_pdf_at_the_same_cursor() {
        use crate::{ocr::OcrLine, reader_preferences::ReaderPreferenceChange, resources::RESOURCES};
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("library.sqlite")).unwrap();
        let source = "Le bateau arrive. Le marin part.";
        let text = store.import("page.txt", source.as_bytes(), "fr").unwrap();
        let pdf = store.import_scanned_pdf("page.pdf", b"%PDF-1.4\n", "fr", vec![OcrLine {
            block_start: false,
            page: 0, text: source.into(), x: 0.1, y: 0.1, width: 0.5, height: 0.05,
            confidence: None, correction: None, words: Vec::new(), source_span: None,
        }], 1).unwrap();
        let route = french_uk_route_with_invalid_assets(&store);
        store.save_cached_segments([(unit_cache_key(&route, "Le bateau arrive.").unwrap(),
            &["Корабель прибуває.".into()][..])]).unwrap();
        store.update_reader_preference(ReaderPreferenceChange::BackgroundTranslation { value: true }).unwrap();
        let before = crate::now();
        let _foreground = RESOURCES.interactive();
        for document in [&text, &pdf] {
            let revision = document.current_revision_id.as_deref().unwrap();
            let started = std::cell::Cell::new(false);
            let result = store.advance_background_translation_until_canceled(
                &document.id, revision, "uk", &|| started.replace(true),
            );
            assert!(result.is_err());
            let interrupted: (u32, i64) = store.connection.query_row(
                "SELECT cursor,resume_at FROM background_translation_progress WHERE document_id=?1",
                [&document.id], |row| Ok((row.get(0)?, row.get(1)?)),
            ).unwrap();
            assert_eq!(interrupted, (0, 0));
            let progress = store.advance_background_translation(&document.id, revision, "uk").unwrap();
            let progress = serde_json::to_value(progress).unwrap();
            if document.format == "pdf" {
                assert_eq!(progress["translatedPage"], 0);
            } else {
                assert_eq!(progress["translatedSentenceIds"], serde_json::json!([document.sentences[0].id]));
            }
            let (cursor, incomplete, resume_at): (u32, bool, i64) = store.connection.query_row(
                "SELECT cursor,incomplete,resume_at FROM background_translation_progress WHERE document_id=?1",
                [&document.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).unwrap();
            assert_eq!((cursor, incomplete), (0, false));
            assert!((before + 2..=crate::now() + 2).contains(&resume_at));
        }
        let visible = store.pdf_page_translation_with_priority(&pdf.id, 0, "uk", false, true).unwrap();
        assert_eq!(visible[0].text.as_deref(), Some("Корабель прибуває."));
        assert!(visible[1].text.is_none());
    }

    #[test]
    fn background_pdf_waits_for_ocr_then_translates_the_recognized_page() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("library.sqlite");
        let mut store = Store::open(&path).unwrap();
        let scan = root.path().join("scan.pdf");
        std::fs::write(&scan, b"%PDF-1.4\n").unwrap();
        let prepared = crate::file_originals::prepare_pdf_import(
            store.pdf_original_root(), "scan.pdf", &scan, "fr", vec![String::new(); 2], vec![],
        ).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        let revision = document.current_revision_id.as_deref().unwrap();
        store.update_reader_preference(
            crate::reader_preferences::ReaderPreferenceChange::BackgroundTranslation { value: true },
        ).unwrap();
        let route = french_uk_route_with_invalid_assets(&store);
        let progress = |store: &mut Store, revision: &str| {
            serde_json::to_value(store.advance_background_translation(&document.id, revision, "uk").unwrap()).unwrap()
        };
        let waiting = progress(&mut store, revision);
        assert_eq!(waiting["waitingForPdfOcr"], true);
        assert!(waiting["resumeAt"].is_null());
        assert!(waiting["pendingPdfTextPage"].is_null());
        assert!(store.claim_pdf_ocr_page(&document.id).unwrap().is_none());
        drop(store);

        let mut store = Store::open(&path).unwrap();
        assert_eq!(progress(&mut store, revision), waiting);
        store.cache_pdf_ocr_page(&document.id, 0, vec![]).unwrap();
        assert_eq!(progress(&mut store, revision), waiting);
        let text = "Le bateau arrive.";
        let updated = store.cache_pdf_ocr_page(&document.id, 1, vec![crate::ocr::OcrLine {
            block_start: false,
            page: 1, text: text.into(), x: 0.1, y: 0.8, width: 0.8, height: 0.1,
            confidence: Some(1.0), correction: None, words: vec![], source_span: None,
        }]).unwrap();
        let revision = updated.current_revision_id.as_deref().unwrap();
        store.save_cached_segments([(
            unit_cache_key(&route, text).unwrap(), &["Корабель прибуває.".into()][..],
        )]).unwrap();
        assert_eq!(progress(&mut store, revision)["translatedPage"], 1);
        let complete = progress(&mut store, revision);
        assert!(complete["waitingForPdfOcr"].is_null());
        assert!(complete["resumeAt"].is_null());
    }

    #[test]
    fn background_text_progress_identifies_machine_translated_sentences() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("library.sqlite")).unwrap();
        let document = store.import("reader.txt", b"Le bateau arrive. Le marin part.", "fr").unwrap();
        let [reviewed, machine] = document.sentences.as_slice() else { panic!("expected two sentences"); };
        store.set_reviewed_translation(&document.id, &reviewed.id, "uk", "Корабель прибуває.").unwrap();
        store.update_reader_preference(crate::reader_preferences::ReaderPreferenceChange::BackgroundTranslation { value: true }).unwrap();
        let route = french_uk_route_with_invalid_assets(&store);
        store.save_cached_segments([(
            unit_cache_key(&route, &machine.text).unwrap(), &["Моряк вирушає.".into()][..],
        )]).unwrap();

        let revision = document.current_revision_id.as_deref().unwrap();
        let warm = store.advance_background_translation(&document.id, revision, "uk").unwrap();
        let progress = serde_json::to_value(warm).unwrap();
        assert_eq!(progress["translatedSentenceIds"], serde_json::json!([machine.id]));
        assert!(progress.get("translatedPage").is_none());

        let done = store.advance_background_translation(&document.id, revision, "uk").unwrap();
        let progress = serde_json::to_value(done).unwrap();
        assert!(progress.get("translatedSentenceIds").is_none());
        assert!(progress["resumeAt"].is_null());
    }

    #[test]
    fn readiness_prefers_a_working_pivot_and_identifies_only_missing_packages() {
        let edge = |source: &str, target: &str| ModelEdge {
            source: source.into(),
            target: target.into(),
            revision: format!("{source}-{target}"),
            quality: None,
        };
        let direct = edge("fr", "uk");
        let first = edge("fr", "en");
        let second = edge("en", "uk");
        let catalog = [direct.clone(), first.clone(), second.clone()];
        let installed = [first.clone(), second];
        let via = route_readiness(&catalog, &installed, "fr", "uk").unwrap();
        assert!(via.ready && via.missing.is_empty());
        assert_eq!(via.pivot_language.as_deref(), Some("en"));
        assert!(
            route_readiness(&catalog, std::slice::from_ref(&direct), "fr", "uk")
                .unwrap()
                .pivot_language
                .is_none()
        );

        let missing_direct =
            route_readiness(&catalog, std::slice::from_ref(&first), "fr", "uk").unwrap();
        assert_eq!(
            missing_direct.missing,
            [TranslationPair {
                source: "fr".into(),
                target: "uk".into()
            }]
        );
        let needs_second =
            route_readiness(&catalog[1..], std::slice::from_ref(&first), "fr", "uk").unwrap();
        assert!(!needs_second.ready);
        assert_eq!(
            needs_second.missing,
            [TranslationPair {
                source: "en".into(),
                target: "uk".into()
            }]
        );
        let unsupported = route_readiness(&catalog, &installed, "uk", "fr").unwrap();
        assert!(
            !unsupported.ready
                && unsupported.missing.is_empty()
                && unsupported.pivot_language.is_none()
        );
    }

    #[test]
    fn reader_restores_partial_paragraph_and_sentence_cache_after_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("library.sqlite");
        let mut store = Store::open(&path).unwrap();
        let document = store
            .import(
                "story.txt",
                b"First sentence. Second sentence. Third sentence. Fourth sentence.",
                "en",
            )
            .unwrap();
        let mut edge = ModelEdge {
            source: "en".into(),
            target: "fr".into(),
            revision: "test-v1".into(),
            quality: None,
        };
        let route = Route::Direct(edge.clone());
        let first_leg = ModelEdge {
            target: "de".into(),
            ..edge.clone()
        };
        let second_leg = ModelEdge {
            source: "de".into(),
            ..edge.clone()
        };
        let pivot = Route::Pivot {
            first: first_leg.clone(),
            second: second_leg.clone(),
        };
        let selected = 1;
        let first = &document.sentences[selected].text;
        let fourth = &document.sentences[3].text;
        assert_eq!(
            unit_cache_key(&route, first).unwrap(),
            cache_key(
                &route,
                std::slice::from_ref(first),
                None,
                TranslationSettings::default(),
            )
            .unwrap()
        );
        store
            .save_cached_segments([
                (
                    unit_cache_key(&route, first).unwrap(),
                    &["Deux.".into()][..],
                ),
                (
                    unit_cache_key(&route, fourth).unwrap(),
                    &["Quatre.".into()][..],
                ),
                (
                    unit_cache_key(&pivot, fourth).unwrap(),
                    &["Quatrième.".into()][..],
                ),
            ])
            .unwrap();
        drop(store);

        let mut store = Store::open(&path).unwrap();
        let cached = cached_translation(
            &store,
            std::slice::from_ref(&edge),
            &[],
            &document,
            selected,
            "fr",
        )
        .unwrap();
        assert_eq!(cached.paragraph.units.len(), 4);
        assert!(cached.paragraph.units[0].translation.is_none());
        assert_eq!(
            cached.paragraph.units[3].translation.as_deref(),
            Some("Quatre.")
        );
        assert_eq!(cached.paragraph.units[selected].translation.as_deref(), Some("Deux."));

        let installed = [first_leg, second_leg];
        let catalog = [edge.clone(), installed[0].clone(), installed[1].clone()];
        for route in [&installed[..], &[]] {
            let recovered =
                cached_translation(&store, route, &catalog, &document, selected, "fr").unwrap();
            assert_eq!(recovered.paragraph.pivot_language, None);
            assert_eq!(
                recovered.paragraph.units[3].translation.as_deref(),
                Some("Quatre.")
            );
            assert_eq!(recovered.paragraph.units[selected].translation.as_deref(), Some("Deux."));
        }

        store
            .save_cached_segments([(
                unit_cache_key(&pivot, first).unwrap(),
                &["Seconde.".into()][..],
            )])
            .unwrap();
        let preferred =
            cached_translation(&store, &installed, &catalog, &document, selected, "fr").unwrap();
        assert_eq!(preferred.paragraph.pivot_language.as_deref(), Some("de"));
        assert_eq!(
            preferred.paragraph.units[3].translation.as_deref(),
            Some("Quatrième.")
        );
        assert_eq!(preferred.paragraph.units[selected].translation.as_deref(), Some("Seconde."));

        edge.revision = "test-v2".into();
        let stale = cached_translation(&store, &[], &[edge], &document, selected, "fr").unwrap();
        assert!(stale
            .paragraph
            .units
            .iter()
            .all(|unit| unit.translation.is_none()));
        assert!(stale.sentence.is_none());
    }

    #[test]
    fn translation_search_uses_cached_routes_and_reviewed_text_in_position_order() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let mut document = store
            .import(
                "search.txt",
                b"First sentence. Second sentence. Third sentence. Fourth sentence.",
                "en",
            )
            .unwrap();
        let edge = ModelEdge {
            source: "en".into(),
            target: "fr".into(),
            revision: "test-v1".into(),
            quality: None,
        };
        let route = Route::Direct(edge.clone());
        let group = "First sentence. Second sentence. Third sentence.";
        let standalone = &document.sentences[3].text;
        store
            .save_cached_segments([
                (
                    unit_cache_key(&route, group).unwrap(),
                    &["L’e\u{301}te\u{301} arrive.".into()][..],
                ),
                (
                    unit_cache_key(&route, standalone).unwrap(),
                    &["L’été finit.".into()][..],
                ),
            ])
            .unwrap();
        let restored = cached_translation(&store, &[], std::slice::from_ref(&edge), &document, 0, "fr").unwrap();
        assert_eq!(restored.paragraph.units[0].sentence_ids.len(), 3);
        assert_eq!(restored.paragraph.units[0].translation.as_deref(), Some("L’e\u{301}te\u{301} arrive."));
        let installed = cached_translation(&store, std::slice::from_ref(&edge), &[], &document, 0, "fr").unwrap();
        assert_eq!(installed.paragraph.units[0].sentence_ids.len(), 1);
        assert!(installed.paragraph.units[0].translation.is_none());

        let search = |document: &crate::Document, target, query, start| {
            search_cached_translation(
                &store,
                std::slice::from_ref(&edge),
                &[],
                document,
                target,
                query,
                start,
            )
            .unwrap()
            .matches
            .into_iter()
            .map(|hit| hit.position)
            .collect::<Vec<_>>()
        };
        assert_eq!(search(&document, "fr", "L'ÉTÉ", 0), [0, 3]);
        assert_eq!(search(&document, "fr", "L'ÉTÉ", 1), [3]);
        assert!(search(&document, "uk", "L'ÉTÉ", 0).is_empty());

        document
            .reviewed_translations
            .entry("fr".into())
            .or_default()
            .insert(
                document.sentences[3].id.clone(),
                "Corrections confirmées.".into(),
            );
        assert_eq!(search(&document, "fr", "CONFIRMÉES", 0), [3]);
        assert_eq!(search(&document, "fr", "L'ÉTÉ", 0), [0]);
    }

    #[test]
    fn pdf_reflow_joins_only_continuing_lines_and_keeps_stored_fragments() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let text = "Заголовок\n\n😀 Млинці готують із \r\nрідкого тіста. Їх подають\nіз медом.\nВам знадобиться:\n• борошно\n• молоко\n\nокремий рядок.";
        let mut document = store.import("story.txt", text.as_bytes(), "uk").unwrap();
        let index = |doc: &crate::Document, fragment: &str| {
            doc.sentences
                .iter()
                .position(|s| s.text == fragment)
                .unwrap()
        };
        let wrapped = index(&document, "😀 Млинці готують із");
        assert_eq!(
            input_for_sentence(&document, wrapped),
            ("😀 Млинці готують із".into(), false)
        );
        document.format = "pdf".into();
        for fragment in ["😀 Млинці готують із", "рідкого тіста."] {
            assert_eq!(
                input_for_sentence(&document, index(&document, fragment)),
                ("😀 Млинці готують із рідкого тіста.".into(), true)
            );
        }
        assert_eq!(
            input_for_sentence(&document, index(&document, "Їх подають")),
            ("Їх подають із медом.".into(), true)
        );
        for fragment in [
            "Заголовок",
            "Вам знадобиться:",
            "• борошно",
            "• молоко",
            "окремий рядок.",
        ] {
            assert_eq!(
                input_for_sentence(&document, index(&document, fragment)),
                (fragment.into(), false)
            );
        }
        assert_eq!(document.text, text);
    }
}

/// Settings are part of the cache identity. Providers must honor them.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct TranslationSettings {
    pub beam_size: u8,
}

impl Default for TranslationSettings {
    fn default() -> Self {
        Self { beam_size: 4 }
    }
}

pub struct TranslationRequest<'a> {
    pub source: &'a str,
    pub target: &'a str,
    pub segments: &'a [String],
    pub context: Option<&'a str>,
    pub settings: TranslationSettings,
}

pub(crate) struct UnitsTranslation {
    pub(crate) segments: Vec<Option<UnitTranslation>>,
    preferred_pivot: Option<String>,
}

#[derive(Clone)]
pub(crate) struct UnitTranslation {
    pub(crate) text: String,
    pivot_language: Option<String>,
}

impl UnitTranslation {
    fn from_route(text: String, route: &Route) -> Self {
        Self { text, pivot_language: route.pivot_language().map(str::to_owned) }
    }
}

impl Store {
    pub(crate) fn record_translation_cache_use(
        &self, document: &str, route: &Route, input: &[String],
    ) -> Result<()> {
        let keys = input.iter().map(|text| unit_cache_key(route, text)).collect::<Result<Vec<_>>>()?;
        self.link_translation_cache_keys(document, keys.iter().map(String::as_str))
    }

    fn link_translation_cache_keys<'a>(&self, document: &str, keys: impl Iterator<Item = &'a str>) -> Result<()> {
        let tx = self.connection.unchecked_transaction().map_err(|error| error.to_string())?;
        {
            let mut link = tx.prepare_cached(
                "INSERT OR IGNORE INTO translation_cache_owners(cache_key,document_id)
                 SELECT cache.key, docs.id FROM translation_cache cache, documents docs
                 WHERE cache.key=?1 AND docs.id=?2",
            ).map_err(|error| error.to_string())?;
            for key in keys {
                link.execute(params![key, document]).map_err(|error| error.to_string())?;
            }
        }
        tx.commit().map_err(|error| error.to_string())
    }

    pub fn translate_aligned(
        &mut self,
        provider: &mut impl TranslationProvider,
        request: TranslationRequest<'_>,
    ) -> Result<Option<AlignedTranslation>> {
        self.translate_aligned_with_catalog(provider, request, &[], None)
    }

    pub(crate) fn translate_aligned_with_catalog(
        &mut self,
        provider: &mut impl TranslationProvider,
        request: TranslationRequest<'_>,
        catalog: &[ModelEdge],
        document: Option<&str>,
    ) -> Result<Option<AlignedTranslation>> {
        let TranslationRequest {
            source,
            target,
            segments,
            context,
            settings,
        } = request;
        if !valid_segments(segments) || context.is_some_and(|text| text.len() > MAX_BATCH_BYTES) {
            return Err("Некоректний обсяг тексту для перекладу".into());
        }
        if !(1..=8).contains(&settings.beam_size) {
            return Err("Некоректні параметри перекладу".into());
        }
        let context = context.filter(|text| !text.trim().is_empty());
        let routes = CacheRoutes::resolve(&provider.installed_models()?, catalog, source, target)?;
        if let Some((translation, key)) = routes.cached_aligned(self, segments, context, target, settings)? {
            if let Some(document) = document {
                self.link_translation_cache_keys(document, std::iter::once(key.as_str()))?;
            }
            return Ok(Some(translation));
        }

        let attempts = if provider.allow_alternative_routes() { routes.installed_count } else { 1.min(routes.installed_count) };
        let mut first_failure = None;
        for route in routes.candidates.into_iter().take(attempts) {
            match infer_route(provider, &route, segments, context, settings) {
                Ok(output) => {
                    let key = cache_key(&route, segments, context, settings)?;
                    self.save_cached_segments([(key.clone(), output.as_slice())])?;
                    if let Some(document) = document {
                        self.link_translation_cache_keys(document, std::iter::once(key.as_str()))?;
                    }
                    return Ok(Some(AlignedTranslation::from_route(route, output)));
                }
                Err(error) => { first_failure.get_or_insert(error); }
            }
        }
        first_failure.map_or(Ok(None), Err)
    }

    pub(crate) fn translate_units(
        &mut self,
        provider: &mut impl TranslationProvider,
        document: &str,
        source: &str,
        target: &str,
        input: &[String],
    ) -> std::result::Result<UnitsTranslation, TranslationBatchError> {
        let routes = ranked_routes(&provider.installed_models()?, source, target)?;
        let mut segments = vec![None; input.len()];
        let preferred_pivot = routes.first().and_then(Route::pivot_language).map(str::to_owned);
        let mut first_failure = None;
        // A cached fallback remains reusable even if the preferred package is present.
        for route in &routes {
            let mut used = Vec::new();
            for (text, output) in input.iter().zip(&mut segments).filter(|(_, output)| output.is_none()) {
                let key = unit_cache_key(route, text)?;
                if let Some(mut value) = self.cached_segments(&key, 1, target)? {
                    *output = value.pop().map(|text| UnitTranslation::from_route(text, route));
                    used.push(key);
                }
            }
            if !used.is_empty() {
                self.link_translation_cache_keys(document, used.iter().map(String::as_str))?;
            }
        }
        for route in &routes {
            let pending: Vec<_> = segments.iter().enumerate()
                .filter_map(|(index, value)| value.is_none().then_some(index)).collect();
            if pending.is_empty() {
                break;
            }
            let batch: Vec<_> = pending.iter().map(|&index| input[index].clone()).collect();
            let (outcomes, deferred) = match infer_independent_units(provider, route, &batch) {
                Ok(outcomes) => (outcomes, None),
                Err(TranslationBatchError::Deferred { message, completed }) => (completed, Some(message)),
                Err(TranslationBatchError::Failed(error)) => {
                    first_failure.get_or_insert(error);
                    continue;
                }
            };
            let mut rows = Vec::new();
            for (index, outcome) in pending.into_iter().zip(outcomes) {
                match outcome {
                    Ok(text) => {
                        rows.push((unit_cache_key(route, &input[index])?, index));
                        segments[index] = Some(UnitTranslation::from_route(text, route));
                    }
                    Err(error) => { first_failure.get_or_insert(error); }
                }
            }
            if !rows.is_empty() {
                self.save_cached_segments(rows.iter().map(|(key, index)| {
                    let output = segments[*index].as_ref().expect("completed translation");
                    (key.clone(), std::slice::from_ref(&output.text))
                }))?;
                self.link_translation_cache_keys(document, rows.iter().map(|(key, _)| key.as_str()))?;
            }
            if let Some(message) = deferred {
                return Err(TranslationBatchError::deferred(message));
            }
        }
        if segments.iter().all(Option::is_none) {
            if let Some(error) = first_failure {
                return Err(error.into());
            }
        }
        Ok(UnitsTranslation { segments, preferred_pivot })
    }

    fn cached_segments(
        &self,
        key: &str,
        count: usize,
        target: &str,
    ) -> Result<Option<Vec<String>>> {
        let cached: Option<String> = self
            .connection
            .query_row(
                "SELECT body FROM translation_cache WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        Ok(cached
            .as_deref()
            .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
            .filter(|segments| {
                segments.len() == count
                    && valid_segments(segments)
                    && (target != "uk"
                        || !segments
                            .iter()
                            .any(|text| crate::marian_target_uk::clearly_russian(text)))
            }))
    }

    fn save_cached_segments<'a>(
        &mut self,
        rows: impl IntoIterator<Item = (String, &'a [String])>,
    ) -> Result<()> {
        let tx = self
            .connection
            .transaction()
            .map_err(|error| error.to_string())?;
        {
            // Refresh eviction order without cascading a delete to document links.
            let mut statement = tx
                .prepare_cached(
                    "INSERT INTO translation_cache(key, body) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET body=excluded.body, rowid=excluded.rowid",
                )
                .map_err(|error| error.to_string())?;
            for (key, segments) in rows {
                let value = serde_json::to_string(segments).map_err(|error| error.to_string())?;
                statement
                    .execute(params![key, value])
                    .map_err(|error| error.to_string())?;
            }
        }
        tx.execute(
            "DELETE FROM translation_cache WHERE rowid IN (
                SELECT rowid FROM translation_cache ORDER BY rowid DESC LIMIT -1 OFFSET ?1
            )",
            [LIVE_CACHE_ROWS],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
    }
}

fn infer_route(
    provider: &mut impl TranslationProvider,
    route: &Route,
    segments: &[String],
    context: Option<&str>,
    settings: TranslationSettings,
) -> Result<Vec<String>> {
    let mut infer = |edge: &ModelEdge, input: &[String], context: Option<&str>| {
        checked_inference(provider, edge, input, context, settings)
    };
    match route {
        Route::Direct(edge) => infer(edge, segments, context),
        Route::Pivot { first, second } => {
            let translated = infer(first, segments, context)?;
            let pivot_context = match context {
                Some(text) => {
                    let input = [text.to_owned()];
                    Some(infer(first, &input, None)?.remove(0))
                }
                None => None,
            };
            infer(second, &translated, pivot_context.as_deref())
        }
    }
}

fn infer_independent_units(
    provider: &mut impl TranslationProvider,
    route: &Route,
    input: &[String],
) -> std::result::Result<Vec<Result<String>>, TranslationBatchError> {
    let mut infer = |edge: &ModelEdge, input: &[String]| {
        let (mut outcomes, deferred) = match provider.translate_independent(edge, input, TranslationSettings::default()) {
            Ok(outcomes) => (outcomes, None),
            Err(TranslationBatchError::Deferred { message, completed }) => (completed, Some(message)),
            Err(error) => return Err(error),
        };
        if outcomes.len() > input.len() || deferred.is_none() && outcomes.len() != input.len() {
            return Err(TranslationBatchError::Failed(
                "Мовний пакет повернув некоректне вирівнювання перекладу".into(),
            ));
        }
        for outcome in &mut outcomes {
            if outcome.as_ref().is_ok_and(|text| {
                text.trim().is_empty() || text.len() > MAX_BATCH_BYTES
                    || (edge.target == "uk" && crate::marian_target_uk::clearly_russian(text))
            }) {
                *outcome = Err("Мовний пакет повернув некоректний переклад речення".into());
            }
        }
        match deferred {
            Some(message) => Err(TranslationBatchError::Deferred { message, completed: outcomes }),
            None => Ok(outcomes),
        }
    };
    match route {
        Route::Direct(edge) => infer(edge, input),
        Route::Pivot { first, second } => {
            let mut outcomes = match infer(first, input) {
                // These are intermediate-language results, so they cannot be stored under the full route.
                Err(TranslationBatchError::Deferred { message, .. }) => return Err(TranslationBatchError::deferred(message)),
                result => result?,
            };
            let pivot_input: Vec<_> = outcomes.iter().filter_map(|outcome| outcome.as_ref().ok()).cloned().collect();
            if !pivot_input.is_empty() {
                let (translated, deferred) = match infer(second, &pivot_input) {
                    Ok(translated) => (translated, None),
                    Err(TranslationBatchError::Deferred { message, completed }) => (completed, Some(message)),
                    Err(error) => return Err(error),
                };
                let mut translated = translated.into_iter();
                let done = outcomes.iter_mut().map_while(|outcome| {
                    if outcome.is_ok() { *outcome = translated.next()?; }
                    Some(())
                }).count();
                if let Some(message) = deferred {
                    outcomes.truncate(done);
                    return Err(TranslationBatchError::Deferred { message, completed: outcomes });
                }
            }
            Ok(outcomes)
        }
    }
}

fn unit_cache_key(route: &Route, text: &str) -> Result<String> {
    cache_key(route, &[text], None, TranslationSettings::default())
}

fn cache_key(
    route: &Route,
    segments: &[impl Serialize + AsRef<str>],
    context: Option<&str>,
    settings: TranslationSettings,
) -> Result<String> {
    // Decoder/tokenizer and contextual reranking revisions must not reuse older outputs.
    let from_uk_to_fr = |edge: &ModelEdge| edge.source == "uk" && edge.target == "fr";
    let deadline_revision = || {
        segments.iter()
            .map(AsRef::as_ref)
            .filter(|text| text.len() <= 768)
            .filter_map(crate::marian_deadline::cache_revision)
            .max()
            .unwrap_or(4)
    };
    let version = match route {
        _ if settings.beam_size > 1 && segments.iter().any(|text|
            crate::marian_provider::may_recover_group(text.as_ref())) => 10,
        Route::Direct(edge) if edge.source == "fr" && edge.target == "uk" &&
            segments.iter().any(|text| crate::translation_dates::contains_french_date(text.as_ref())) => 9,
        Route::Direct(edge) if edge.source == "fr" && edge.target == "uk" &&
            segments.iter().any(|text| {
                crate::marian_target_uk::french_manner_of_walking(text.as_ref()).is_some()
            }) => 8,
        Route::Direct(edge) if from_uk_to_fr(edge) => deadline_revision(),
        Route::Pivot { second, .. } if from_uk_to_fr(second) => 7,
        Route::Pivot { first, .. } if from_uk_to_fr(first) => deadline_revision(),
        _ => 4,
    };
    let input = serde_json::to_vec(&(version, route.without_quality(), segments, context, settings))
        .map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(input)))
}

fn valid_segments(segments: &[String]) -> bool {
    !segments.is_empty()
        && segments.len() <= 32
        && segments.iter().all(|segment| !segment.trim().is_empty())
        && segments.iter().map(String::len).sum::<usize>() <= MAX_BATCH_BYTES
}

fn checked_inference(
    provider: &mut (impl TranslationProvider + ?Sized),
    model: &ModelEdge,
    source: &[String],
    context: Option<&str>,
    settings: TranslationSettings,
) -> Result<Vec<String>> {
    let result = provider.translate(model, source, context, settings)?;
    if result.len() != source.len() || !valid_segments(&result) {
        return Err("Мовний пакет повернув некоректне вирівнювання перекладу".into());
    }
    Ok(result)
}
