use super::{generate, prompt_checkpoint, review_note, with_prompt_context, HintWork, PositionedWordExample, HINT_GENERATION_MODEL};
use crate::{
    ocr::{byte_at_utf16, differs_by_one_glyph, OcrLine, OcrLineEdit},
    Result, Store,
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const SYSTEM: &str = concat!(
    "<|im_start|>system\n",
    "Check only the highlighted word from OCR in the given language. ",
    "Read before and after as source text. Keep correct words and names. ",
    "If a clearly misread glyph changes the spelling, return one corrected word in JSON. ",
    "Otherwise return {\"replacement\":null}. Do not translate or rewrite surrounding words.",
    "<|im_end|>\n",
    "<|im_start|>user\n{\"language\":\"en\",\"before\":\"A \",\"word\":\"rnagic\",\"after\":\" book.\"}<|im_end|>\n",
    "<|im_start|>assistant\n{\"replacement\":\"magic\"}<|im_end|>\n",
    "<|im_start|>user\n",
);

const GRAMMAR: &str = r#"
root ::= "{" ws "\"replacement\"" ws ":" ws replacement ws "}"
replacement ::= "null" | "\"" [A-Za-zÀ-ÖØ-öø-ÿĀ-ɏА-Яа-яІіЇїЄєҐґ]{2,32} "\""
ws ::= [ \t]{0,2}
"#;

const LINE_SYSTEM: &str = concat!(
    "<|im_start|>system\n",
    "Check a line of OCR text in the given language. Keep its wording and punctuation. ",
    "Fix only visually plausible spelling errors. Do not translate, rewrite, or correct grammar. ",
    "The words array is numbered from zero. Return at most four edits with a zero-based word index. ",
    "If no clear mistakes exist, return {\"edits\":[]}.\n<|im_end|>\n",
    "<|im_start|>user\n{\"language\":\"en\",\"text\":\"A rnagic b0ok.\",\"words\":[\"A\",\"rnagic\",\"b0ok\"]}<|im_end|>\n",
    "<|im_start|>assistant\n{\"edits\":[{\"index\":1,\"replacement\":\"magic\"},{\"index\":2,\"replacement\":\"book\"}]}<|im_end|>\n",
    "<|im_start|>user\n",
);

const LINE_GRAMMAR: &str = r#"
root ::= "{" ws "\"edits\"" ws ":" ws "[" ws (edit (ws "," ws edit){0,3})? ws "]" ws "}"
edit ::= "{" ws "\"index\"" ws ":" ws ("0" | [1-9] [0-9]?) ws "," ws "\"replacement\"" ws ":" ws "\"" [A-Za-zÀ-ÖØ-öø-ÿĀ-ɏА-Яа-яІіЇїЄєҐґ]{2,32} "\"" ws "}"
ws ::= [ \t]{0,2}
"#;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OcrSpelling {
    #[serde(deserialize_with = "review_note")]
    replacement: Option<String>,
}

#[derive(Serialize)]
pub struct OcrReview {
    replacement: Option<String>,
    cached: bool,
}

pub struct ReviewOcrWordJob {
    work: HintWork<OcrSpelling>,
    word: String,
}

pub struct ReviewOcrLineJob {
    work: HintWork<OcrSpelling>,
    text: String,
}

