use crate::Result;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Read},
    path::Path,
};

pub(crate) fn package_directory(path: &Path) -> Result<bool> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err("Некоректний каталог мовного пакета.".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(model_error(error)),
    }
}

pub(crate) fn activate_verified_package(
    stage: &Path,
    active: &Path,
    previous: &Path,
    discarded: &Path,
) -> Result<()> {
    let had_active = package_directory(active)?;
    let mut rollback = previous;
    if had_active {
        if package_directory(previous)? {
            remove_package_directory(discarded)?;
            rollback = discarded;
        }
        fs::rename(active, rollback).map_err(model_error)?;
    }
    if let Err(error) = fs::rename(stage, active) {
        if had_active {
            fs::rename(rollback, active).map_err(|restore_error| {
                format!("Не вдалося встановити пакет. Попередні файли збережено. {error}. {restore_error}")
            })?;
        }
        return Err(model_error(error));
    }
    Ok(())
}

pub(crate) fn remove_package_directory(path: &Path) -> Result<bool> {
    if !package_directory(path)? {
        return Ok(false);
    }
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound && !path.exists() => Ok(true),
        Err(error) => Err(model_error(error)),
    }
}
pub(crate) fn copy_and_hash(
    mut input: impl Read,
    mut output: impl io::Write,
    limit: u64,
) -> Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut size = 0;
    loop {
        let count = input.read(&mut buffer).map_err(model_error)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > limit {
            return Err("Файл мовного пакета завеликий".into());
        }
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count]).map_err(model_error)?;
    }
    output.flush().map_err(model_error)?;
    Ok((size, format!("{:x}", hash.finalize())))
}

pub(crate) fn model_error(error: impl std::fmt::Display) -> String {
    format!("Помилка локального мовного пакета: {error}")
}

pub(crate) fn lock_package(path: &Path, exclusive: bool) -> Result<File> {
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(model_error)?;
    if exclusive {
        lock.lock()
    } else {
        lock.lock_shared()
    }
    .map_err(model_error)?;
    Ok(lock)
}
