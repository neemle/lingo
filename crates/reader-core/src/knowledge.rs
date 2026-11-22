use crate::{Result, Store};
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

mod expressions;
mod lexicon;
mod ocr_suggestions;

pub use lexicon::{LexicalPackage, LexicalPackageInstall, WordMeaning};

const INDEX_VERSION: u32 = 3;

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

const PACKS: [Pack; 3] = [
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
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgePackage {
    language: &'static str,
    name: &'static str,
    version: &'static str,
    source_url: String,
    download_url: String,
    revision: &'static str,
    license: &'static str,
    bytes: usize,
    sha256: &'static str,
    installed: bool,
    context_available: bool,
    expressions_available: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordAnalysis {
    lemma: String,
    part_of_speech: String,
    features: String,
    observations: u32,
    context_observations: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordLookup {
    package: KnowledgePackage,
    total_observations: u32,
    analyses: Vec<WordAnalysis>,
    expressions: Vec<expressions::ExpressionMatch>,
    lexical_package: Option<LexicalPackage>,
    meanings: Vec<WordMeaning>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WordContext {
    sentence: String,
    start_utf16: usize,
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
        }
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

    pub fn install_knowledge_package(
        &self,
        language: &str,
        source: &Path,
    ) -> Result<KnowledgePackage> {
        let pack = PACKS
            .iter()
            .find(|pack| pack.language == language)
            .ok_or("Для цієї мови немає пакета граматичного аналізу.")?;
        let mut file = fs::File::open(source).map_err(|error| error.to_string())?;
        let mut data = Vec::with_capacity(pack.bytes);
        (&mut file)
            .take(pack.bytes as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|error| error.to_string())?;
        if data.len() != pack.bytes || format!("{:x}", Sha256::digest(&data)) != pack.sha256 {
            return Err("Розмір або SHA-256 мовного пакета не збігається з каталогом.".into());
        }
        let destination = pack.path(&self.knowledge_directory);
        let directory = destination
            .parent()
            .ok_or("Невірний шлях мовного пакета.")?;
        fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        let staging =
            tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
        build_pack(staging.path(), BufReader::new(Cursor::new(data)))?;
        staging
            .as_file()
            .sync_all()
            .map_err(|error| error.to_string())?;
        staging
            .persist(&destination)
            .map_err(|error| error.to_string())?;
        Ok(pack.describe(INDEX_VERSION))
    }

    pub fn lookup_word(
        &self,
        language: &str,
        word: &str,
        context: Option<&WordContext>,
    ) -> Result<Option<WordLookup>> {
        let key = word_key(word);
        let Some((pack, db)) = self.open_knowledge_index(language)? else {
            let Some(pack) = PACKS.iter().find(|pack| pack.language == language) else {
                return Ok(None);
            };
            if !lexicon::available(&self.knowledge_directory, language) {
                return Ok(None);
            }
            let (lexical_package, meanings) =
                lexicon::lookup(&self.knowledge_directory, language, &key, &[], &[], context)?;
            return Ok(Some(WordLookup {
                package: pack.describe(0),
                total_observations: 0,
                analyses: Vec::new(),
                expressions: Vec::new(),
                lexical_package,
                meanings,
            }));
        };
        let index_version: u32 = db
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        let (left, right) = context
            .filter(|_| index_version >= 2)
            .map(|context| context.neighbor_hashes(&key))
            .unwrap_or_default();
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
                "WITH candidates AS ({candidates}), ranked AS (
                    SELECT lemma,pos,features,observations,hits,sum(observations) OVER () AS total,
                           row_number() OVER (PARTITION BY lemma,pos ORDER BY hits DESC,observations DESC,features) AS variant
                    FROM candidates
                 )
                 SELECT lemma,pos,features,observations,total,hits FROM ranked
                 ORDER BY variant != 1,hits DESC,observations DESC,lemma,pos,features LIMIT 5"
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
        let expressions = match context {
            Some(context) if index_version >= 3 => expressions::lookup(&db, context, &key)?,
            _ => Vec::new(),
        };
        let (lexical_package, meanings) = lexicon::lookup(
            &self.knowledge_directory,
            language,
            &key,
            &analyses,
            &expressions,
            context,
        )?;
        Ok(Some(WordLookup {
            package: pack.describe(index_version),
            total_observations: total,
            analyses,
            expressions,
            lexical_package,
            meanings,
        }))
    }
}

fn neighbor_hash(key: &str) -> i64 {
    let hash = Sha256::digest(key.as_bytes());
    i64::from_le_bytes(hash[..8].try_into().expect("SHA-256 contains eight bytes"))
}

impl WordContext {
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

    fn neighbor_hashes(&self, selected: &str) -> (Option<i64>, Option<i64>) {
        let mut parts = self.parts();
        let mut previous = None;
        while let Some((start, part)) = parts.next() {
            if start == self.start_utf16 && word_key(part) == selected {
                return (
                    previous.and_then(token_hash),
                    parts.next().and_then(|(_, word)| token_hash(word)),
                );
            }
            if start >= self.start_utf16 {
                break;
            }
            previous = Some(part);
        }
        (None, None)
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
        for line in input.lines() {
            let line = line.map_err(|error| error.to_string())?;
            if line.is_empty() {
                expressions.flush()?;
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
                expressions.token(id, form, lemma, head.parse().unwrap_or_default(), relation);
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
