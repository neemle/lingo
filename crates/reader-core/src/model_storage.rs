use crate::Result;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Read},
    path::Path,
};

pub(crate) fn write_verified_package_zip<'a>(
    path: &Path,
    limit: u64,
    assets: impl IntoIterator<Item = &'a str>,
    mut copy: impl FnMut(usize, &str, &mut dyn io::Write) -> Result<()>,
) -> Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or("Виберіть теку для архіву моделі.")?;
    crate::space::ensure_capacity([(parent, limit)])?;
    let mut output = tempfile::NamedTempFile::new_in(parent).map_err(model_error)?;
    let mut zip = zip::ZipWriter::new(output.as_file_mut());
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (index, asset) in assets.into_iter().enumerate() {
        zip.start_file(asset, options).map_err(model_error)?;
        copy(index, asset, &mut zip)?;
    }
    let file = zip.finish().map_err(model_error)?;
    if file.metadata().map_err(model_error)?.len() > limit {
        return Err("Архів моделі завеликий.".into());
    }
    file.sync_all().map_err(model_error)?;
    output.persist_noclobber(path).map_err(model_error)?;
    Ok(())
}

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

/// Activate a caller-verified recovery copy and retain the replaced installation.
/// The caller must hold the package's exclusive lock throughout this operation.
pub(crate) fn restore_verified_package(candidate: &Path, active: &Path) -> Result<()> {
    if !package_directory(candidate)? {
        return Err("Збережена копія моделі відсутня.".into());
    }
    let had_active = package_directory(active)?;
    if !had_active {
        return fs::rename(candidate, active).map_err(model_error);
    }
    let root = active.parent().ok_or("Некоректне сховище моделі.")?;
    let holding = tempfile::Builder::new()
        .prefix(".model-recovery-")
        .tempdir_in(root)
        .map_err(model_error)?
        .keep();
    let retained = holding.join("installed");
    if let Err(error) = fs::rename(active, &retained) {
        let _ = fs::remove_dir(&holding);
        return Err(model_error(error));
    }
    if let Err(error) = fs::rename(candidate, active) {
        let restored = fs::rename(&retained, active);
        if restored.is_ok() {
            let _ = fs::remove_dir(&holding);
        }
        return Err(format!(
            "Не вдалося відновити модель: {error}. Попередні файли збережено: {}",
            if restored.is_ok() { active } else { &retained }.display()
        ));
    }
    fs::rename(&retained, candidate).map_err(|error| {
        format!(
            "Модель відновлено. Замінені файли збережено в {}: {error}",
            retained.display()
        )
    })?;
    let _ = fs::remove_dir(&holding);
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
    input: impl Read,
    output: impl io::Write,
    limit: u64,
) -> Result<(u64, String)> {
    copy_and_hash_checked(input, output, limit, &|| Ok(()))
}

pub(crate) fn copy_and_hash_checked(
    mut input: impl Read,
    mut output: impl io::Write,
    limit: u64,
    checkpoint: &impl Fn() -> Result<()>,
) -> Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut size = 0;
    loop {
        checkpoint()?;
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
