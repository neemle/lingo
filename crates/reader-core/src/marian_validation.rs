use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
    time::SystemTime,
};

use super::{contains_book_term, probe_input, Package, TranslationPackageValidationResult, ASSETS};
use crate::model_storage::{model_error, package_directory};
use crate::Result;

const FILE_NAME: &str = ".probe-validation.json";
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 8192;

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
    fingerprint: Fingerprint,
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
    let file = open_evidence(&directory.join(FILE_NAME)).ok()?;
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
    let input = probe_input(package.source)?;
    if evidence.version != VERSION
        || evidence.revision != package.revision
        || evidence.input != input
        || !contains_book_term(package.target, &evidence.output)
        || evidence.fingerprint != fingerprint(directory, package)?
    {
        return None;
    }
    Some(TranslationPackageValidationResult {
        source: package.source,
        target: package.target,
        repository: package.repository,
        revision: package.revision,
        input,
        output: evidence.output,
    })
}

pub(super) fn write(
    directory: &Path,
    validation: &TranslationPackageValidationResult,
    fingerprint: Fingerprint,
) -> Result<()> {
    let evidence = Evidence {
        version: VERSION,
        revision: validation.revision.into(),
        input: validation.input.into(),
        output: validation.output.clone(),
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
        .persist(directory.join(FILE_NAME))
        .map_err(model_error)?;
    Ok(())
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
