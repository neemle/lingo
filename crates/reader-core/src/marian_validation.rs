use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
    time::SystemTime,
};
use unicode_normalization::UnicodeNormalization;

use super::{contains_book_term, Package, TranslationPackageValidationResult, ASSETS};
use crate::model_storage::{model_error, package_directory};
use crate::Result;

const LEGACY_FILE_NAME: &str = ".probe-validation.json";
const VERSION: u32 = 2;
const MAX_BYTES: u64 = 8192;
const MAX_OUTPUT_BYTES: usize = 1000;

#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct AssetStamp {
    bytes: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
}

pub(super) type Fingerprint = [AssetStamp; ASSETS.len()];

#[derive(Serialize, Deserialize)]
struct Evidence {
    version: u32,
    revision: String,
    input: String,
    output: String,
    #[serde(default)]
    additional_outputs: Vec<String>,
    fingerprint: Fingerprint,
}

pub(super) fn samples(language: &str) -> Option<[&'static str; 4]> {
    Some(match language {
        "en" => ["I am reading a book.", "The windows are open.", "I left the keys on the table.", "We will arrive before noon."],
        "uk" => ["Я читаю книжку.", "Вікна відчинені.", "Я залишив ключі на столі.", "Ми приїдемо до полудня."],
        "fr" => ["Je lis un livre.", "Les fenêtres sont ouvertes.", "J’ai laissé les clés sur la table.", "Nous arriverons avant midi."],
        "de" => ["Ich lese ein Buch.", "Die Fenster sind offen.", "Ich habe die Schlüssel auf dem Tisch liegen lassen.", "Wir werden vor Mittag ankommen."],
        "es" => ["Estoy leyendo un libro.", "Las ventanas están abiertas.", "Dejé las llaves en la mesa.", "Llegaremos antes del mediodía."],
        "it" => ["Sto leggendo un libro.", "Le finestre sono aperte.", "Ho lasciato le chiavi sul tavolo.", "Arriveremo prima di mezzogiorno."],
        "nl" => ["Ik lees een boek.", "De ramen staan open.", "Ik heb de sleutels op tafel laten liggen.", "We komen voor het middaguur aan."],
        "cs" => ["Čtu knihu.", "Okna jsou otevřená.", "Nechal jsem klíče na stole.", "Přijedeme před polednem."],
        "sv" => ["Jag läser en bok.", "Fönstren är öppna.", "Jag lämnade nycklarna på bordet.", "Vi kommer fram före klockan tolv."],
        "pl" => ["Czytam książkę.", "Okna są otwarte.", "Zostawiłem klucze na stole.", "Przyjedziemy przed południem."],
        "pt" => ["Estou a ler um livro.", "As janelas estão abertas.", "Deixei as chaves em cima da mesa.", "Chegaremos antes do meio-dia."],
        "ro" => ["Citesc o carte.", "Ferestrele sunt deschise.", "Am lăsat cheile pe masă.", "Vom ajunge înainte de ora douăsprezece."],
        _ => return None,
    })
}

pub(super) fn score(target: &str, outputs: &[String]) -> Option<u16> {
    let references = samples(target)?;
    if outputs.len() != references.len()
        || outputs.iter().any(|text| {
            text.len() > MAX_OUTPUT_BYTES || !text.chars().any(char::is_alphanumeric)
        })
    {
        return None;
    }
    let total: usize = outputs.iter().zip(references).map(|(output, reference)| {
        let output = normalized_chars(output);
        let reference = normalized_chars(reference);
        (1..=3).map(|n| {
            let mut expected = HashMap::new();
            for grams in reference.windows(n) {
                *expected.entry(grams).or_insert(0usize) += 1;
            }
            let mut common = 0;
            for grams in output.windows(n) {
                if let Some(count) = expected.get_mut(grams).filter(|count| **count > 0) {
                    *count -= 1;
                    common += 1;
                }
            }
            let gram_count = reference.windows(n).len() + output.windows(n).len();
            2000 * common / gram_count.max(1)
        }).sum::<usize>()
    }).sum();
    Some((total / (outputs.len() * 3)) as u16)
}

fn normalized_chars(text: &str) -> Vec<char> {
    text.nfc()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(super) fn fingerprint(directory: &Path, package: &Package) -> Option<Fingerprint> {
    if !package_directory(directory).ok()? {
        return None;
    }
    ASSETS
        .iter()
        .zip(package.sizes)
        .map(|(name, bytes)| {
            let meta = fs::symlink_metadata(directory.join(name)).ok()?;
            if !meta.is_file() || meta.len() != bytes {
                return None;
            }
            Some(AssetStamp {
                bytes,
                modified: meta.modified().ok()?,
                created: meta.created().ok(),
            })
        })
        .collect::<Option<Vec<_>>>()?
        .try_into()
        .ok()
}

pub(super) fn read(
    directory: &Path,
    package: &Package,
) -> Option<TranslationPackageValidationResult> {
    if !package_directory(directory).ok()? {
        return None;
    }
    let file = match open_evidence(&directory.join(evidence_file_name(package.source, package.target))) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            open_evidence(&directory.join(LEGACY_FILE_NAME))
        }
        result => result,
    }.ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    let evidence: Evidence = serde_json::from_slice(&bytes).ok()?;
    let [input, ..] = samples(package.source)?;
    if evidence.revision != package.revision
        || evidence.input != input
        || evidence.fingerprint != fingerprint(directory, package)?
    {
        return None;
    }
    let quality = match evidence.version {
        1 if matches!(package.source, "en" | "fr" | "uk")
            && matches!(package.target, "en" | "fr" | "uk")
            && contains_book_term(package.target, &evidence.output) => None,
        VERSION => {
            if !contains_book_term(package.target, &evidence.output) {
                return None;
            }
            let outputs = std::iter::once(evidence.output.clone())
                .chain(evidence.additional_outputs)
                .collect::<Vec<_>>();
            Some(score(package.target, &outputs)?)
        }
        _ => return None,
    };
    Some(TranslationPackageValidationResult {
        source: package.source,
        target: package.target,
        repository: package.repository,
        revision: package.revision,
        input,
        output: evidence.output,
        quality,
    })
}

pub(super) fn write(
    directory: &Path,
    validation: &TranslationPackageValidationResult,
    additional_outputs: Vec<String>,
    fingerprint: Fingerprint,
) -> Result<()> {
    let evidence = Evidence {
        version: VERSION,
        revision: validation.revision.into(),
        input: validation.input.into(),
        output: validation.output.clone(),
        additional_outputs,
        fingerprint,
    };
    let bytes = serde_json::to_vec(&evidence).map_err(model_error)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Результат пробного перекладу завеликий.".into());
    }
    let mut stage = tempfile::NamedTempFile::new_in(directory).map_err(model_error)?;
    stage.write_all(&bytes).map_err(model_error)?;
    stage.as_file().sync_all().map_err(model_error)?;
    stage
        .persist(directory.join(evidence_file_name(validation.source, validation.target)))
        .map_err(model_error)?;
    Ok(())
}

fn evidence_file_name(source: &str, target: &str) -> String {
    format!(".probe-validation-{source}-{target}.json")
}

#[cfg(unix)]
fn open_evidence(path: &Path) -> io::Result<File> {
    use rustix::fs::{open, Mode, OFlags};

    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    Ok(File::from(open(path, flags, Mode::empty())?))
}

#[cfg(windows)]
fn open_evidence(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}
