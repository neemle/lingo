use crate::{model_storage::lock_package, Result, Store};
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Cursor, Read},
    path::{Path, PathBuf},
};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) mod annotation_cache;
mod expressions;
mod grammar_cues;
mod lexicon;
mod ocr_suggestions;
mod scoped_analysis;
pub(crate) use scoped_analysis::BoundaryAnalysis;
mod syntax;
mod syntax_parse;

const KNOWLEDGE_INSTALL_LOCK: &str = ".install-knowledge.lock";

fn remove_indexes(paths: &[PathBuf], lock_name: &str) -> Result<bool> {
    let directory = paths.first().and_then(|path| path.parent())
        .ok_or("Невірний шлях мовного пакета.")?;
    if !directory.try_exists().map_err(|error| error.to_string())? {
        return Ok(false);
    }
    let _lock = crate::file_originals::try_worker_lock(directory, lock_name)?
        .ok_or("Мовний пакет зараз оновлюється. Спробуйте пізніше.")?;
    let mut removed = false;
    for path in paths {
        match fs::remove_file(path) {
            Ok(()) => removed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(removed)
}

pub(crate) use scoped_analysis::{resolve_text_selection, select_words};
pub use scoped_analysis::{AnalysisScope, DetachedPageAnalysis, DetachedVocabularyInput, LinguisticAnalysis};

pub use lexicon::{LexicalPackage, LexicalPackageInstall, WordMeaning};

const INDEX_VERSION: u32 = 5;

struct Pack {
    language: &'static str,
    name: &'static str,
    repo: &'static str,
    file: &'static str,
    revision: &'static str,
    license: &'static str,
    bytes: usize,
    sha256: &'static str,
}

const PACKS: [Pack; 12] = [
    Pack {
        language: "en",
        name: "UD English EWT",
        repo: "UD_English-EWT",
        file: "en_ewt-ud-train.conllu",
        revision: "b7711cce01cdd4f5fcc0a8199b8a50d951b16c0c",
        license: "CC BY-SA 4.0",
        bytes: 15_034_493,
        sha256: "d68e06122a702464c613076523d56740f047e5bbe89dd90ec32737e04d952143",
    },
    Pack {
        language: "fr",
        name: "UD French GSD",
        repo: "UD_French-GSD",
        file: "fr_gsd-ud-train.conllu",
        revision: "15958a5df1d09e2a360784caaed07b9a3fcbf7a8",
        license: "CC BY-SA 4.0",
        bytes: 25_555_018,
        sha256: "4b9a87b1b7e6e7003219df1d3ccca87ffaca0f00a6055b12b1857df5373589d2",
    },
    Pack {
        language: "uk",
        name: "UD Ukrainian IU",
        repo: "UD_Ukrainian-IU",
        file: "uk_iu-ud-train.conllu",
        revision: "0d5ed088e959ccfbd933d768114d8503c32c9b37",
        license: "CC BY-NC-SA 4.0",
        bytes: 13_165_569,
        sha256: "f7282b32c72a2c6f304f1df13572b7dd9bff370d3da0abf726da00f6a15d1b14",
    },
    Pack {
        language: "de",
        name: "UD German GSD",
        repo: "UD_German-GSD",
        file: "de_gsd-ud-train.conllu",
        revision: "81d8c3612a88f5867fd089e9c0a5466ef230078b",
        license: "CC BY-SA 4.0",
        bytes: 19_637_803,
        sha256: "6cb1b600029292fb7bf60531a1d6d7824544791d732c0f7abae8102a2ac337bf",
    },
    Pack {
        language: "es",
        name: "UD Spanish AnCora",
        repo: "UD_Spanish-AnCora",
        file: "es_ancora-ud-train.conllu",
        revision: "197cca385e0e7db1b1fe26a5772dade1b6fbbee8",
        license: "CC BY 4.0",
        bytes: 43_100_702,
        sha256: "47b6fc49d6ed48a016e7fa5ad9f5c7ecf3ec21e9db005df3ea14d10f07c6deb6",
    },
    Pack {
        language: "it",
        name: "UD Italian ISDT",
        repo: "UD_Italian-ISDT",
        file: "it_isdt-ud-train.conllu",
        revision: "4852011b996b9ec30d884a7a48e1118d0ce928f6",
        license: "CC BY-NC-SA 3.0",
        bytes: 19_443_447,
        sha256: "7c3e32d7f296c877387a0c805c3ae8c471e2b5e8df97f5e3da810834b50566ab",
    },
    Pack {
        language: "pt",
        name: "UD Portuguese Bosque",
        repo: "UD_Portuguese-Bosque",
        file: "pt_bosque-ud-train.conllu",
        revision: "53bb7584bec8f6b83a27eaefe0656690df9d7773",
        license: "CC BY-SA 4.0",
        bytes: 11_291_250,
        sha256: "b56d7124af821e2968302c38298904ec67af342304a33ae8a9dc4fa57df89648",
    },
    Pack {
        language: "pl",
        name: "UD Polish LFG",
        repo: "UD_Polish-LFG",
        file: "pl_lfg-ud-train.conllu",
        revision: "9a0be46202ce87f0a507f427c52594c863a197e2",
        license: "GNU GPL 3.0",
        bytes: 11_898_531,
        sha256: "ac2ca65589af5789d2f18255fd80bb5cd07a7b278179e62165f1cf65bfc9acc2",
    },
    Pack {
        language: "nl",
        name: "UD Dutch Alpino",
        repo: "UD_Dutch-Alpino",
        file: "nl_alpino-ud-train.conllu",
        revision: "d64749ac918f18d31dd4c4e75ff56626432d9ba3",
        license: "CC BY-SA 4.0",
        bytes: 16_057_099,
        sha256: "d6612a025ae77a8f58494458b987be6f39fdcaa40c94b5e2a5522e50c4166cfb",
    },
    Pack {
        language: "cs",
        name: "UD Czech FicTree",
        repo: "UD_Czech-FicTree",
        file: "cs_fictree-ud-train.conllu",
        revision: "87f727e761e58117f4cfe9d9cc5dd2856a5fcbdd",
        license: "CC BY-NC-SA 4.0",
        bytes: 13_760_774,
        sha256: "8da3498cc9dfbe3ae00cfaf6287964f289f83325e3f6a921d686ce83733c0d64",
    },
    Pack {
        language: "ro",
        name: "UD Romanian RRT",
        repo: "UD_Romanian-RRT",
        file: "ro_rrt-ud-train.conllu",
        revision: "a654775d0b273f12169d8db623c8913c1433af75",
        license: "CC BY-SA 4.0",
        bytes: 14_603_210,
        sha256: "15c7a90a45f8a7ae809c09f6ec9cf2250bf4cf323cf0cbfcda7cfc4dbac4ea19",
    },
    Pack {
        language: "sv",
        name: "UD Swedish Talbanken",
        repo: "UD_Swedish-Talbanken",
        file: "sv_talbanken-ud-train.conllu",
        revision: "e64439a3a27906617e34b61fe1d29e3435f04ab6",
        license: "CC BY-SA 4.0",
        bytes: 6_023_121,
        sha256: "5e3c41f35999453c94a0f66155f695e6699950c44741abe7d98a2305f7f3f63d",
    },
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgePackage {
    pub(crate) language: &'static str,
    name: &'static str,
    version: &'static str,
    source_url: String,
    download_url: String,
    pub(crate) revision: &'static str,
    license: &'static str,
    bytes: usize,
    sha256: &'static str,
    pub(crate) installed: bool,
    context_available: bool,
    expressions_available: bool,
    syntax_available: bool,
    dependency_parse_available: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordAnalysis {
    lemma: String,
    part_of_speech: String,
    features: String,
    observations: u32,
    context_observations: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    grammar_evidence: Option<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordLookup {
    package: KnowledgePackage,
    total_observations: u32,
    analyses: Vec<WordAnalysis>,
    expressions: Vec<expressions::ExpressionMatch>,
    syntax_evidence: Vec<syntax::SyntaxEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    syntax_hint: Option<&'static str>,
    spelling_suggestions: Vec<ocr_suggestions::SpellingSuggestion>,
    lexical_package: Option<LexicalPackage>,
    meanings: Vec<WordMeaning>,
}

impl WordLookup {
    pub(crate) fn into_meanings(self) -> Vec<WordMeaning> {
        self.meanings
    }

    pub(crate) fn has_finite_verb_evidence(&self) -> bool {
        self.analyses.iter().any(|analysis|
            analysis.grammar_evidence == Some(grammar_cues::FRENCH_SUBJECT_VERB_AGREEMENT))
    }

    pub(crate) fn bind_expression_tokens(&mut self, token_id: impl Fn(usize) -> String) {
        for expression in &mut self.expressions {
            expression.token_ids = expression
                .part_starts_utf16
                .iter()
                .copied()
                .map(&token_id)
                .collect();
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WordContext {
    sentence: String,
    start_utf16: usize,
}

impl WordContext {
    pub(crate) fn at(sentence: &str, start_utf16: usize) -> Self {
        Self { sentence: sentence.into(), start_utf16 }
    }
}

impl Pack {
    fn path(&self, root: &Path) -> PathBuf {
        root.join(self.language)
            .join(self.revision)
            .join("knowledge.sqlite")
    }

    fn index_version(&self, root: &Path) -> u32 {
        Connection::open_with_flags(self.path(root), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .and_then(|db| db.query_row("PRAGMA user_version", [], |row| row.get(0)))
            .unwrap_or_default()
    }

    fn describe(&self, index_version: u32) -> KnowledgePackage {
        KnowledgePackage {
            language: self.language,
            name: self.name,
            version: "2.18",
            source_url: format!(
                "https://github.com/UniversalDependencies/{}/tree/{}",
                self.repo, self.revision
            ),
            download_url: format!(
                "https://raw.githubusercontent.com/UniversalDependencies/{}/{}/{}",
                self.repo, self.revision, self.file
            ),
            revision: self.revision,
            license: self.license,
            bytes: self.bytes,
            sha256: self.sha256,
            installed: index_version != 0,
            context_available: index_version >= 2,
            expressions_available: index_version >= 3,
            syntax_available: index_version >= 4,
            dependency_parse_available: index_version >= 5,
        }
    }
}

pub struct KnowledgePackageInstall {
    pack: &'static Pack,
    source: PathBuf,
    destination: PathBuf,
}

impl KnowledgePackageInstall {
    pub fn run(self) -> Result<KnowledgePackage> {
        let pack = self.pack;
        let mut file = fs::File::open(&self.source).map_err(|error| error.to_string())?;
        let mut data = Vec::with_capacity(pack.bytes);
        (&mut file)
            .take(pack.bytes as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|error| error.to_string())?;
        if data.len() != pack.bytes || format!("{:x}", Sha256::digest(&data)) != pack.sha256 {
            return Err("Розмір або SHA-256 мовного пакета не збігається з каталогом.".into());
        }
        let directory = self
            .destination
            .parent()
            .ok_or("Невірний шлях мовного пакета.")?;
        fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        let _lock = lock_package(&directory.join(KNOWLEDGE_INSTALL_LOCK), true)?;
        let staging =
            tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
        build_pack(staging.path(), BufReader::new(Cursor::new(data)))?;
        staging
            .as_file()
            .sync_all()
            .map_err(|error| error.to_string())?;
        staging
            .persist(&self.destination)
            .map_err(|error| error.to_string())?;
        Ok(pack.describe(INDEX_VERSION))
    }
}

impl Store {
    fn open_knowledge_index(&self, language: &str) -> Result<Option<(&'static Pack, Connection)>> {
        let Some(pack) = PACKS.iter().find(|pack| pack.language == language) else {
            return Ok(None);
        };
        let path = pack.path(&self.knowledge_directory);
        if !path.is_file() {
            return Ok(None);
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| error.to_string())?;
        Ok(Some((pack, db)))
    }

    pub fn knowledge_packages(&self) -> Vec<KnowledgePackage> {
        PACKS
            .iter()
            .map(|pack| pack.describe(pack.index_version(&self.knowledge_directory)))
            .collect()
    }

    pub fn prepare_knowledge_package(
        &self,
        language: &str,
        source: &Path,
    ) -> Result<KnowledgePackageInstall> {
        let pack = PACKS
            .iter()
            .find(|pack| pack.language == language)
            .ok_or("Для цієї мови немає пакета граматичного аналізу.")?;
        Ok(KnowledgePackageInstall {
            pack,
            source: source.to_owned(),
            destination: pack.path(&self.knowledge_directory),
        })
    }

    pub fn install_knowledge_package(
        &self,
        language: &str,
        source: &Path,
    ) -> Result<KnowledgePackage> {
        self.prepare_knowledge_package(language, source)?.run()
    }

    pub fn remove_knowledge_package(&self, language: &str) -> Result<bool> {
        let pack = PACKS.iter().find(|pack| pack.language == language)
            .ok_or("Для цієї мови немає пакета граматичного аналізу.")?;
        remove_indexes(&[pack.path(&self.knowledge_directory)], KNOWLEDGE_INSTALL_LOCK)
    }

    pub(crate) fn repair_pdf_ocr(&self, language: &str, lines: &mut [crate::ocr::OcrLine]) -> Result<()> {
        if let Some(db) = self.pdf_ocr_corpus(language)? {
            Self::repair_pdf_ocr_with_corpus(&db, language, lines)?;
        }
        Ok(())
    }

    pub(crate) fn pdf_ocr_corpus(&self, language: &str) -> Result<Option<Connection>> {
        let index = self.open_knowledge_index(language)?;
        let usable = versioned_index(&index)?.is_some_and(|(_, _, version)| version >= 2);
        Ok(index.filter(|_| usable).map(|(_, db)| db))
    }

    pub(crate) fn repair_pdf_ocr_with_corpus<'a>(
        db: &Connection, language: &str, lines: impl IntoIterator<Item = &'a mut crate::ocr::OcrLine>,
    ) -> Result<bool> {
        let mut changed = false;
        for line in lines.into_iter().filter(|line| line.needs_proofreading()) {
            if let Some(revised) = ocr_suggestions::revise_line(db, language, line)? {
                if revised.len() <= 8192 {
                    line.correction = Some(revised);
                    changed = true;
                }
            }
        }
        Ok(changed)
    }

    pub(crate) fn has_dictionary(&self, language: &str) -> bool {
        lexicon::available(&self.knowledge_directory, language)
    }

    pub fn lookup_word(
        &self,
        language: &str,
        word: &str,
        context: Option<&WordContext>,
    ) -> Result<Option<WordLookup>> {
        let index = self.open_knowledge_index(language)?;
        self.lookup_word_with_index(language, word, context, versioned_index(&index)?)
    }

    pub fn translation_gloss_matches(
        &self, language: &str, gloss: &str, texts: &[String],
    ) -> Result<Vec<Vec<crate::ocr::OcrSpan>>> {
        crate::valid_language(language)?;
        if gloss.len() > 512 || texts.len() > 64 || texts.iter().map(String::len).sum::<usize>() > 128_000 {
            return Err("Забагато тексту для підсвічування перекладу.".into());
        }
        let index = self.open_knowledge_index(language)?;
        let Some((_, db, _)) = versioned_index(&index)? else {
            return Ok(vec![Vec::new(); texts.len()]);
        };
        let mut forms = db.prepare_cached("SELECT DISTINCT lemma FROM forms WHERE term=?1 LIMIT 2")
            .map_err(|error| error.to_string())?;
        let mut cache = std::collections::BTreeMap::<String, Option<String>>::new();
        let mut unique_lemma = |term: &str| -> Result<Option<String>> {
            if let Some(found) = cache.get(term) {
                return Ok(found.clone());
            }
            let mut rows = forms.query([term]).map_err(|error| error.to_string())?;
            let first: Option<String> = rows.next().map_err(|error| error.to_string())?
                .map(|row| row.get(0)).transpose().map_err(|error| error.to_string())?;
            let lemma = if rows.next().map_err(|error| error.to_string())?.is_none() {
                first.as_deref().map(word_key)
            } else { None };
            cache.insert(term.to_owned(), lemma.clone());
            Ok(lemma)
        };
        let needles = crate::document_tokens::word_parts(gloss)
            .map(|(_, _, word)| {
                let key = word_key(word);
                let lemma = unique_lemma(&key)?;
                Ok((key, lemma))
            })
            .collect::<Result<Vec<_>>>()?;
        if needles.is_empty() {
            return Ok(vec![Vec::new(); texts.len()]);
        }
        texts.iter().map(|text| {
            let words = crate::document_tokens::word_parts(text).collect::<Vec<_>>();
            let mut spans = Vec::new();
            let mut next = 0;
            for (index, candidate) in words.windows(needles.len()).enumerate() {
                if index < next || candidate.windows(2).any(|pair| {
                    let end = pair[0].0 + pair[0].2.len();
                    !text[end..pair[1].0].chars().all(char::is_whitespace)
                }) {
                    continue;
                }
                let mut matched = true;
                for ((_, _, word), (needle, lemma)) in candidate.iter().zip(&needles) {
                    let key = word_key(word);
                    if &key != needle && (lemma.is_none() || unique_lemma(&key)? != *lemma) {
                        matched = false;
                        break;
                    }
                }
                if matched {
                    let (start_utf8, start_utf16, _) = candidate[0];
                    let (last_utf8, last_utf16, last) = candidate[candidate.len() - 1];
                    spans.push(crate::ocr::OcrSpan { start_utf8, start_utf16,
                        end_utf8: last_utf8 + last.len(), end_utf16: last_utf16 + last.encode_utf16().count() });
                    next = index + candidate.len();
                }
            }
            Ok(spans)
        }).collect()
    }

    fn lookup_word_with_index(
        &self,
        language: &str,
        word: &str,
        context: Option<&WordContext>,
        index: Option<(&Pack, &Connection, u32)>,
    ) -> Result<Option<WordLookup>> {
        let key = word_key(word);
        let syntax_hint = context.and_then(|context| {
            context.french_partitive_hint(language, &key)
                .or_else(|| context.french_spatial_hint(language, &key))
        });
        let Some((pack, db, index_version)) = index else {
            let Some(pack) = PACKS.iter().find(|pack| pack.language == language) else {
                return Ok(None);
            };
            if syntax_hint.is_none() && !lexicon::available(&self.knowledge_directory, language) {
                return Ok(None);
            }
            let (lexical_package, meanings) = lexicon::lookup(
                &self.knowledge_directory,
                language,
                &key,
                &[],
                &[],
                context,
                None,
            )?;
            return Ok(Some(WordLookup {
                package: pack.describe(0),
                total_observations: 0,
                analyses: Vec::new(),
                expressions: Vec::new(),
                syntax_evidence: Vec::new(),
                syntax_hint,
                spelling_suggestions: Vec::new(),
                lexical_package,
                meanings,
            }));
        };
        let neighbors = context.and_then(|context| context.neighbors(&key));
        let (previous, next) = neighbors.unwrap_or_default();
        let (left, right) = if index_version >= 2 {
            (previous.and_then(token_hash), next.and_then(token_hash))
        } else {
            (None, None)
        };
        let candidates = if left.is_some() || right.is_some() {
            "SELECT f.lemma, f.pos, f.features, f.observations, coalesce(m.hits,0) AS hits
             FROM forms f LEFT JOIN (
               SELECT lemma, pos, features, sum(observations) AS hits
               FROM neighbors WHERE term=?1 AND ((side=0 AND neighbor=?2) OR (side=1 AND neighbor=?3))
               GROUP BY lemma, pos, features
             ) m USING (lemma,pos,features) WHERE f.term=?1"
        } else {
            "SELECT lemma,pos,features,observations,0 AS hits FROM forms
             WHERE term=?1 AND ?2 IS NULL AND ?3 IS NULL"
        };
        let mut query = db
            .prepare(&format!(
                "WITH candidates AS ({candidates})
                 SELECT lemma,pos,features,observations,sum(observations) OVER (),hits FROM candidates
                 ORDER BY hits DESC,observations DESC,lemma,pos,features"
            ))
            .map_err(|error| error.to_string())?;
        let rows = query
            .query_map(params![key, left, right], |row| {
                Ok((
                    WordAnalysis {
                        lemma: row.get(0)?,
                        part_of_speech: row.get(1)?,
                        features: row.get(2)?,
                        observations: row.get(3)?,
                        context_observations: row.get(5)?,
                        grammar_evidence: None,
                    },
                    row.get::<_, u32>(4)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        let mut analyses = Vec::new();
        let mut total = 0;
        for row in rows {
            let (analysis, observations) = row.map_err(|error| error.to_string())?;
            total = observations;
            analyses.push(analysis);
        }
        let previous = previous.map(word_key);
        let cue = grammar_cues::GrammarCue::infer(language, previous.as_deref(), &analyses);
        grammar_cues::order_readings(&mut analyses, cue);
        let expressions = match context {
            Some(context) if index_version >= 3 => expressions::lookup(db, context, &key)?,
            _ => Vec::new(),
        };
        let syntax_evidence = match context {
            Some(context) if index_version >= 4 => syntax::lookup(db, context, &key)?,
            _ => Vec::new(),
        };
        let (lexical_package, meanings) = lexicon::lookup(
            &self.knowledge_directory,
            language,
            &key,
            &analyses,
            &expressions,
            context,
            cue,
        )?;
        let spelling_suggestions =
            if analyses.is_empty() && meanings.is_empty() && neighbors.is_some() {
                ocr_suggestions::suggest_spelling(db, language, word, &key, left, right)?
            } else {
                Vec::new()
            };
        Ok(Some(WordLookup {
            package: pack.describe(index_version),
            total_observations: total,
            analyses,
            expressions,
            syntax_evidence,
            syntax_hint,
            spelling_suggestions,
            lexical_package,
            meanings,
        }))
    }
}

fn versioned_index<'a>(
    index: &'a Option<(&'static Pack, Connection)>,
) -> Result<Option<(&'static Pack, &'a Connection, u32)>> {
    index.as_ref().map(|(pack, db)| {
        let version = db.query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        Ok((*pack, db, version))
    }).transpose()
}

fn neighbor_hash(key: &str) -> i64 {
    let hash = Sha256::digest(key.as_bytes());
    i64::from_le_bytes(hash[..8].try_into().expect("SHA-256 contains eight bytes"))
}

impl WordContext {
    pub(crate) fn new(sentence: String, start_utf16: usize) -> Self {
        Self { sentence, start_utf16 }
    }

    fn parts(&self) -> impl Iterator<Item = (usize, &str)> {
        self.sentence
            .split_word_bounds()
            .scan(0, |offset, part| {
                let start = *offset;
                *offset += part.encode_utf16().count();
                Some((start, part))
            })
            .filter(|(_, part)| !part.trim().is_empty())
    }

    fn french_partitive_hint(&self, language: &str, selected: &str) -> Option<&'static str> {
        if language != "fr" {
            return None;
        }
        let parts: Vec<_> = self.parts().map(|(offset, part)| (offset, word_key(part))).collect();
        let focus = parts.iter().position(|(offset, _)| *offset == self.start_utf16)?;
        if selected.is_empty() || !selected.split_word_bounds()
            .filter(|part| !part.trim().is_empty()).enumerate()
            .all(|(index, part)| parts.get(focus + index).is_some_and(|(_, token)| token == part))
        {
            return None;
        }
        let start = (0..=focus).rev().take(4).find(|&index| {
            matches!(parts[index].1.as_str(), "l'un" | "l'une")
                && parts.get(index + 1).is_some_and(|(_, token)| token == "des")
        })?;
        let word_after_des = parts.get(start + 2).map(|(_, token)| token.as_str());
        let qualifier = parts.get(start + 3).map(|(_, token)| token.as_str());
        if qualifier.is_some_and(|token| token == "de" || token.starts_with("d'")) {
            match word_after_des {
                Some("plus") => return Some(
                    "«plus de» означає «понад» і уточнює кількість у групі. «L’un / l’une» виділяє одного з цієї групи."
                ),
                Some("moins") => return Some(
                    "«moins de» означає «менше ніж» і уточнює кількість у групі. «L’un / l’une» виділяє одного з цієї групи."
                ),
                _ => {}
            }
        }
        let superlative = match word_after_des {
            Some(word @ ("plus" | "moins")) if qualifier
                .is_some_and(|token| token.chars().any(char::is_alphabetic)) => Some(word),
            _ => None,
        };
        if focus > start + 1 && superlative.is_none() {
            return None;
        }
        match (parts[start].1.as_str(), superlative) {
            ("l'un", Some("plus")) => Some(
                "«l’un des plus …» означає «один із най…». «l’un» замінює іменник чоловічого роду, «des» (de + les) вводить групу, а «plus» утворює найвищий ступінь."
            ),
            ("l'une", Some("plus")) => Some(
                "«l’une des plus …» означає «одна з най…». «l’une» замінює іменник жіночого роду, «des» (de + les) вводить групу, а «plus» утворює найвищий ступінь."
            ),
            (_, Some("moins")) => Some(
                "«l’un / l’une des moins …» означає «один / одна з найменш …». «des» (de + les) вводить групу, а «moins» позначає найнижчий ступінь ознаки."
            ),
            ("l'un", _) => Some(
                "«l’un des …» означає «один із …». «l’un» замінює іменник чоловічого роду, а «des» (de + les) вводить групу, з якої його виділяють."
            ),
            ("l'une", _) => Some(
                "«l’une des …» означає «одна з …». «l’une» замінює іменник жіночого роду, а «des» (de + les) вводить групу, з якої її виділяють."
            ),
            _ => None,
        }
    }

    pub(crate) fn french_spatial_hint(&self, language: &str, selected: &str) -> Option<&'static str> {
        if language != "fr" {
            return None;
        }
        let parts: Vec<_> = self.parts().collect();
        let focus = parts.iter().position(|(offset, _)| *offset == self.start_utf16)?;
        let matches_source = selected.split_word_bounds().filter(|part| !part.trim().is_empty())
            .enumerate().all(|(index, part)| {
                parts.get(focus + index).is_some_and(|(_, token)| word_key(token) == word_key(part))
            });
        if selected.trim().is_empty() || !matches_source {
            return None;
        }
        let token = |index: usize| parts.get(index).map(|&(_, token)| word_key(token));
        let nominal = |index: usize| {
            token(index).is_some_and(|token| {
                (matches!(token.as_str(), "le" | "la" | "les" | "un" | "une" | "des" | "ce" | "cet"
                    | "cette" | "ces" | "mon" | "ma" | "mes" | "son" | "sa" | "ses" | "leur" | "leurs")
                    && parts.get(index + 1).is_some_and(|(_, word)| word.starts_with(char::is_alphabetic)))
                    || token.strip_prefix("l'").is_some_and(|word| word.starts_with(char::is_alphabetic))
            })
        };
        match token(focus)?.as_str() {
            "devant" if nominal(focus + 1) && focus.checked_sub(1).and_then(token)
                .is_some_and(|previous| matches!(previous.as_str(),
                    "attend" | "attends" | "attendent" | "attendait" | "attendaient"
                    | "reste" | "restent" | "restait" | "restaient"
                    | "est" | "sont" | "était" | "étaient" | "stationne" | "stationnait")) =>
                Some("«devant» вводить обставину місця й указує, перед чим відбувається дія."),
            "derrière" if nominal(focus + 1) =>
                Some("«derrière» вводить прийменникову групу й указує, за чим щось розташовано або відбувається."),
            "près" if token(focus + 1).is_some_and(|next| {
                (next == "de" && nominal(focus + 2))
                    || (matches!(next.as_str(), "du" | "des" | "d'un" | "d'une") &&
                        parts.get(focus + 2).is_some_and(|(_, word)| word.starts_with(char::is_alphabetic)))
            }) => Some("«près de» вводить прийменникову групу й указує на близькість до місця або предмета."),
            _ => None,
        }
    }

    fn neighbors(&self, selected: &str) -> Option<(Option<&str>, Option<&str>)> {
        let mut parts = self.parts();
        let mut previous = None;
        while let Some((start, part)) = parts.next() {
            if start == self.start_utf16 && word_key(part) == selected {
                return Some((previous, parts.next().map(|(_, word)| word)));
            }
            if start >= self.start_utf16 {
                break;
            }
            previous = Some(part);
        }
        None
    }
}

fn token_hash(part: &str) -> Option<i64> {
    part.chars()
        .any(char::is_alphabetic)
        .then(|| neighbor_hash(&word_key(part)))
}

pub(crate) fn word_key(word: &str) -> String {
    word.trim()
        .to_lowercase()
        .replace(['\u{2019}', '\u{02bc}'], "'")
        .nfc()
        .collect()
}

fn build_pack(path: &Path, input: impl BufRead) -> Result<()> {
    let mut db = Connection::open(path).map_err(|error| error.to_string())?;
    db.execute_batch(
        "CREATE TABLE forms (term TEXT NOT NULL, lemma TEXT NOT NULL, pos TEXT NOT NULL,
            features TEXT NOT NULL, observations INTEGER NOT NULL,
            PRIMARY KEY(term, lemma, pos, features)) WITHOUT ROWID;
         CREATE TABLE neighbors (term TEXT NOT NULL, side INTEGER NOT NULL, neighbor INTEGER NOT NULL,
            lemma TEXT NOT NULL, pos TEXT NOT NULL, features TEXT NOT NULL, observations INTEGER NOT NULL,
            PRIMARY KEY(term,side,neighbor,lemma,pos,features)) WITHOUT ROWID;
         CREATE TABLE expressions (anchor TEXT NOT NULL, kind TEXT NOT NULL, parts TEXT NOT NULL,
             observations INTEGER NOT NULL, PRIMARY KEY(anchor,kind,parts)) WITHOUT ROWID;",
    )
    .map_err(|error| error.to_string())?;
    db.execute_batch(syntax::SCHEMA)
        .map_err(|error| error.to_string())?;
    let transaction = db.transaction().map_err(|error| error.to_string())?;
    transaction
        .pragma_update(None, "user_version", INDEX_VERSION)
        .map_err(|error| error.to_string())?;
    let mut inserted = 0usize;
    {
        let mut insert = transaction.prepare(
            "INSERT INTO forms VALUES (?1,?2,?3,?4,1) ON CONFLICT DO UPDATE SET observations=observations+1",
        ).map_err(|error| error.to_string())?;
        let mut link = transaction.prepare(
            "INSERT INTO neighbors VALUES (?1,?2,?3,?4,?5,?6,1) ON CONFLICT DO UPDATE SET observations=observations+1",
        ).map_err(|error| error.to_string())?;
        let mut previous = None;
        let mut expressions = expressions::Indexer::new(&transaction)?;
        let mut syntax = syntax::Indexer::new(&transaction)?;
        for line in input.lines() {
            let line = line.map_err(|error| error.to_string())?;
            if line.is_empty() {
                expressions.flush()?;
                syntax.flush()?;
                previous = None;
                continue;
            }
            if line.starts_with('#') {
                continue;
            }
            let mut fields = line.split('\t');
            let [Some(id), Some(form), Some(lemma), Some(pos), _, Some(features)] =
                std::array::from_fn(|_| fields.next())
            else {
                return Err("Неповний запис у корпусі CoNLL-U.".into());
            };
            let Ok(id) = id.parse::<u32>() else {
                continue;
            };
            if let (Some(head), Some(relation)) = (fields.next(), fields.next()) {
                let head = head.parse().unwrap_or_default();
                expressions.token(id, form, lemma, head, relation);
                syntax.token(id, form, pos, head, relation);
            }
            if !form.chars().any(char::is_alphabetic)
                || lemma == "_"
                || matches!(pos, "_" | "X" | "PUNCT" | "SYM")
            {
                previous = None;
                continue;
            }
            let term = word_key(form);
            let hash = neighbor_hash(&term);
            let features = if features == "_" { "" } else { features };
            insert
                .execute(params![term, lemma, pos, features])
                .map_err(|error| error.to_string())?;
            if let Some((prev_term, prev_lemma, prev_pos, prev_features, prev_hash)) = previous {
                link.execute(params![term, 0, prev_hash, lemma, pos, features])
                    .map_err(|error| error.to_string())?;
                link.execute(params![
                    prev_term,
                    1,
                    hash,
                    prev_lemma,
                    prev_pos,
                    prev_features
                ])
                .map_err(|error| error.to_string())?;
            }
            previous = Some((
                term,
                lemma.to_owned(),
                pos.to_owned(),
                features.to_owned(),
                hash,
            ));
            inserted += 1;
        }
        expressions.flush()?;
        syntax.flush()?;
    }
    if inserted == 0 {
        return Err("У корпусі немає граматичних записів.".into());
    }
    transaction.commit().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn french_partitive_explains_the_selected_expression_and_its_members_without_a_pack() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let sentence = "« Il est l’un des plus riches du village. »";
        for selected in ["l’un", "l’un des plus riches", "des plus riches", "plus", "riches"] {
            let start = sentence.find(selected).unwrap();
            let context = WordContext::at(sentence, sentence[..start].encode_utf16().count());
            let lookup = store.lookup_word("fr", selected, Some(&context)).unwrap().unwrap();
            assert!(lookup.syntax_hint.unwrap().contains("найвищий ступінь"), "{selected}");
        }
        let unrelated = WordContext::at(sentence, sentence.find("village").unwrap());
        assert!(store.lookup_word("fr", "village", Some(&unrelated)).unwrap().is_none());
        let punctuation = WordContext::at("L’un, des gens sont venus.", 0);
        assert!(store.lookup_word("fr", "l’un", Some(&punctuation)).unwrap().is_none());
    }

    #[test]
    fn french_partitive_quantity_hint_reaches_every_analysis_scope() {
        use crate::knowledge::scoped_analysis::AnalysisScope;

        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let text = "C’est l’un des plus de trois cents candidats.";
        let doc = store.import("sample.txt", text.as_bytes(), "fr").unwrap();
        let sentence = &doc.sentences[0];
        let revision = doc.current_revision_id.as_deref().unwrap();
        let offset = |word| text[..text.find(word).unwrap()].encode_utf16().count();
        let word = offset("l’un");
        for scope in [
            AnalysisScope::Word { start_utf16: word },
            AnalysisScope::Expression { member_starts_utf16: vec![word, offset("des"), offset("plus")] },
            AnalysisScope::Sentence { start: 0 },
        ] {
            let analysis = store.cached_linguistic_analysis(&doc.id, &sentence.id, revision, scope).unwrap();
            let hints = analysis["syntaxHints"].as_array().unwrap();
            assert_eq!(hints.len(), 1);
            assert!(hints[0].as_str().unwrap().contains("«понад»"));
            let selected = analysis["words"].as_array().unwrap().iter()
                .find(|word| word["text"] == "l’un").unwrap();
            assert_eq!(selected["spellingStatus"], "unavailable");
        }
    }

    #[test]
    fn translated_gloss_matches_inflection_as_utf16_without_crossing_punctuation() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(&directory.path().join("user.sqlite")).unwrap();
        let path = PACKS.iter().find(|pack| pack.language == "uk").unwrap().path(&store.knowledge_directory);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        build_pack(&path, concat!(
            "1\tпідйомний\tпідйомний\tADJ\t_\t_\t0\troot\t_\t_\n",
            "2\tпідйомним\tпідйомний\tADJ\t_\t_\t1\tdep\t_\t_\n",
            "3\tкран\tкран\tNOUN\t_\t_\t1\tdep\t_\t_\n",
            "4\tкраном\tкран\tNOUN\t_\t_\t1\tdep\t_\t_\n",
        ).as_bytes()).unwrap();
        let text = "📚 Працюють підйомним краном.";
        let inputs = [text.into(), "Підйомним, краном.".into(), "Краном.".into()];
        let matches = store.translation_gloss_matches("uk", "підйомний кран", &inputs).unwrap();
        let [matched] = matches[0].as_slice() else { panic!("expected one phrase match"); };
        assert_eq!(&text[matched.start_utf8..matched.end_utf8], "підйомним краном");
        assert_eq!(matched.start_utf16, "📚 Працюють ".encode_utf16().count());
        assert!(matches[1].is_empty());
        assert!(matches[2].is_empty());
        assert!(store.translation_gloss_matches("fr", "grue", &inputs).unwrap().iter().all(Vec::is_empty));
    }

    #[test]
    fn lookup_keeps_ambiguous_readings_and_normalizes_unicode() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(&directory.path().join("user.sqlite")).unwrap();
        let path = PACKS[1].path(&store.knowledge_directory);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let corpus = concat!(
            "# sent_id = sample\n",
            "1\tCafé\tcafé\tNOUN\t_\tGender=Masc|Number=Sing\t0\troot\t_\t_\n",
            "2\tCAFÉ\tcafé\tNOUN\t_\tGender=Masc|Number=Sing\t1\tdep\t_\t_\n",
            "3\tCafé\tCafé\tPROPN\t_\t_\t1\tdep\t_\t_\n",
            "4-5\tCafé\t_\t_\t_\t_\t_\t_\t_\t_\n",
        );
        build_pack(&path, corpus.as_bytes()).unwrap();
        let found = store
            .lookup_word("fr", "CAFE\u{301}", None)
            .unwrap()
            .unwrap();
        assert_eq!(found.total_observations, 3);
        assert_eq!(found.analyses[0].lemma, "café");
        assert_eq!(found.analyses[0].observations, 2);
        assert_eq!(found.analyses[1].part_of_speech, "PROPN");
        assert!(store.lookup_word("en", "café", None).unwrap().is_none());
        assert!(store
            .lookup_word("fr", "unknown", None)
            .unwrap()
            .unwrap()
            .analyses
            .is_empty());
    }

    #[test]
    fn contextual_evidence_can_promote_a_rare_reading_and_old_indexes_still_work() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(&directory.path().join("user.sqlite")).unwrap();
        let path = PACKS[0].path(&store.knowledge_directory);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let corpus = concat!(
            "1\tI\tI\tPRON\t_\t_\n",
            "2\tsaw\tsee\tVERB\t_\tTense=Past\n\n",
            "1\tThey\tthey\tPRON\t_\t_\n",
            "2\tsaw\tsee\tVERB\t_\tTense=Past\n\n",
            "1\told\told\tADJ\t_\t_\n",
            "2\tsaw\tsaw\tNOUN\t_\tNumber=Sing\n",
        );
        build_pack(&path, corpus.as_bytes()).unwrap();
        let context = WordContext {
            sentence: "😀 old saw.".into(),
            start_utf16: "😀 old ".encode_utf16().count(),
        };
        let found = store
            .lookup_word("en", "saw", Some(&context))
            .unwrap()
            .unwrap();
        assert_eq!(found.total_observations, 3);
        assert_eq!(found.analyses[0].part_of_speech, "NOUN");
        assert_eq!(found.analyses[0].context_observations, 1);
        assert_eq!(found.analyses[1].part_of_speech, "VERB");
        let missing = WordContext {
            sentence: "old, saw.".into(),
            start_utf16: 5,
        };
        assert_eq!(
            store
                .lookup_word("en", "saw", Some(&missing))
                .unwrap()
                .unwrap()
                .analyses[0]
                .part_of_speech,
            "VERB"
        );

        Connection::open(&path)
            .unwrap()
            .execute_batch("DROP TABLE neighbors; PRAGMA user_version=1;")
            .unwrap();
        let old = store
            .lookup_word("en", "saw", Some(&context))
            .unwrap()
            .unwrap();
        assert!(old.package.installed);
        assert!(!old.package.context_available);
        assert_eq!(old.analyses[0].part_of_speech, "VERB");
        assert_eq!(old.analyses[0].context_observations, 0);
    }

    #[test]
    fn ocr_suggestions_rank_confusions_and_preserve_utf16_offsets() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(&directory.path().join("user.sqlite")).unwrap();
        let path = PACKS[0].path(&store.knowledge_directory);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        build_pack(
            &path,
            "1\tmagic\tmagic\tNOUN\t_\t_\n2\tbook\tbook\tNOUN\t_\t_\n".as_bytes(),
        )
        .unwrap();
        let text = "😀 Magic, RNAGIC and B0ok.";
        let suggestions = store.suggest_ocr("en", text).unwrap().unwrap();
        let changes = serde_json::to_value(suggestions).unwrap();
        let first = &changes[0];
        assert_eq!(first["source"], "RNAGIC");
        assert_eq!(first["replacement"], "MAGIC");
        assert_eq!(first["startUtf16"], "😀 Magic, ".encode_utf16().count());
        assert_eq!(first["lengthUtf16"], 6);
        assert_eq!(changes[1]["replacement"], "Book");
        assert!(store.suggest_ocr("fr", text).unwrap().is_none());
    }

    #[test]
    fn imported_photo_and_scan_use_corpus_corrections_without_losing_original_spans() {
        use crate::ocr::{OcrLine, OcrWord};

        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("reader.db");
        let mut store = Store::open(&database).unwrap();
        let path = PACKS[0].path(&store.knowledge_directory);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        build_pack(&path, concat!(
            "1\tA\ta\tDET\t_\t_\n",
            "2\tmagic\tmagic\tADJ\t_\t_\n",
            "3\tbook\tbook\tNOUN\t_\t_\n",
        ).as_bytes()).unwrap();
        let raw = "A maglc book.";
        let uncertain = OcrLine {
            rotation: None,
            block_start: false,
            page: 0, text: raw.into(), x: 0.1, y: 0.1, width: 0.8, height: 0.1,
            confidence: Some(0.8), correction: None, source_span: None,
            words: vec![OcrWord {
                text: "maglc".into(), x: 0.2, y: 0.1, width: 0.15, height: 0.1,
                raw_start_utf16: Some(2), source_span: None,
            }],
        };
        let lines = vec![
            uncertain.clone(),
            OcrLine { y: 0.3, confidence: Some(0.98), words: vec![], ..uncertain.clone() },
            OcrLine { y: 0.5, correction: Some("A maglc book!".into()), words: vec![], ..uncertain },
        ];
        let image = b"\x89PNG\r\n\x1a\n";
        let pdf = b"%PDF-1.4\n";
        let photo = store.import_image("photo.png", image, "en", lines.clone()).unwrap();
        let scan = store.import_scanned_pdf("scan.pdf", pdf, "en", lines, 1).unwrap();
        drop(store);
        let store = Store::open(&database).unwrap();
        for (imported, original) in [(&photo, image.as_slice()), (&scan, pdf.as_slice())] {
            let document = store.translation_document(&imported.id).unwrap();
            assert_eq!(store.original(&document.id).unwrap().bytes, original);
            let lines = document.ocr_lines.as_ref().unwrap();
            assert_eq!(lines[0].text, raw);
            assert_eq!(lines[0].reading_text(), "A magic book.");
            assert_eq!(lines[1].reading_text(), raw);
            assert_eq!(lines[2].reading_text(), "A maglc book!");
            let word = &lines[0].words[0];
            let span = word.source_span.unwrap();
            assert_eq!(word.text, "maglc");
            assert_eq!(&document.text[span.start_utf8..span.end_utf8], "magic");
            assert_eq!(crate::translation::sentence_input(
                &store, &document, 0, "uk", crate::translation::PageBoundaries::Canonical,
            ).unwrap().0, "A magic book.");
            let (_, speech) = store.speech_source(&document.id, &document.sentences[0].id, false)
                .unwrap().unwrap();
            assert_eq!(speech.text, "A magic book.");
        }
    }

    #[test]
    fn photo_proofreading_preserves_annotations_when_ocr_offsets_shift() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("reader.db")).unwrap();
        let line = |text: &str, y| crate::ocr::OcrLine {
            rotation: None,
            block_start: false,
            page: 0, text: text.into(), x: 0.1, y, width: 0.8, height: 0.1,
            confidence: Some(0.8), correction: None, words: vec![], source_span: None,
        };
        let original = b"\x89PNG\r\n\x1a\n";
        let photo = store.import_image("photo.png", original, "en", vec![
            line("A rnagic book.", 0.1),
            line("Read it again.", 0.4),
        ]).unwrap();
        let revision = photo.current_revision_id.as_deref().unwrap();
        store.set_bookmark(&photo.id, &photo.sentences[0].id, true).unwrap();
        store.set_sentence_note(&photo.id, &photo.sentences[1].id, "A note").unwrap();
        store.set_reviewed_translation(&photo.id, &photo.sentences[0].id, "uk", "Чарівна книжка.").unwrap();
        assert!(store.reconcile_photo_ocr(&photo.id, revision).is_err());

        let path = PACKS[0].path(&store.knowledge_directory);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        build_pack(&path, concat!(
            "1\tA\ta\tDET\t_\t_\n",
            "2\tmagic\tmagic\tADJ\t_\t_\n",
            "3\tbook\tbook\tNOUN\t_\t_\n",
        ).as_bytes()).unwrap();
        assert!(store.reconcile_photo_ocr(&photo.id, revision).unwrap());
        let revised = store.translation_document(&photo.id).unwrap();
        assert_eq!(revised.text, "A magic book.\n\nRead it again.");
        assert_eq!(revised.sentences[1].start_utf16 + 1, photo.sentences[1].start_utf16);
        assert_eq!(revised.bookmarks, [revised.sentences[0].id.clone()]);
        assert_eq!(revised.notes[&revised.sentences[1].id], "A note");
        assert_eq!(revised.reviewed_translations["uk"][&revised.sentences[0].id], "Чарівна книжка.");
        assert_eq!(revised.ocr_lines.unwrap()[0].text, "A rnagic book.");
        assert_eq!(store.original(&photo.id).unwrap().bytes, original);
        assert!(store.reconcile_photo_ocr(&photo.id, revision).is_err());
        assert!(!store.reconcile_photo_ocr(&photo.id, revised.current_revision_id.as_deref().unwrap()).unwrap());
    }

    #[test]
    fn recheck_scan_without_text_layer_uses_corpus_on_canonical_and_detached_pages() {
        use crate::{file_originals, ocr::OcrLine, pdf_detached_pages};
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("reader.db")).unwrap();
        let original = directory.path().join("scan.pdf");
        fs::write(&original, "%PDF-1.4\n").unwrap();
        let prepared = file_originals::prepare_pdf_import(
            store.pdf_original_root(), "scan.pdf", &original, "en", vec![String::new(); 2], vec![],
        ).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        let raw = "A rnagic book.";
        let line = |page| OcrLine {
            rotation: None,
            block_start: false,
            page, text: raw.into(), x: 0.1, y: 0.1, width: 0.8, height: 0.1,
            confidence: Some(0.8), correction: None, words: vec![], source_span: None,
        };
        let protected = |page| OcrLine { text: "A magic book.".into(), y: 0.4, ..line(page) };
        let document = store.cache_pdf_ocr_page(&document.id, 0, vec![line(0), protected(0)]).unwrap();
        store.correct_ocr_line(&document.id, 0, Some("A rnagic book!".into())).unwrap();
        store.correct_ocr_line(&document.id, 1, Some("A rnagic book.".into())).unwrap();
        let document = store.translation_document(&document.id).unwrap();
        pdf_detached_pages::write_ocr(&store.connection, &document.id, 1, "en", &[line(1), protected(1)], 2).unwrap();
        for (index, text) in ["A rnagic book!", "A rnagic book."].into_iter().enumerate() {
            let detached = serde_json::to_value(store.detached_pdf_ocr(&document.id, 1).unwrap()).unwrap();
            store.correct_detached_pdf_ocr(
                &document.id, 1, detached["revision"].as_str().unwrap(), index, Some(text.into()),
            ).unwrap();
        }
        let detached = serde_json::to_value(store.detached_pdf_ocr(&document.id, 1).unwrap()).unwrap();
        let path = PACKS[0].path(&store.knowledge_directory);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        build_pack(&path, concat!(
            "1\tA\ta\tDET\t_\t_\n",
            "2\tmagic\tmagic\tADJ\t_\t_\n",
            "3\tbook\tbook\tNOUN\t_\t_\n",
        ).as_bytes()).unwrap();
        assert!(store.reconcile_pdf_ocr_page(
            &document.id, 0, document.current_revision_id.as_deref().unwrap(), "en", "", false,
        ).unwrap());
        assert!(store.reconcile_pdf_ocr_page(
            &document.id, 1, detached["revision"].as_str().unwrap(), "en", "", true,
        ).unwrap());
        let snapshot = store.snapshot().unwrap();
        let canonical = snapshot.documents[0].ocr_lines.as_ref().unwrap();
        let detached = store.detached_pdf_ocr(&document.id, 1).unwrap().unwrap();
        for lines in [canonical, &detached.lines] {
            assert_eq!(lines[0].text, raw);
            assert_eq!(lines[0].reading_text(), "A magic book!");
            assert_eq!(lines[1].reading_text(), "A rnagic book.");
        }
        assert!(!store.reconcile_pdf_ocr_page(
            &document.id, 0, snapshot.documents[0].current_revision_id.as_deref().unwrap(), "en", "", false,
        ).unwrap());
    }

    #[test]
    fn expression_lookup_finds_parts_and_keeps_clause_boundaries() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("reader.sqlite")).unwrap();
        let path = PACKS[0].path(&store.knowledge_directory);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let corpus = concat!(
            "1\tTurn\tturn\tVERB\t_\t_\t0\troot\n",
            "2\tlights\tlight\tNOUN\t_\t_\t1\tobj\n",
            "3\toff\toff\tADP\t_\t_\t1\tcompound:prt\n\n",
            "1\tturned\tturn\tVERB\t_\t_\t0\troot\n",
            "2\tbecause\tbecause\tADP\t_\t_\t1\tobl\n",
            "3\tof\tof\tADP\t_\t_\t2\tfixed\n",
        );
        build_pack(&path, corpus.as_bytes()).unwrap();
        let lookup = |sentence: &str, selected: &str| {
            let start = sentence.find(selected).unwrap();
            let context = WordContext {
                sentence: sentence.into(),
                start_utf16: sentence[..start].encode_utf16().count(),
            };
            serde_json::to_value(
                store
                    .lookup_word("en", selected, Some(&context))
                    .unwrap()
                    .unwrap(),
            )
            .unwrap()
        };
        for selected in ["turned", "off"] {
            let found = lookup("😀 He turned the lights off.", selected);
            assert_eq!(found["expressions"][0]["text"], "turned … off");
            assert_eq!(found["expressions"][0]["kind"], "particle");
        }
        assert_eq!(
            lookup("He turned because of the rain.", "of")["expressions"][0]["text"],
            "because of"
        );
        assert!(lookup("He turned the lights, off.", "off")["expressions"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(lookup("He turned the lights off.", "lights")["expressions"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}
