use super::{
    install_optional_model_candidate, model, probe_optional_model, verify_asset, Model, ModelPaths,
};
use crate::{
    model_storage::{copy_and_hash, lock_package, model_error, package_directory},
    space::{ensure_capacity, PACKAGE_OVERHEAD},
    Result, Store,
};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};
use zip::{
    read::{ArchiveOffset, Config},
    write::SimpleFileOptions,
    CompressionMethod, ZipArchive, ZipWriter,
};

const EOCD_SIZE: usize = 22;

pub struct OptionalModelArchiveJob {
    model: &'static Model,
    root: PathBuf,
}

impl Store {
    pub fn prepare_optional_model_archive(&self, id: &str) -> Result<OptionalModelArchiveJob> {
        Ok(OptionalModelArchiveJob {
            model: model(id)?,
            root: self.optional_model_directory.clone(),
        })
    }
}

impl OptionalModelArchiveJob {
    pub fn export(self, path: &Path) -> Result<()> {
        export(&self.root, self.model, path)
    }

    pub fn install(self, path: &Path) -> Result<()> {
        install_optional_model_candidate(
            &self.root,
            self.model,
            |destination| stage_archive(self.model, path, destination),
            |directory| probe_optional_model(self.model, directory),
        )
    }
}

fn byte_limit(model: &Model) -> u64 {
    model.assets.iter().map(|asset| asset.bytes).sum::<u64>() + PACKAGE_OVERHEAD
}

fn export(root: &Path, model: &Model, path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or("Виберіть теку для архіву моделі.")?;
    let paths = ModelPaths::new(root, model.id);
    if !package_directory(root)? {
        return Err("Модель відсутня.".into());
    }
    let _gate = lock_package(&paths.lock, false)?;
    let installed = paths.installed();
    if !package_directory(installed)? {
        return Err("Модель відсутня.".into());
    }

    ensure_capacity([(parent, byte_limit(model))])?;
    let mut output = tempfile::NamedTempFile::new_in(parent).map_err(model_error)?;
    let mut zip = ZipWriter::new(output.as_file_mut());
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for asset in model.assets {
        zip.start_file(asset.name, options).map_err(model_error)?;
        verify_asset(&installed.join(asset.name), asset, &mut zip)?;
    }
    let file = zip.finish().map_err(model_error)?;
    if file.metadata().map_err(model_error)?.len() > byte_limit(model) {
        return Err("Архів моделі завеликий.".into());
    }
    file.sync_all().map_err(model_error)?;
    output.persist_noclobber(path).map_err(model_error)?;
    Ok(())
}

fn stage_archive(model: &Model, path: &Path, destination: &Path) -> Result<()> {
    if !fs::symlink_metadata(path).map_err(model_error)?.is_file() {
        return Err("Виберіть звичайний ZIP-файл моделі.".into());
    }
    let mut file = File::open(path).map_err(model_error)?;
    check_directory(&mut file, model)?;
    let config = Config {
        archive_offset: ArchiveOffset::Known(0),
    };
    let mut zip = ZipArchive::with_config(config, file).map_err(model_error)?;
    if zip.len() != model.assets.len() {
        return Err("Вміст ZIP не відповідає вибраній моделі.".into());
    }
    for asset in model.assets {
        let entry = zip.by_name(asset.name).map_err(model_error)?;
        if !entry.is_file()
            || entry.is_symlink()
            || entry.compression() != CompressionMethod::Stored
            || entry.size() != asset.bytes
            || entry.compressed_size() != asset.bytes
        {
            return Err(format!("Некоректний файл в архіві моделі: {}", asset.name));
        }
        let mut output = File::create(destination.join(asset.name)).map_err(model_error)?;
        let (length, digest) = copy_and_hash(entry, &mut output, asset.bytes)?;
        if length != asset.bytes || digest != asset.sha256 {
            return Err(format!("Файл моделі пошкоджений: {}", asset.name));
        }
        output.sync_all().map_err(model_error)?;
    }
    Ok(())
}