pub struct ReviewOcrPageJob {
    lines: Vec<(usize, ReviewOcrLineJob)>,
    remaining_lines: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrPageReview {
    checked_lines: usize,
    reviewed_indices: Vec<usize>,
    remaining_lines: usize,
    corrections: Vec<OcrLineEdit>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LineEdits {
    edits: Vec<LineEdit>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LineEdit {
    index: usize,
    replacement: String,
}

impl Store {
    pub fn prepare_ocr_word_review(
        &self,
        id: &str,
        page: Option<u16>,
        revision: &str,
        start_utf16: usize,
        word: &str,
        cached_only: bool,
    ) -> Result<ReviewOcrWordJob> {
        if !(2..=32).contains(&word.chars().count())
            || !word.chars().any(char::is_alphabetic)
            || !word
                .chars()
                .all(|ch| ch.is_alphabetic() || ch.is_ascii_digit())
        {
            return Err("Виберіть одне розпізнане слово.".into());
        }
        let (language, lines) = self.ocr_review_lines(id, page, revision)?;
        let (before, after) = word_context(&lines, start_utf16, word)
            .ok_or("Слово не відповідає збереженій позиції OCR.")?;
        let example = serde_json::to_string(&PositionedWordExample {
            language: &language,
            before,
            word,
            after,
        })
        .map_err(|error| error.to_string())?;
        let work = self.prepare_hint_work(
            HINT_GENERATION_MODEL,
            id,
            "qwen-ocr-word-v1",
            SYSTEM,
            example,
            cached_only.into(),
        )?;
        Ok(ReviewOcrWordJob { work, word: word.to_owned() })
    }

    pub fn prepare_ocr_line_review(
        &self, id: &str, page: Option<u16>, revision: &str, index: usize, cached_only: bool,
    ) -> Result<ReviewOcrLineJob> {
        let (language, lines) = self.ocr_review_lines(id, page, revision)?;
        let line = lines.get(index).ok_or("Розпізнаний рядок не знайдено.")?;
        self.prepare_ocr_line_hint(id, &language, line.reading_text(), cached_only)
    }

    pub fn prepare_ocr_page_review(
        &self, id: &str, physical_page: u16, detached: bool, revision: &str, cached_only: bool,
        reviewed_indices: &[usize],
    ) -> Result<ReviewOcrPageJob> {
        const BATCH_SIZE: usize = 6;
        let reviewed: std::collections::HashSet<_> = reviewed_indices.iter().copied().collect();
        let (language, lines) = self.ocr_review_lines(id, detached.then_some(physical_page), revision)?;
        let mut candidates: Vec<_> = lines.iter().enumerate().filter(|(index, line)|
            line.page == physical_page && line.correction.is_none() && line.needs_proofreading()
                && !reviewed.contains(index)
                && reviewable_line(line.reading_text())).collect();
        candidates.sort_by(|(_, left), (_, right)| left.confidence.partial_cmp(&right.confidence)
            .unwrap_or(std::cmp::Ordering::Equal));
        let remaining_lines = candidates.len().saturating_sub(BATCH_SIZE);
        let mut checked = candidates.into_iter().take(BATCH_SIZE).map(|(index, line)| {
            self.prepare_ocr_line_hint(id, &language, line.reading_text(), cached_only).map(|job| (index, job))
        }).collect::<Result<Vec<_>>>()?;
        checked.sort_by_key(|(index, _)| *index);
        Ok(ReviewOcrPageJob { lines: checked, remaining_lines })
    }

    fn prepare_ocr_line_hint(
        &self, id: &str, language: &str, text: &str, cached_only: bool,
    ) -> Result<ReviewOcrLineJob> {
        let words: Vec<_> = crate::document_tokens::word_parts(text).map(|(_, _, word)| word).collect();
        if text.len() > 600 || words.len() > 48 {
            return Err("Розпізнаний рядок задовгий для перевірки Qwen.".into());
        }
        if words.is_empty() {
            return Err("У цьому рядку немає слів для перевірки.".into());
        }
        let example = serde_json::json!({ "language": language, "text": text, "words": words }).to_string();
        let work = self.prepare_hint_work(
            HINT_GENERATION_MODEL, id, "qwen-ocr-line-v1", LINE_SYSTEM, example, cached_only.into(),
        )?;
        Ok(ReviewOcrLineJob { work, text: text.to_owned() })
    }

    fn ocr_review_lines(&self, id: &str, page: Option<u16>, revision: &str) -> Result<(String, Vec<OcrLine>)> {
        if let Some(page) = page {
            let cached = self
                .detached_pdf_ocr(id, page)?
                .ok_or("Розпізнана сторінка недоступна.")?;
            let text = self
                .detached_pdf_sentences(id, page)?
                .ok_or("Розпізнана сторінка недоступна.")?;
            if cached.revision != revision {
                return Err("Розпізнаний текст змінився. Виберіть слово знову.".into());
            }
            Ok((text.language, cached.lines))
        } else {
            let document = self.translation_document(id)?;
            if document.current_revision_id.as_deref() != Some(revision) {
                return Err("Розпізнаний текст змінився. Виберіть слово знову.".into());
            }
            Ok((document.language, document.ocr_lines.unwrap_or_default()))
        }
    }
}

fn reviewable_line(text: &str) -> bool {
    text.len() <= 600 && (1..=48).contains(&crate::document_tokens::word_parts(text).take(49).count())
}

fn apply_line_edits(text: &str, mut edits: Vec<LineEdit>) -> Option<String> {
    if edits.is_empty() || edits.len() > 4 { return None; }
    edits.sort_unstable_by_key(|edit| edit.index);
    if edits.windows(2).any(|pair| pair[0].index == pair[1].index) { return None; }
    let words: Vec<_> = crate::document_tokens::word_parts(text).collect();
    let mut output = text.to_owned();
    for edit in edits.into_iter().rev() {
        let Some(&(start, _, word)) = words.get(edit.index) else { continue; };
        if plausible_replacement(word, &edit.replacement) {
            output.replace_range(start..start + word.len(), &edit.replacement);
        }
    }
    (output != text).then_some(output)
}

impl ReviewOcrLineJob {
    pub fn run(self) -> Result<Option<OcrReview>> {
        let text = self.text;
        let result = self.work.run(LINE_SYSTEM, LINE_GRAMMAR, |output| parse_line_review(&text, output))?;
        Ok(result.map(|(hint, cached)| OcrReview { replacement: hint.replacement, cached }))
    }
}

fn parse_line_review(text: &str, output: &[u8]) -> Result<OcrSpelling> {
    let error = "Qwen не повернув перевірку рядка у правильному форматі.";
    let edits: LineEdits = serde_json::from_str(super::json_output(output).ok_or(error)?)
        .map_err(|_| error)?;
    Ok(OcrSpelling { replacement: apply_line_edits(text, edits.edits) })
}

impl ReviewOcrPageJob {
    pub fn run(self) -> Result<Option<OcrPageReview>> {
        let read_cache = || -> Result<Option<Vec<OcrSpelling>>> {
            let results = self.lines.iter().map(|(_, job)| match &job.work {
                HintWork::Cached(result) => Ok(result.clone()),
                HintWork::Generate { cache, .. } => cache.load(),
            }).collect::<Result<Vec<_>>>()?;
            Ok(results.into_iter().collect::<Option<Vec<OcrSpelling>>>())
        };
        let model = self.lines.iter().find_map(|(_, job)| match &job.work {
            HintWork::Generate { model, .. } => Some(model),
            HintWork::Cached(_) => None,
        });
        let Some(model) = model else {
            return Ok(read_cache()?.map(|results| self.merge(results)));
        };
        model.run_requested_with_cache(
            None,
            || Ok(read_cache()?.map(|results| Some(self.merge(results)))),
            |directory, should_yield| {
                let deadline = Instant::now() + Duration::from_secs(20);
                with_prompt_context(directory, || prompt_checkpoint(should_yield, deadline), |model, context| {
                    let mut results = Vec::with_capacity(self.lines.len());
                    for (_, job) in &self.lines {
                        match &job.work {
                            HintWork::Cached(Some(review)) => results.push(review.clone()),
                            HintWork::Cached(None) => return Ok(None),
                            HintWork::Generate { example, cache, .. } => {
                                let review = if let Some(review) = cache.load()? {
                                    review
                                } else {
                                    let deadline = Instant::now() + Duration::from_secs(20);
                                    let check = || prompt_checkpoint(should_yield, deadline);
                                    check()?;
                                    context.clear_kv_cache();
                                    let output = generate(model, context, LINE_SYSTEM, example, LINE_GRAMMAR, check)?;
                                    let review = parse_line_review(&job.text, &output)?;
                                    cache.save(&review)?;
                                    review
                                };
                                results.push(review);
                            }
                        }
                    }
                    Ok(Some(self.merge(results)))
                })
            },
        )
    }

    fn merge(&self, results: Vec<OcrSpelling>) -> OcrPageReview {
        let corrections = self.lines.iter().zip(results).filter_map(|((index, job), hint)|
            hint.replacement.map(|replacement| OcrLineEdit { index: *index, source: job.text.clone(), replacement })
        ).collect();
        OcrPageReview {
            checked_lines: self.lines.len(),
            reviewed_indices: self.lines.iter().map(|(index, _)| *index).collect(),
            remaining_lines: self.remaining_lines,
            corrections,
        }
    }
}

fn word_context<'a>(
    lines: &'a [OcrLine],
    start_utf16: usize,
    word: &str,
) -> Option<(&'a str, &'a str)> {
    let end_utf16 = start_utf16.checked_add(word.encode_utf16().count())?;
    let line = lines.iter().find(|line| {
        line.words.iter().any(|candidate| {
            candidate
                .source_span
                .is_some_and(|span| span.start_utf16 == start_utf16 && span.end_utf16 == end_utf16)
        })
    })?;
    let span = line.source_span?;
    let reading = line.reading_text();
    let start = byte_at_utf16(reading, start_utf16.checked_sub(span.start_utf16)?)?;
    let end = start.checked_add(word.len())?;
    if reading.get(start..end)? != word {
        return None;
    }
    let prefix = &reading[..start];
    let suffix = &reading[end..];
    let left = prefix
        .char_indices()
        .rev()
        .nth(95)
        .map_or(0, |(index, _)| index);
    let right = suffix
        .char_indices()
        .nth(96)
        .map_or(suffix.len(), |(index, _)| index);
    Some((&prefix[left..], &suffix[..right]))
}

fn plausible_replacement(word: &str, replacement: &str) -> bool {
    let cyrillic = |ch: char| ('\u{0400}'..='\u{04ff}').contains(&ch);
    let script = word
        .chars()
        .find(|ch| ch.is_alphabetic())
        .is_some_and(cyrillic);
    if word == replacement
        || !(2..=32).contains(&replacement.chars().count())
        || !replacement.chars().all(|ch| {
            ch.is_alphabetic()
                && cyrillic(ch) == script
                && (script || ch.is_ascii() || ('\u{00c0}'..='\u{024f}').contains(&ch))
        })
        || (word.starts_with(char::is_alphabetic)
            && word.starts_with(char::is_uppercase) != replacement.starts_with(char::is_uppercase))
    {
        return false;
    }
    differs_by_one_glyph(word, replacement)
        || [("rn", "m"), ("cl", "d"), ("vv", "w")]
            .into_iter()
            .any(|(from, to)| {
                [(from, to), (to, from)].into_iter().any(|(from, to)| {
                    word.match_indices(from).any(|(index, _)| {
                        replacement
                            .strip_prefix(&word[..index])
                            .and_then(|tail| tail.strip_prefix(to))
                            == Some(&word[index + from.len()..])
                    })
                })
            })
}

impl ReviewOcrWordJob {
    pub fn run(self) -> Result<Option<OcrReview>> {
        let word = self.word;
        let result = self.work.run(SYSTEM, GRAMMAR, |output| {
            let error = "Qwen не повернув перевірку слова у правильному форматі.";
            serde_json::from_str::<OcrSpelling>(super::json_output(output).ok_or(error)?)
                .map_err(|_| error.into())
                .map(|mut hint| {
                    hint.replacement = hint
                        .replacement
                        .filter(|replacement| plausible_replacement(&word, replacement));
                    hint
                })
        })?;
        Ok(result.map(|(hint, cached)| OcrReview {
            replacement: hint
                .replacement
                .filter(|replacement| plausible_replacement(&word, replacement)),
            cached,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_review_limits_work_to_the_six_least_confident_unedited_lines() {
        let mut store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let lines = (0..9).map(|index| OcrLine {
            page: 0, text: format!("A rnagic line {index}."), x: 0.1, y: 0.05 + f64::from(index) * 0.07,
            width: 0.8, height: 0.04,
            confidence: Some(0.9 - f64::from(index) * 0.03),
            correction: (index == 8).then(|| "A magic line 8.".to_owned()),
            words: vec![], source_span: None,
        }).collect();
        let document = store.import_image("scan.png", b"\x89PNG\r\n\x1a\n", "en", lines).unwrap();
        let revision = document.current_revision_id.as_deref().unwrap();
        let job = store.prepare_ocr_page_review(&document.id, 0, false, revision, true, &[]).unwrap();
        let reviewed: Vec<_> = job.lines.iter().map(|(index, _)| *index).collect();
        assert_eq!(reviewed, [2, 3, 4, 5, 6, 7]);
        assert_eq!(job.remaining_lines, 2);
        assert!(job.run().unwrap().is_none());
        let followup = store.prepare_ocr_page_review(&document.id, 0, false, revision, true, &reviewed).unwrap();
        assert_eq!(followup.lines.iter().map(|(index, _)| *index).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(followup.remaining_lines, 0);
        store.apply_ocr_page_review(&document.id, 0, false, revision, vec![OcrLineEdit {
            index: 2,
            source: "A rnagic line 2.".into(),
            replacement: "A magic line 2.".into(),
        }]).unwrap();
        let updated = store.token_document(&document.id).unwrap();
        let revision = updated.current_revision_id.as_deref().unwrap();
        let followup = store.prepare_ocr_page_review(&document.id, 0, false, revision, true, &reviewed).unwrap();
        assert_eq!(followup.lines.iter().map(|(index, _)| *index).collect::<Vec<_>>(), [0, 1]);
        let done = store.prepare_ocr_page_review(&document.id, 0, false, revision, true, &[0, 1, 2, 3, 4, 5, 6, 7])
            .unwrap().run().unwrap().unwrap();
        assert!(done.reviewed_indices.is_empty());
        assert_eq!(done.remaining_lines, 0);
    }

    #[test]
    fn line_review_keeps_spacing_and_punctuation_and_rejects_unrelated_edits() {
        let text = "A rnagic  b0ok, vraiment.";
        let edits: LineEdits = serde_json::from_str(r#"{"edits":[
            {"index":2,"replacement":"book"},
            {"index":1,"replacement":"magic"},
            {"index":3,"replacement":"autre"},
            {"index":42,"replacement":"fin"}
        ]}"#).unwrap();
        assert_eq!(apply_line_edits(text, edits.edits).as_deref(), Some("A magic  book, vraiment."));
        let edit = || LineEdit { index: 1, replacement: "magic".into() };
        assert!(apply_line_edits(text, vec![edit(), edit()]).is_none());
    }

    #[test]
    fn accepts_ocr_like_edits_and_rejects_rewrites_or_mixed_scripts() {
        for (source, correction) in [
            ("rnagic", "magic"),
            ("b0ok", "book"),
            ("fénêtre", "fenêtre"),
            ("сторінка", "сторинка"),
        ] {
            assert!(plausible_replacement(source, correction));
        }
        for (source, correction) in [
            ("rnagic", "an old book"),
            ("magic", "magic"),
            ("magic", "mаgic"),
            ("Magic", "magic"),
            ("magic", "conte"),
        ] {
            assert!(!plausible_replacement(source, correction));
        }
    }

    #[test]
    fn detached_review_uses_the_ocr_revision_and_word_coordinates() {
        use crate::{file_originals, ocr::OcrWord, pdf_detached_pages};

        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("reader.db")).unwrap();
        let pdf = temp.path().join("scan.pdf");
        std::fs::write(&pdf, "%PDF-1.4\n").unwrap();
        let prepared = file_originals::prepare_pdf_import(
            store.pdf_original_root(),
            "scan.pdf",
            &pdf,
            "en",
            vec![String::new(); 2],
            vec![],
        )
        .unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        let line = OcrLine {
            page: 1,
            text: "A rnagic book.".into(),
            x: 0.1,
            y: 0.1,
            width: 0.8,
            height: 0.1,
            confidence: Some(0.8),
            correction: None,
            source_span: None,
            words: vec![OcrWord {
                text: "rnagic".into(),
                x: 0.2,
                y: 0.1,
                width: 0.2,
                height: 0.1,
                raw_start_utf16: Some(2),
                source_span: None,
            }],
        };
        pdf_detached_pages::write_ocr(&store.connection, &document.id, 1, "en", &[line], 2)
            .unwrap();
        let saved = store.detached_pdf_ocr(&document.id, 1).unwrap().unwrap();
        let review = store
            .prepare_ocr_word_review(&document.id, Some(1), &saved.revision, 2, "rnagic", true)
            .unwrap()
            .run()
            .unwrap();
        assert!(review.is_none());
        assert!(store
            .prepare_ocr_word_review(&document.id, Some(1), &saved.revision, 3, "rnagic", true)
            .is_err());
        assert!(store
            .prepare_ocr_word_review(&document.id, Some(1), "stale", 2, "rnagic", true)
            .is_err());
    }
}
