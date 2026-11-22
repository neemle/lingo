use std::{
    collections::HashMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, PoisonError},
};

use super::{verify_inference_assets, Model, Result};

#[derive(PartialEq, Eq)]
struct FileStamp {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

struct VerifiedPackage {
    directory: PathBuf,
    revision: &'static str,
    files: Vec<FileStamp>,
}

// The catalog bounds this map to one entry per supported model.
static VERIFIED: LazyLock<Mutex<HashMap<&'static str, VerifiedPackage>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn verify_for_inference(
    directory: &Path,
    model: &Model,
    checkpoint: &impl Fn() -> Result<()>,
) -> Result<()> {
    // The caller holds the package lock throughout verification and inference.
    let Some(before) = file_stamps(directory, model) else {
        return verify_inference_assets(directory, model, checkpoint);
    };
    if VERIFIED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(model.id)
        .is_some_and(|package| {
            package.directory == directory
                && package.revision == model.revision
                && package.files == before
        })
    {
        return Ok(());
    }

    verify_inference_assets(directory, model, checkpoint)?;
    if file_stamps(directory, model).as_ref() != Some(&before) {
        return Err("Файли моделі змінилися під час перевірки.".into());
    }
    VERIFIED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            model.id,
            VerifiedPackage {
                directory: directory.to_owned(),
                revision: model.revision,
                files: before,
            },
        );
    Ok(())
}

fn file_stamps(directory: &Path, model: &Model) -> Option<Vec<FileStamp>> {
    model
        .assets
        .iter()
        .map(|asset| {
            let metadata = fs::symlink_metadata(directory.join(asset.name)).ok()?;
            if !metadata.is_file() || metadata.len() != asset.bytes {
                return None;
            }
            Some(FileStamp {
                device: metadata.dev(),
                inode: metadata.ino(),
                length: metadata.len(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                changed: (metadata.ctime(), metadata.ctime_nsec()),
            })
        })
        .collect()
}