fn check_directory(file: &mut File, model: &Model) -> Result<()> {
    let bytes = file.metadata().map_err(model_error)?.len();
    let invalid = || "Некоректний ZIP моделі. Виберіть архів, збережений у Читай.".to_owned();
    let footer_at = bytes.checked_sub(EOCD_SIZE as u64).ok_or_else(invalid)?;
    if bytes > byte_limit(model) {
        return Err("Архів моделі завеликий.".into());
    }

    // Require a classic, single-disk footer before letting ZIP parse its metadata.
    file.seek(SeekFrom::Start(footer_at)).map_err(model_error)?;
    let mut footer = [0; EOCD_SIZE];
    file.read_exact(&mut footer).map_err(model_error)?;
    let count = |at| u16::from_le_bytes([footer[at], footer[at + 1]]) as usize;
    let word = |at| {
        u64::from(u32::from_le_bytes([
            footer[at],
            footer[at + 1],
            footer[at + 2],
            footer[at + 3],
        ]))
    };
    let size = word(12);
    let offset = word(16);
    if footer[..8] != *b"PK\x05\x06\0\0\0\0"
        || footer[20..] != [0; 2]
        || count(8) != model.assets.len()
        || count(10) != model.assets.len()
        || !(1..=PACKAGE_OVERHEAD).contains(&size)
        || offset + size != footer_at
    {
        return Err(invalid());
    }
    // If the final footer points at junk, do not let the library fall back to an embedded footer.
    file.seek(SeekFrom::Start(offset)).map_err(model_error)?;
    let mut central_magic = [0; 4];
    file.read_exact(&mut central_magic).map_err(model_error)?;
    if central_magic != *b"PK\x01\x02" {
        return Err(invalid());
    }
    file.rewind().map_err(model_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{optional_models::Asset, resources::ModelBudget};

    const FIXTURE: Model = Model {
        id: "fixture",
        name: "Fixture",
        license: "MIT",
        repo: "",
        revision: "v1",
        budget: ModelBudget {
            working_mib: 0,
            minimum_total_mib: 0,
        },
        assets: &[Asset {
            name: "model.onnx",
            remote_path: "",
            bytes: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        }],
    };

    #[test]
    fn portable_archive_verifies_content_and_never_publishes_a_partial_export() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let installed = root.join(FIXTURE.id);
        fs::create_dir(&installed).unwrap();
        let asset = installed.join(FIXTURE.assets[0].name);
        fs::write(&asset, b"abc").unwrap();

        let archive = root.join("model.zip");
        export(root, &FIXTURE, &archive).unwrap();
        assert!(export(root, &FIXTURE, &archive).is_err());

        let destination = root.join("candidate");
        fs::create_dir(&destination).unwrap();
        stage_archive(&FIXTURE, &archive, &destination).unwrap();
        assert_eq!(fs::read(destination.join("model.onnx")).unwrap(), b"abc");

        let mut malformed = fs::read(&archive).unwrap();
        let counts = malformed.len() - EOCD_SIZE + 8;
        malformed[counts..counts + 4].fill(0xff);
        fs::write(&archive, malformed).unwrap();
        assert!(stage_archive(&FIXTURE, &archive, &destination).is_err());

        fs::write(&asset, b"xyz").unwrap();
        let corrupt_export = root.join("corrupt.zip");
        assert!(export(root, &FIXTURE, &corrupt_export).is_err());
        assert!(!corrupt_export.exists());

        let altered = File::create(&archive).unwrap();
        let mut zip = ZipWriter::new(altered);
        zip.start_file(
            "model.onnx",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
        std::io::Write::write_all(&mut zip, b"xyz").unwrap();
        zip.finish().unwrap();
        assert!(stage_archive(&FIXTURE, &archive, &destination).is_err());
    }
}
