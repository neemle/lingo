use crate::{Result, Store};
use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Cursor, Read},
    path::{Path, PathBuf},
};
use unicode_normalization::UnicodeNormalization;

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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordAnalysis {
    lemma: String,
    part_of_speech: String,
    features: String,
    observations: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordLookup {
    package: KnowledgePackage,
    total_observations: u32,
    analyses: Vec<WordAnalysis>,
}

impl Pack {
    fn path(&self, root: &Path) -> PathBuf {
        root.join(self.language)
            .join(self.revision)
            .join("knowledge.sqlite")
    }

    fn describe(&self, root: &Path) -> KnowledgePackage {
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
            installed: self.path(root).is_file(),
        }
    }
}

impl Store {
    pub fn knowledge_packages(&self) -> Vec<KnowledgePackage> {
        PACKS
            .iter()
            .map(|pack| pack.describe(&self.knowledge_directory))
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
        Ok(pack.describe(&self.knowledge_directory))
    }

    pub fn lookup_word(&self, language: &str, word: &str) -> Result<Option<WordLookup>> {
        let Some(pack) = PACKS.iter().find(|pack| pack.language == language) else {
            return Ok(None);
        };
        let path = pack.path(&self.knowledge_directory);
        if !path.is_file() {
            return Ok(None);
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| error.to_string())?;
        let key = word_key(word);
        let mut query = db
            .prepare(
                "WITH ranked AS (
                    SELECT lemma, pos, features, observations, sum(observations) OVER () AS total,
                           row_number() OVER (PARTITION BY lemma, pos ORDER BY observations DESC, features) AS variant
                    FROM forms WHERE term=?1
                 )
                 SELECT lemma, pos, features, observations, total FROM ranked
                 ORDER BY variant != 1, observations DESC, lemma, pos, features LIMIT 5",
            )
            .map_err(|error| error.to_string())?;
        let rows = query
            .query_map([&key], |row| {
                Ok((
                    WordAnalysis {
                        lemma: row.get(0)?,
                        part_of_speech: row.get(1)?,
                        features: row.get(2)?,
                        observations: row.get(3)?,
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
        Ok(Some(WordLookup {
            package: pack.describe(&self.knowledge_directory),
            total_observations: total,
            analyses,
        }))
    }
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
            PRIMARY KEY(term, lemma, pos, features)) WITHOUT ROWID; PRAGMA user_version=1;",
    )
    .map_err(|error| error.to_string())?;
    let transaction = db.transaction().map_err(|error| error.to_string())?;
    let mut inserted = 0usize;
    {
        let mut insert = transaction.prepare(
            "INSERT INTO forms VALUES (?1,?2,?3,?4,1) ON CONFLICT DO UPDATE SET observations=observations+1",
        ).map_err(|error| error.to_string())?;
        for line in input.lines() {
            let line = line.map_err(|error| error.to_string())?;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split('\t');
            let [Some(id), Some(form), Some(lemma), Some(pos), _, Some(features)] =
                std::array::from_fn(|_| fields.next())
            else {
                return Err("Неповний запис у корпусі CoNLL-U.".into());
            };
            if id.parse::<u32>().is_err()
                || !form.chars().any(char::is_alphabetic)
                || lemma == "_"
                || matches!(pos, "_" | "X" | "PUNCT" | "SYM")
            {
                continue;
            }
            insert
                .execute(params![
                    word_key(form),
                    lemma,
                    pos,
                    if features == "_" { "" } else { features }
                ])
                .map_err(|error| error.to_string())?;
            inserted += 1;
        }
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
        let found = store.lookup_word("fr", "CAFE\u{301}").unwrap().unwrap();
        assert_eq!(found.total_observations, 3);
        assert_eq!(found.analyses[0].lemma, "café");
        assert_eq!(found.analyses[0].observations, 2);
        assert_eq!(found.analyses[1].part_of_speech, "PROPN");
        assert!(store.lookup_word("en", "café").unwrap().is_none());
        assert!(store
            .lookup_word("fr", "unknown")
            .unwrap()
            .unwrap()
            .analyses
            .is_empty());
    }
}
