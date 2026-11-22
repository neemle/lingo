//! Routes translations through models already installed on the device.

use crate::{valid_language, Result, Store};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

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

pub(crate) fn input_for_sentence(document: &crate::Document, selected: usize) -> (String, bool) {
    const MAX_JOINED_BYTES: usize = 1024;
    const MAX_FRAGMENTS: usize = 12;
    let active = &document.sentences[selected];
    if document.format != "pdf" && document.ocr_lines.is_none() {
        return (active.text.clone(), false);
    }
    let soft_wrap = |left: usize, right: usize| {
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
    };
    let within_budget = |left: usize, right: usize| {
        right - left < MAX_FRAGMENTS
            && document.sentences[right]
                .end_utf8
                .saturating_sub(document.sentences[left].start_utf8)
                <= MAX_JOINED_BYTES
    };
    let mut start = selected;
    let mut end = selected;
    while start > 0 && within_budget(start - 1, end) && soft_wrap(start - 1, start) {
        start -= 1;
    }
    while end + 1 < document.sentences.len()
        && within_budget(start, end + 1)
        && soft_wrap(end, end + 1)
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
        let key_bytes = serde_json::to_vec(&(1, &route, segments, context, settings))
            .map_err(|error| error.to_string())?;
        let key = format!("{:x}", Sha256::digest(key_bytes));
        let cached: Option<String> = self
            .connection
            .query_row(
                "SELECT body FROM translation_cache WHERE key = ?1",
                [&key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(segments) = cached
            .as_deref()
            .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
            .filter(|value| value.len() == segments.len() && valid_segments(value))
        {
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
        let value = serde_json::to_string(&result).map_err(|error| error.to_string())?;
        let tx = self
            .connection
            .transaction()
            .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT OR REPLACE INTO translation_cache(key, body) VALUES (?1, ?2)",
            params![key, value],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "DELETE FROM translation_cache WHERE rowid IN (
                SELECT rowid FROM translation_cache ORDER BY rowid DESC LIMIT -1 OFFSET ?1
            )",
            [CACHE_ROWS],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(Some(AlignedTranslation {
            route,
            segments: result,
            joined_lines: false,
        }))
    }
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
