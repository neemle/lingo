//! Routes translations through models already installed on the device.

use crate::{valid_language, Result, Store};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

const MAX_BATCH_BYTES: usize = 64 * 1024;
const CACHE_ROWS: i64 = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelEdge {
    pub source: String,
    pub target: String,
    /// Identifier pinned to an installed model and its tokenizer/configuration.
    pub revision: String,
    /// Comparable measured quality on a 0–1000 scale. None means unevaluated.
    pub quality: Option<u16>,
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
}

pub fn resolve_route(installed: &[ModelEdge], source: &str, target: &str) -> Result<Option<Route>> {
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

    if let Some(edge) = installed
        .iter()
        .filter(|edge| edge.source == source && edge.target == target)
        .max_by(|left, right| {
            left.quality
                .cmp(&right.quality)
                .then_with(|| right.revision.cmp(&left.revision))
        })
    {
        return Ok(Some(Route::Direct(edge.clone())));
    }

    let best = installed
        .iter()
        .filter(|edge| edge.source == source)
        .flat_map(|first| {
            installed
                .iter()
                .filter(move |second| second.source == first.target && second.target == target)
                .map(move |second| (first, second))
        })
        .max_by(|(left_first, left_second), (right_first, right_second)| {
            // Prefer fully evaluated routes, then the stronger weakest edge.
            let rank = |first: &ModelEdge, second: &ModelEdge| {
                (
                    first.quality.zip(second.quality).map(|(a, b)| a.min(b)),
                    first.target == "en",
                )
            };
            rank(left_first, left_second)
                .cmp(&rank(right_first, right_second))
                .then_with(|| right_first.target.cmp(&left_first.target))
                .then_with(|| right_first.revision.cmp(&left_first.revision))
                .then_with(|| right_second.revision.cmp(&left_second.revision))
        });
    Ok(best.map(|(first, second)| Route::Pivot {
        first: first.clone(),
        second: second.clone(),
    }))
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
                .filter(|edge| !installed.contains(edge))
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

pub trait TranslationProvider {
    /// Inspect the local inventory without downloading or loading models for inference.
    fn installed_models(&self) -> Result<Vec<ModelEdge>>;

    /// Return one target string per source segment, preserving the segment order.
    /// Providers receive context in the input language of this model edge.
    fn translate(
        &mut self,
        model: &ModelEdge,
        segments: &[String],
        context: Option<&str>,
        settings: TranslationSettings,
    ) -> Result<Vec<String>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AlignedTranslation {
    pub route: Route,
    /// Each entry stays paired to its source. A target entry may contain several sentences.
    pub segments: Vec<String>,
    pub joined_lines: bool,
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
}

pub struct CachedTranslation {
    pub paragraph: ParagraphTranslation,
    pub sentence: Option<AlignedTranslation>,
}

struct ParagraphPlan {
    result: ParagraphTranslation,
    pending: Vec<(usize, String)>,
}

pub(crate) fn translate_paragraph(
    store: &mut Store,
    provider: &mut impl TranslationProvider,
    document: &crate::Document,
    selected: usize,
    target: &str,
) -> Result<ParagraphTranslation> {
    let ParagraphPlan {
        mut result,
        pending,
    } = paragraph_plan(document, selected, target)?;
    if !pending.is_empty() {
        let (indices, input): (Vec<_>, Vec<_>) = pending.into_iter().unzip();
        if let Some(aligned) =
            store.translate_units(provider, &document.language, target, &input)?
        {
            result.pivot_language = aligned.route.pivot_language().map(str::to_owned);
            for (index, text) in indices.into_iter().zip(aligned.segments) {
                result.units[index].translation = Some(text);
            }
        }
    }
    Ok(result)
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
    cached_translation_with_routes(
        store,
        &routes,
        document,
        selected,
        target,
        CacheScope::Window,
    )
}

struct CacheRoutes {
    preferred: Option<Route>,
    candidates: Vec<Route>,
}

impl CacheRoutes {
    fn resolve(
        installed: &[ModelEdge],
        catalog: &[ModelEdge],
        source: &str,
        target: &str,
    ) -> Result<Self> {
        let preferred = resolve_route(installed, source, target)?;
        let candidates = preferred
            .iter()
            .cloned()
            .chain(
                catalog_routes(catalog, source, target)
                    .filter(|route| Some(route) != preferred.as_ref()),
            )
            .collect();
        Ok(Self {
            preferred,
            candidates,
        })
    }
}

#[derive(Clone, Copy)]
enum CacheScope {
    Window,
    Active,
}

fn cached_translation_with_routes(
    store: &Store,
    routes: &CacheRoutes,
    document: &crate::Document,
    selected: usize,
    target: &str,
    scope: CacheScope,
) -> Result<CachedTranslation> {
    let ParagraphPlan {
        result: mut paragraph,
        pending,
    } = paragraph_plan(document, selected, target)?;
    paragraph.pivot_language = routes
        .preferred
        .as_ref()
        .and_then(Route::pivot_language)
        .map(str::to_owned);

    let active_id = &document.sentences[selected].id;
    let active_reviewed = paragraph
        .units
        .iter()
        .any(|unit| unit.reviewed && unit.sentence_ids.contains(active_id));
    let standalone = (!active_reviewed).then(|| input_for_sentence(document, selected));
    let mut best = None;
    for route in &routes.candidates {
        let mut units = Vec::new();
        let mut active_hit = false;
        for (index, text) in &pending {
            let active = paragraph.units[*index].sentence_ids.contains(active_id);
            if matches!(scope, CacheScope::Active) && !active {
                continue;
            }
            if let Some(mut segments) =
                store.cached_segments(&unit_cache_key(route, text)?, 1, target)?
            {
                active_hit |= active;
                units.push((*index, segments.remove(0)));
            }
        }

        let mut sentence = None;
        if !active_hit {
            if let Some((text, joined_lines)) = standalone
                .as_ref()
                .filter(|(text, _)| text.len() <= MAX_BATCH_BYTES)
            {
                if let Some(segments) =
                    store.cached_segments(&unit_cache_key(route, text)?, 1, target)?
                {
                    sentence = Some(AlignedTranslation {
                        route: route.clone(),
                        segments,
                        joined_lines: *joined_lines,
                    });
                }
            }
        }
        // Prefer the first route with the active result. If no route has it, retain the
        // first partial window. Never merge machine output from different routes.
        if active_hit || sentence.is_some() {
            best = Some((route, units, sentence));
            break;
        }
        if best.is_none() && !units.is_empty() {
            best = Some((route, units, sentence));
        }
    }

    let mut sentence = None;
    if let Some((route, units, active_sentence)) = best {
        paragraph.pivot_language = route.pivot_language().map(str::to_owned);
        for (index, translation) in units {
            paragraph.units[index].translation = Some(translation);
        }
        sentence = active_sentence;
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
            CacheScope::Active,
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
    document: &crate::Document,
    selected: usize,
    target: &str,
) -> Result<ParagraphPlan> {
    valid_language(target)?;
    if document.language == target {
        return Err("Мова перекладу має відрізнятися від мови джерела".into());
    }

    const CONTEXT_SENTENCES: usize = 4;
    const CONTEXT_GROUP: usize = 3;
    const CONTEXT_BYTES: usize = 768;
    let sentences = &document.sentences;
    let paragraph = document.paragraph_utf8_range(sentences[selected].start_utf8);
    let first = sentences
        .partition_point(|sentence| sentence.end_utf8 <= paragraph.start)
        .min(selected);
    let last = sentences
        .partition_point(|sentence| sentence.start_utf8 < paragraph.end)
        .max(selected + 1);
    let reviews = document.reviewed_translations.get(target);
    let reviewed = |index: usize| reviews.and_then(|entries| entries.get(&sentences[index].id));
    let can_wrap = |left: usize, right: usize| {
        (document.format == "pdf" || document.ocr_lines.is_some())
            && reviewed(left).is_none()
            && reviewed(right).is_none()
            && soft_wrap(document, left, right)
    };
    let mut start = first.max(selected.saturating_sub(CONTEXT_SENTENCES));
    let mut end = last.min(selected + CONTEXT_SENTENCES + 1);
    // Use paragraph-relative group boundaries so moving the selection does not
    // change the context of overlapping units.
    start -= (start - first) % CONTEXT_GROUP;
    end = last.min(end + (CONTEXT_GROUP - (end - first) % CONTEXT_GROUP) % CONTEXT_GROUP);
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
        let mut next = index + 1;
        while next < end
            && next - index < 12
            && reviewed(index).is_none()
            && reviewed(next).is_none()
        {
            let bytes = sentences[next]
                .end_utf8
                .saturating_sub(sentences[index].start_utf8);
            let continues_context = next - index < CONTEXT_GROUP
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
        if review.is_none() {
            let input = if next == index + 1 {
                sentences[index].text.clone()
            } else {
                document
                    .text
                    .get(sentences[index].start_utf8..sentences[next - 1].end_utf8)
                    .ok_or("Межі абзацу пошкоджені.")?
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            pending.push((units.len(), input));
        }
        units.push(ParagraphUnit {
            sentence_ids: sentences[index..next]
                .iter()
                .map(|sentence| sentence.id.clone())
                .collect(),
            reviewed: review.is_some(),
            translation: review,
        });
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

pub(crate) fn input_for_sentence(document: &crate::Document, selected: usize) -> (String, bool) {
    const MAX_JOINED_BYTES: usize = 1024;
    const MAX_FRAGMENTS: usize = 12;
    let active = &document.sentences[selected];
    if document.format != "pdf" && document.ocr_lines.is_none() {
        return (active.text.clone(), false);
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
    if start == end {
        return (active.text.clone(), false);
    }
    let span =
        &document.text[document.sentences[start].start_utf8..document.sentences[end].end_utf8];
    (span.split_whitespace().collect::<Vec<_>>().join(" "), true)
}

fn soft_wrap(document: &crate::Document, left: usize, right: usize) -> bool {
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
        assert_eq!(cached.paragraph.units.len(), 2);
        assert!(cached.paragraph.units[0].translation.is_none());
        assert_eq!(
            cached.paragraph.units[1].translation.as_deref(),
            Some("Quatre.")
        );
        assert_eq!(cached.sentence.unwrap().segments, ["Deux."]);

        let installed = [first_leg, second_leg];
        let catalog = [edge.clone(), installed[0].clone(), installed[1].clone()];
        for route in [&installed[..], &[]] {
            let recovered =
                cached_translation(&store, route, &catalog, &document, selected, "fr").unwrap();
            assert_eq!(recovered.paragraph.pivot_language, None);
            assert_eq!(
                recovered.paragraph.units[1].translation.as_deref(),
                Some("Quatre.")
            );
            assert_eq!(recovered.sentence.unwrap().segments, ["Deux."]);
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
            preferred.paragraph.units[1].translation.as_deref(),
            Some("Quatrième.")
        );
        assert_eq!(preferred.sentence.unwrap().segments, ["Seconde."]);

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
        let search = |document: &crate::Document, target, query, start| {
            search_cached_translation(
                &store,
                &[],
                std::slice::from_ref(&edge),
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

impl Store {
    pub fn translate_aligned(
        &mut self,
        provider: &mut impl TranslationProvider,
        request: TranslationRequest<'_>,
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
        // Resolve the installed route before looking at the cache: a removed model invalidates its results.
        let Some(route) = resolve_route(&provider.installed_models()?, source, target)? else {
            return Ok(None);
        };
        let key = cache_key(&route, segments, context, settings)?;
        if let Some(segments) = self.cached_segments(&key, segments.len(), target)? {
            return Ok(Some(AlignedTranslation {
                route,
                segments,
                joined_lines: false,
            }));
        }

        let result = match &route {
            Route::Direct(model) => {
                checked_inference(provider, model, segments, context, settings)?
            }
            Route::Pivot { first, second } => {
                let pivot = checked_inference(provider, first, segments, context, settings)?;
                let pivot_context = match context {
                    Some(text) => Some(
                        checked_inference(provider, first, &[text.to_owned()], None, settings)?
                            .remove(0),
                    ),
                    None => None,
                };
                checked_inference(provider, second, &pivot, pivot_context.as_deref(), settings)?
            }
        };
        self.save_cached_segments([(key, result.as_slice())])?;
        Ok(Some(AlignedTranslation {
            route,
            segments: result,
            joined_lines: false,
        }))
    }

    fn translate_units(
        &mut self,
        provider: &mut impl TranslationProvider,
        source: &str,
        target: &str,
        input: &[String],
    ) -> Result<Option<AlignedTranslation>> {
        let Some(route) = resolve_route(&provider.installed_models()?, source, target)? else {
            return Ok(None);
        };
        let settings = TranslationSettings::default();
        let mut uncached = Vec::new();
        let mut output = vec![String::new(); input.len()];
        for (index, text) in input.iter().enumerate() {
            let key = unit_cache_key(&route, text)?;
            if let Some(mut value) = self.cached_segments(&key, 1, target)? {
                output[index] = value.remove(0);
            } else {
                uncached.push((index, key, text.clone()));
            }
        }
        if !uncached.is_empty() {
            let batch: Vec<_> = uncached.iter().map(|(_, _, text)| text.clone()).collect();
            let translated = match &route {
                Route::Direct(model) => checked_inference(provider, model, &batch, None, settings)?,
                Route::Pivot { first, second } => {
                    let intermediate = checked_inference(provider, first, &batch, None, settings)?;
                    checked_inference(provider, second, &intermediate, None, settings)?
                }
            };
            let mut rows = Vec::with_capacity(translated.len());
            for ((index, key, _), text) in uncached.into_iter().zip(translated) {
                output[index] = text;
                rows.push((key, index));
            }
            self.save_cached_segments(
                rows.into_iter()
                    .map(|(key, index)| (key, std::slice::from_ref(&output[index]))),
            )?;
        }
        Ok(Some(AlignedTranslation {
            route,
            segments: output,
            joined_lines: false,
        }))
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
            let mut statement = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO translation_cache(key, body) VALUES (?1, ?2)",
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
            [CACHE_ROWS],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
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
    let can_retry = || {
        segments.iter().any(|text| {
            let text = text.as_ref();
            text.len() <= 768 && crate::marian_deadline::find(text).is_some()
        })
    };
    let deadline_revision = match route {
        Route::Direct(edge) => from_uk_to_fr(edge) && can_retry(),
        Route::Pivot { first, second } => {
            from_uk_to_fr(first) && can_retry() || from_uk_to_fr(second)
        }
    };
    let version = if deadline_revision { 5 } else { 4 };
    let input = serde_json::to_vec(&(version, route, segments, context, settings))
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
    provider: &mut impl TranslationProvider,
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
