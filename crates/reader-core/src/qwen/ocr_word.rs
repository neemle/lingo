use super::{review_note, HintWork, PositionedWordExample, HINT_GENERATION_MODEL};
use crate::{
    ocr::{byte_at_utf16, differs_by_one_glyph, OcrLine},
    Result, Store,
};
use serde::{Deserialize, Serialize};

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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OcrSpelling {
    #[serde(deserialize_with = "review_note")]
    replacement: Option<String>,
}

#[derive(Serialize)]
pub struct OcrWordReview {
    replacement: Option<String>,
    cached: bool,
}

pub struct ReviewOcrWordJob {
    work: HintWork<OcrSpelling>,
    word: String,
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
        let (language, lines) = if let Some(page) = page {
            let cached = self
                .detached_pdf_ocr(id, page)?
                .ok_or("Розпізнана сторінка недоступна.")?;
            let text = self
                .detached_pdf_sentences(id, page)?
                .ok_or("Розпізнана сторінка недоступна.")?;
            if cached.revision != revision {
                return Err("Розпізнаний текст змінився. Виберіть слово знову.".into());
            }
            (text.language, cached.lines)
        } else {
            let document = self.translation_document(id)?;
            if document.current_revision_id.as_deref() != Some(revision) {
                return Err("Розпізнаний текст змінився. Виберіть слово знову.".into());
            }
            (document.language, document.ocr_lines.unwrap_or_default())
        };
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
        Ok(ReviewOcrWordJob {
            work,
            word: word.to_owned(),
        })
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
    pub fn run(self) -> Result<Option<OcrWordReview>> {
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
        Ok(result.map(|(hint, cached)| OcrWordReview {
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
