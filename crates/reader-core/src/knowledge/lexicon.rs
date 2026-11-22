use super::{word_key, WordAnalysis, WordContext};
use crate::{space::ensure_capacity, Result, Store};
use flate2::read::MultiGzDecoder;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fmt::{Display, Write as _},
    fs,
    io::{self, BufRead, BufReader, Read, Seek},
    path::{Path, PathBuf},
};
use unicode_segmentation::UnicodeSegmentation;

mod search;

const REVISION: &str = "enwiktionary-2026-10-01-kaikki-2026-10-30";
const INDEX_VERSION: u32 = 2;
const LEGACY_INDEX_VERSION: u32 = 1;
const MAX_UNPACKED_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MAX_LINE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SENSES_PER_TERM: i64 = 48;
const MAX_RESULTS: usize = 12;

struct Pack {
    language: &'static str,
    name: &'static str,
    bytes: u64,
    sha256: &'static str,
}

// Hashes cover the original gzip files emitted by Kaikki using Wiktextract
// (19f9a30/6f96ddf). The publisher's download URLs may advance independently.
const PACKS: &[Pack] = &[
    Pack {
        language: "en",
        name: "English",
        bytes: 534_186_395,
        sha256: "4d03b6cba283744756c57ff661c2c6cbfe34fcf8ff513a966576cf7d97e91a99",
    },
    Pack {
        language: "fr",
        name: "French",
        bytes: 59_206_544,
        sha256: "224fce45865bad43c4f43f11d0f6fc84e044ffda8407c549609639cfc7d38cc7",
    },
    Pack {
        language: "uk",
        name: "Ukrainian",
        bytes: 36_040_429,
        sha256: "2b8074025c192314a3906b6bb33587ff1c97f06b398f422308001561124b9586",
    },
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LexicalPackage {
    pub(crate) language: &'static str,
    name: String,
    source_url: String,
    download_url: String,
    pub(crate) revision: &'static str,
    license: &'static str,
    gloss_language: &'static str,
    bytes: u64,
    sha256: &'static str,
    pub(crate) installed: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Example {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    translation: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Sense {
    id: String,
    lemma: String,
    part_of_speech: String,
    gloss: String,
    examples: Vec<Example>,
    tags: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordMeaning {
    #[serde(flatten)]
    sense: Sense,
    gloss_language: &'static str,
    source_name: &'static str,
    source_url: String,
    source_license: &'static str,
    source_revision: &'static str,
    confidence: &'static str,
    context_matches: usize,
}

#[derive(Deserialize)]
struct WiktextractEntry {
    word: String,
    lang_code: String,
    pos: String,
    #[serde(default)]
    senses: Vec<WiktextractSense>,
}

#[derive(Deserialize)]
struct WiktextractSense {
    #[serde(default)]
    glosses: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    examples: Vec<WiktextractExample>,
    #[serde(default)]
    form_of: Vec<WiktextractReference>,
}

#[derive(Deserialize)]
struct WiktextractReference {
    word: String,
}

#[derive(Deserialize)]
struct WiktextractExample {
    #[serde(default)]
    text: String,
    english: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "ref")]
    reference: Option<String>,
}

impl Pack {
    fn path(&self, root: &Path, version: u32) -> PathBuf {
        root.join(self.language)
            .join("lexicon")
            .join(REVISION)
            .join(format!("meanings-v{version}.sqlite"))
    }

    fn installed_path(&self, root: &Path) -> Option<(PathBuf, u32)> {
        [INDEX_VERSION, LEGACY_INDEX_VERSION]
            .into_iter()
            .map(|version| (self.path(root, version), version))
            .find(|(path, _)| path.is_file())
    }

    fn describe(&self, installed: bool) -> LexicalPackage {
        let source_url = format!("https://kaikki.org/dictionary/{}/", self.name);
        LexicalPackage {
            language: self.language,
            name: format!("Wiktionary · {}", self.name),
            download_url: format!("{source_url}kaikki.org-dictionary-{}.jsonl.gz", self.name),
            source_url,
            revision: REVISION,
            license: "CC BY-SA 4.0",
            gloss_language: "en",
            bytes: self.bytes,
            sha256: self.sha256,
            installed,
        }
    }

    fn open(&self, root: &Path) -> Result<Option<(Connection, u32)>> {
        let Some((path, version)) = self.installed_path(root) else {
            return Ok(None);
        };
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map(|db| Some((db, version)))
            .map_err(lexical_error)
    }
}

pub struct LexicalPackageInstall {
    pack: &'static Pack,
    source: PathBuf,
    destination: PathBuf,
}

impl Store {
    pub fn lexical_packages(&self) -> Vec<LexicalPackage> {
        PACKS
            .iter()
            .map(|pack| pack.describe(pack.installed_path(&self.knowledge_directory).is_some()))
            .collect()
    }

    pub fn prepare_lexical_package(
        &self,
        language: &str,
        source: &Path,
    ) -> Result<LexicalPackageInstall> {
        let pack = find_pack(language).ok_or("Для цієї мови немає словникового пакета.")?;
        Ok(LexicalPackageInstall {
            pack,
            source: source.to_owned(),
            destination: pack.path(&self.knowledge_directory, INDEX_VERSION),
        })
    }

    pub fn install_lexical_package(&self, language: &str, source: &Path) -> Result<LexicalPackage> {
        self.prepare_lexical_package(language, source)?.run()
    }
}

impl LexicalPackageInstall {
    pub fn run(self) -> Result<LexicalPackage> {
        let pack = self.pack;
        let mut source = fs::File::open(self.source).map_err(lexical_error)?;
        if source.metadata().map_err(lexical_error)?.len() != pack.bytes {
            return Err("Розмір словникового пакета не збігається з каталогом.".into());
        }
        let mut verified = HashingReader::new((&mut source).take(pack.bytes + 1));
        io::copy(&mut verified, &mut io::sink()).map_err(lexical_error)?;
        verified.verify(pack)?;
        source.rewind().map_err(lexical_error)?;

        let directory = self
            .destination
            .parent()
            .ok_or("Невірний шлях словникового пакета.")?;
        fs::create_dir_all(directory).map_err(lexical_error)?;
        ensure_capacity([(directory, pack.bytes * 4)])?;
        let staging = tempfile::NamedTempFile::new_in(directory).map_err(lexical_error)?;
        // Verify the exact bytes used to build the index even if another process
        // modifies the selected file after preflight.
        let mut compressed = HashingReader::new(source.take(pack.bytes + 1));
        let decoder = MultiGzDecoder::new(&mut compressed);
        build_index(staging.path(), pack.language, BufReader::new(decoder))?;
        compressed.verify(pack)?;
        staging.as_file().sync_all().map_err(lexical_error)?;
        staging.persist(self.destination).map_err(lexical_error)?;
        Ok(pack.describe(true))
    }
}

struct HashingReader<R> {
    input: R,
    bytes: u64,
    hash: Sha256,
}

impl<R> HashingReader<R> {
    fn new(input: R) -> Self {
        Self {
            input,
            bytes: 0,
            hash: Sha256::new(),
        }
    }

    fn verify(self, pack: &Pack) -> Result<()> {
        if self.bytes != pack.bytes || format!("{:x}", self.hash.finalize()) != pack.sha256 {
            return Err("SHA-256 словникового пакета не збігається з каталогом.".into());
        }
        Ok(())
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let n = self.input.read(output)?;
        self.hash.update(&output[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
}

fn find_pack(language: &str) -> Option<&'static Pack> {
    PACKS.iter().find(|pack| pack.language == language)
}

fn build_index(path: &Path, language: &str, mut input: impl BufRead) -> Result<()> {
    let mut db = Connection::open(path).map_err(lexical_error)?;
    db.execute_batch(
        "CREATE TABLE meanings (
            term TEXT NOT NULL, ordinal INTEGER NOT NULL, body TEXT NOT NULL,
            PRIMARY KEY (term, ordinal)
         ) WITHOUT ROWID;
         CREATE TABLE lemma_links (
            term TEXT NOT NULL, lemma TEXT NOT NULL,
            PRIMARY KEY (term, lemma)
         ) WITHOUT ROWID;",
    )
    .map_err(lexical_error)?;
    db.execute_batch(search::SCHEMA).map_err(lexical_error)?;
    let tx = db.transaction().map_err(lexical_error)?;
    let mut line = Vec::new();
    let mut unpacked = 0;
    let mut ordinal = 0i64;
    {
        let mut insert = tx
            .prepare("INSERT INTO meanings VALUES (?1,?2,?3)")
            .map_err(lexical_error)?;
        let mut insert_link = tx
            .prepare("INSERT OR IGNORE INTO lemma_links VALUES (?1,?2)")
            .map_err(lexical_error)?;
        let mut insert_search = tx.prepare(search::INSERT).map_err(lexical_error)?;
        loop {
            line.clear();
            let bytes = (&mut input)
                .take(MAX_LINE_BYTES + 1)
                .read_until(b'\n', &mut line)
                .map_err(lexical_error)? as u64;
            if bytes == 0 {
                break;
            }
            unpacked += bytes;
            if bytes > MAX_LINE_BYTES || unpacked > MAX_UNPACKED_BYTES {
                return Err("Розпакований словниковий пакет завеликий.".into());
            }
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let entry: WiktextractEntry = serde_json::from_slice(&line).map_err(lexical_error)?;
            if entry.lang_code != language || !is_text(&entry.word, 240) || !is_text(&entry.pos, 50)
            {
                continue;
            }
            let term = lexical_key(language, &entry.word);
            for sense in entry.senses {
                for form in &sense.form_of {
                    if is_text(&form.word, 240) {
                        let lemma = lexical_key(language, &form.word);
                        if lemma != term {
                            insert_link
                                .execute(params![term, lemma])
                                .map_err(lexical_error)?;
                        }
                    }
                }
                let Some(sense) = dictionary_sense(language, &entry.word, &entry.pos, sense) else {
                    continue;
                };
                let body = serde_json::to_string(&sense).map_err(lexical_error)?;
                insert
                    .execute(params![term, ordinal, body])
                    .map_err(lexical_error)?;
                search::index(&mut insert_search, language, &term, ordinal, &sense)?;
                ordinal += 1;
            }
        }
    }
    if ordinal == 0 {
        return Err("У словниковому пакеті немає визначень.".into());
    }
    tx.commit().map_err(lexical_error)
}

fn dictionary_sense(
    language: &str,
    word: &str,
    pos: &str,
    sense: WiktextractSense,
) -> Option<Sense> {
    if sense
        .tags
        .iter()
        .any(|tag| matches!(tag.as_str(), "form-of" | "no-gloss"))
    {
        return None;
    }
    let gloss = sense.glosses.last()?.trim();
    if !is_text(gloss, 2_000) {
        return None;
    }

    let part_of_speech = match pos {
        "adj" => "ADJ",
        "adv" => "ADV",
        "article" | "det" => "DET",
        "conj" => "CONJ",
        "intj" => "INTJ",
        "name" => "PROPN",
        "noun" => "NOUN",
        "num" => "NUM",
        "particle" => "PART",
        "prep" => "ADP",
        "pron" => "PRON",
        "verb" => "VERB",
        other => other,
    };
    let id = crate::card_learning::sense_id(language, word, part_of_speech, gloss);
    let examples = sense
        .examples
        .into_iter()
        .filter(|example| example.kind.as_deref() == Some("example") && example.reference.is_none())
        .filter_map(|example| {
            let text = example.text.trim();
            is_text(text, 800).then(|| Example {
                text: text.to_owned(),
                translation: example
                    .english
                    .filter(|translation| is_text(translation, 800)),
            })
        })
        .take(3)
        .collect();
    let tags = sense
        .tags
        .into_iter()
        .filter(|tag| is_text(tag, 80))
        .take(10)
        .collect();
    Some(Sense {
        id,
        lemma: word.to_owned(),
        part_of_speech: part_of_speech.to_owned(),
        gloss: gloss.to_owned(),
        examples,
        tags,
    })
}

fn is_text(value: &str, maximum_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum_bytes && !value.chars().any(char::is_control)
}

pub(super) fn lookup(
    root: &Path,
    language: &str,
    selected: &str,
    analyses: &[WordAnalysis],
    expressions: &[super::expressions::ExpressionMatch],
    context: Option<&WordContext>,
    cue: Option<super::grammar_cues::GrammarCue>,
) -> Result<(Option<LexicalPackage>, Vec<WordMeaning>)> {
    let Some(pack) = find_pack(language) else {
        return Ok((None, Vec::new()));
    };
    let Some((db, version)) = pack.open(root)? else {
        return Ok((Some(pack.describe(false)), Vec::new()));
    };

    let context = context.filter(|context| context.neighbors(selected).is_some());
    let gloss_anchor = context
        .filter(|_| language == "en")
        .and_then(english_complement_anchor);
    let selected = lexical_key(language, selected);
    let context_words = context.map_or_else(BTreeSet::new, |context| {
        content_words(language, &context.sentence)
    });
    let mut keys = BTreeSet::from([selected.clone()]);
    let grammar: Vec<_> = analyses
        .iter()
        .map(|analysis| (lexical_key(language, &analysis.lemma), analysis))
        .collect();
    keys.extend(grammar.iter().map(|(key, _)| key.clone()));
    // UD particle entries use a gap marker in their canonical display.
    keys.extend(
        expressions
            .iter()
            .map(|expression| lexical_key(language, &expression.canonical.replace(" … ", " "))),
    );
    let mut redirects = db
        .prepare("SELECT lemma FROM lemma_links WHERE term=?1 ORDER BY lemma LIMIT 8")
        .map_err(lexical_error)?;
    let redirects = redirects
        .query_map([&selected], |row| row.get::<_, String>(0))
        .map_err(lexical_error)?;
    for lemma in redirects {
        keys.insert(lemma.map_err(lexical_error)?);
    }

    let mut query = db
        .prepare("SELECT ordinal, body FROM meanings WHERE term=?1 ORDER BY ordinal LIMIT ?2")
        .map_err(lexical_error)?;
    let mut unique = BTreeSet::new();
    let mut candidates = Vec::new();
    for key in keys {
        let mut rows = query
            .query_map(params![key, MAX_SENSES_PER_TERM], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(lexical_error)?
            .collect::<rusqlite::Result<Vec<(i64, String)>>>()
            .map_err(lexical_error)?;
        // Use postings to shortlist contextual evidence and recover matching senses past the exact cutoff.
        let hits = if version == INDEX_VERSION && rows.len() > 1 && context.is_some() {
            let after = rows
                .last()
                .filter(|_| rows.len() as i64 == MAX_SENSES_PER_TERM)
                .map_or(i64::MAX, |(ordinal, _)| *ordinal);
            search::matching(
                &db,
                &key,
                after,
                &selected,
                &context_words,
                gloss_anchor.as_deref(),
            )?
        } else {
            None
        };
        let indexed_matches = hits.map(|hits| {
            let ordinals = hits
                .iter()
                .map(|(ordinal, _)| *ordinal)
                .collect::<BTreeSet<_>>();
            rows.extend(
                hits.into_iter()
                    .filter_map(|(ordinal, body)| Some((ordinal, body?))),
            );
            ordinals
        });
        for (ordinal, body) in rows {
            let sense: Sense = serde_json::from_str(&body).map_err(lexical_error)?;
            if !unique.insert(sense.id.clone()) {
                continue;
            }
            let matching_grammar = grammar.iter().filter(|(lemma, analysis)| {
                *lemma == key && analysis.part_of_speech == sense.part_of_speech
            });
            let grammar_context = matching_grammar
                .clone()
                .map(|(_, analysis)| analysis.context_observations)
                .max();
            let grammar_priority = cue.map_or(0, |cue| {
                cue.lexical_priority(
                    &sense.tags,
                    matching_grammar
                        .clone()
                        .any(|(_, analysis)| analysis.grammar_evidence.is_some()),
                )
            });
            let has_context = indexed_matches
                .as_ref()
                .is_none_or(|hits| hits.contains(&ordinal));
            let example_matches = if has_context {
                sense
                    .examples
                    .iter()
                    .map(|example| {
                        contextual_overlap(language, &example.text, &context_words, &key, &selected)
                    })
                    .max()
                    .unwrap_or_default()
            } else {
                0
            };
            // An of-complement gives more direct lexical evidence than unrelated words elsewhere in the sentence.
            let gloss_matches = has_context
                && gloss_anchor.as_ref().is_some_and(|anchor| {
                    *anchor != key
                        && *anchor != selected
                        && content_words(language, &sense.gloss).contains(anchor)
                });

            candidates.push((
                sense,
                example_matches,
                (
                    grammar_priority,
                    gloss_matches,
                    example_matches,
                    grammar_context,
                    key == selected,
                ),
            ));
        }
    }

    // Keep lexical alternatives even when one sense has contextual evidence.
    // Dictionary order breaks ties without implying corpus frequency.
    candidates.sort_by_key(|(_, _, rank)| std::cmp::Reverse(*rank));
    let only_candidate = candidates.len() == 1;
    let meanings = candidates
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(sense, context_matches, rank)| {
            let confidence = if context_matches >= 2 && (cue.is_none() || rank.0 > 0) {
                "contextual"
            } else if only_candidate {
                "dictionary"
            } else {
                "uncertain"
            };
            WordMeaning {
                source_url: article_url(&sense.lemma, pack.name),
                sense,
                gloss_language: "en",
                source_name: "English Wiktionary via Kaikki/Wiktextract",
                source_license: "CC BY-SA 4.0",
                source_revision: REVISION,
                confidence,
                context_matches,
            }
        })
        .collect();
    Ok((Some(pack.describe(true)), meanings))
}

pub(super) fn available(root: &Path, language: &str) -> bool {
    find_pack(language).is_some_and(|pack| pack.installed_path(root).is_some())
}

fn lexical_key(language: &str, word: &str) -> String {
    let mut key = word_key(word);
    if language == "uk" {
        // Ukrainian dictionary forms may include an optional stress marker.
        key.retain(|symbol| symbol != '\u{0301}');
    }
    key
}

fn content_words(language: &str, text: &str) -> BTreeSet<String> {
    text.unicode_words()
        .filter(|word| word.chars().filter(|ch| ch.is_alphabetic()).take(4).count() >= 4)
        .take(128)
        .map(|word| lexical_key(language, word))
        .filter(|word| {
            language != "en"
                || !matches!(
                    word.as_str(),
                    "about"
                        | "also"
                        | "from"
                        | "have"
                        | "into"
                        | "over"
                        | "some"
                        | "such"
                        | "than"
                        | "that"
                        | "their"
                        | "them"
                        | "then"
                        | "there"
                        | "these"
                        | "they"
                        | "this"
                        | "those"
                        | "under"
                        | "were"
                        | "what"
                        | "when"
                        | "where"
                        | "which"
                        | "while"
                        | "with"
                        | "would"
                        | "your"
                )
        })
        .collect()
}

fn english_complement_anchor(context: &WordContext) -> Option<String> {
    let mut after = context
        .parts()
        .filter(|(offset, _)| *offset > context.start_utf16);
    if word_key(after.next()?.1) != "of" {
        return None;
    }
    let leading = word_key(after.next()?.1);
    let word = if matches!(leading.as_str(), "the" | "a" | "an") {
        word_key(after.next()?.1)
    } else {
        leading
    };
    (word.chars().count() >= 3 && word.chars().all(char::is_alphabetic)).then_some(word)
}

fn contextual_overlap(
    language: &str,
    candidate: &str,
    source: &BTreeSet<String>,
    lemma: &str,
    selected: &str,
) -> usize {
    content_words(language, candidate)
        .intersection(source)
        .filter(|word| word.as_str() != lemma && word.as_str() != selected)
        .count()
}

fn article_url(word: &str, language: &str) -> String {
    let mut url = "https://en.wiktionary.org/wiki/".to_owned();
    for byte in word.bytes() {
        match byte {
            b' ' => url.push('_'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                url.push(char::from(byte))
            }
            _ => write!(url, "%{byte:02X}").expect("writing to a String cannot fail"),
        }
    }
    url.push('#');
    url.push_str(language);
    url
}

fn lexical_error(error: impl Display) -> String {
    format!("Помилка словникового пакета. {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn indexed_dictionary_context_recovers_senses_after_the_exact_cutoff() {
        let directory = tempfile::tempdir().unwrap();
        for (language, term, sentence, example, gloss) in [
            (
                "fr",
                "MOULE",
                "📘 La MOULE à gâteau évoque l'été.",
                Some("Une moule à gâteau évoque l’été."),
                "Sense matched in an accented example",
            ),
            (
                "uk",
                "вікна",
                "🪟 Наші вікна прикрашають розкішними шторами.",
                Some("Ві́кна прикрашають розкі́шними шторами."),
                "Sense with Ukrainian stress marks in the example",
            ),
            (
                "en",
                "bank",
                "The bank of the river was steep.",
                None,
                "A slope alongside a river",
            ),
        ] {
            let mut senses: Vec<_> = (0..MAX_SENSES_PER_TERM)
                .map(|i| json!({ "glosses": [format!("Unrelated definition {i}")] }))
                .collect();
            senses.push(json!({ "glosses": [gloss], "examples": example.into_iter()
                .map(|text| json!({ "type": "example", "text": text })).collect::<Vec<_>>() }));
            let data = [
                json!({ "word": term, "lang_code": language, "pos": "noun", "senses": senses }),
                json!({ "word": "different", "lang_code": language, "pos": "noun",
                    "senses": [{ "glosses": [gloss] }] }),
            ]
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
            let pack = find_pack(language).unwrap();
            let old = pack.path(directory.path(), LEGACY_INDEX_VERSION);
            fs::create_dir_all(old.parent().unwrap()).unwrap();
            build_index(&old, language, data.as_bytes()).unwrap();
            Connection::open(&old)
                .unwrap()
                .execute_batch("DROP TABLE meaning_search")
                .unwrap();
            assert!(available(directory.path(), language));
            let context = WordContext {
                sentence: sentence.to_owned(),
                start_utf16: sentence[..sentence.find(term).unwrap()]
                    .encode_utf16()
                    .count(),
            };
            let find = |context: Option<&WordContext>| {
                lookup(
                    directory.path(),
                    language,
                    &word_key(term),
                    &[],
                    &[],
                    context,
                    None,
                )
                .unwrap()
                .1
            };
            assert!(find(Some(&context))
                .iter()
                .all(|row| row.sense.gloss != gloss));

            let current = pack.path(directory.path(), INDEX_VERSION);
            build_index(&current, language, data.as_bytes()).unwrap();
            let indexed = find(Some(&context));
            assert_eq!(indexed[0].sense.gloss, gloss);
            if example.is_some() {
                assert_eq!(indexed[0].context_matches, 3);
            }
            assert!(find(None).iter().all(|row| row.sense.gloss != gloss));
        }
    }
}
