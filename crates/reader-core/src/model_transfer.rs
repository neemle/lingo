use crate::{
    file_originals::{ensure_root, sync_directory, verify_root},
    marian_provider::{self, TranslationPackageFiles},
    model_storage::{copy_and_hash, lock_package, model_error},
    optional_models::{self, OptionalModelInstallJob},
    space::{ensure_capacity, PACKAGE_OVERHEAD},
    Result, Store,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

const CHUNK_BYTES: u64 = 16 * 1024 * 1024;
const PAUSED: &str = ".paused";

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ModelPackage {
    Optional { id: String },
    Translation { source: String, target: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum TransferAction {
    Status,
    Resume,
    Pause,
    Discard,
    AdoptPrefix { asset: String, path: PathBuf },
    AcceptRange {
        asset: String,
        path: PathBuf,
        status: u16,
        from: u64,
        to: u64,
        total: u64,
    },
    Activate,
}

pub(crate) struct DownloadManifest {
    pub revision: &'static str,
    pub assets: Vec<DownloadAsset>,
}

pub(crate) struct DownloadAsset {
    pub name: &'static str,
    pub url: String,
    pub size: u64,
    pub sha256: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferPlan {
    pub revision: &'static str,
    pub status: TransferStatus,
    pub received_bytes: u64,
    pub total_bytes: u64,
    pub next_range: Option<TransferRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferStatus { Idle, Paused, AwaitingRange, ReadyToInstall, Installed }

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferRange {
    pub asset: &'static str,
    pub url: String,
    pub from: u64,
    pub to: u64,
    pub total: u64,
}

enum Installer {
    Optional(OptionalModelInstallJob),
    Translation { files: TranslationPackageFiles, source: String, target: String },
}

pub struct ModelTransfer {
    root: PathBuf,
    stage: PathBuf,
    destination: PathBuf,
    manifest: DownloadManifest,
    installer: Installer,
}

impl Store {
    pub fn prepare_model_transfer(&self, package: ModelPackage) -> Result<ModelTransfer> {
        let root = self.optional_model_directory.with_file_name("model-downloads");
        let (manifest, destination, installer) = match package {
            ModelPackage::Optional { id } => (
                optional_models::download_manifest(&id)?,
                self.optional_model_directory.clone(),
                Installer::Optional(self.prepare_optional_model_install(&id, &root)?),
            ),
            ModelPackage::Translation { source, target } => (
                marian_provider::download_manifest(&source, &target)?,
                self.model_directory.clone(),
                Installer::Translation { files: self.translation_package_files(), source, target },
            ),
        };
        let stage = root.join(manifest.revision);
        Ok(ModelTransfer { root, stage, destination, manifest, installer })
    }
}

impl ModelTransfer {
    pub fn run(self, action: TransferAction) -> Result<TransferPlan> {
        if matches!(&action, TransferAction::Status) && !self.root.exists() {
            return Ok(self.inactive(TransferStatus::Idle));
        }
        ensure_root(&self.root)?;
        let _lock = lock_package(&self.root.join(format!("{}.lock", self.manifest.revision)), true)?;
        if !self.stage.exists() && !matches!(&action, TransferAction::Resume) {
            return match action {
                TransferAction::Status | TransferAction::Pause | TransferAction::Discard =>
                    Ok(self.inactive(TransferStatus::Idle)),
                _ => Err("Спочатку розпочніть завантаження пакета.".into()),
            };
        }
        match action {
            TransferAction::Status => self.plan(),
            TransferAction::Resume => {
                ensure_root(&self.stage)?;
                ensure_root(&self.destination)?;
                let plan = self.plan()?;
                ensure_capacity([
                    (&self.stage, plan.total_bytes - plan.received_bytes),
                    (&self.destination, plan.total_bytes + PACKAGE_OVERHEAD),
                ])?;
                match fs::remove_file(self.stage.join(PAUSED)) {
                    Ok(()) => sync_directory(&self.stage)?,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(model_error(error)),
                }
                self.plan()
            }
            TransferAction::Pause => {
                verify_root(&self.stage)?;
                OpenOptions::new().write(true).create(true).truncate(false).open(self.stage.join(PAUSED))
                    .and_then(|file| file.sync_all()).map_err(model_error)?;
                sync_directory(&self.stage)?;
                self.plan()
            }
            TransferAction::Discard => {
                verify_root(&self.stage)?;
                fs::remove_dir_all(&self.stage).map_err(model_error)?;
                sync_directory(&self.root)?;
                Ok(self.inactive(TransferStatus::Idle))
            }
            TransferAction::AdoptPrefix { asset, path } => {
                let next = self.plan()?.next_range.ok_or("Пакет зараз не приймає дані.")?;
                if next.from != 0 || next.asset != asset.as_str() {
                    return Err("Новий кеш уже містить дані для цього файлу.".into());
                }
                let asset = self.manifest.assets.iter().find(|asset| asset.name == next.asset)
                    .ok_or("Файл відсутній у каталозі.")?;
                let length = file_length(&path)?.unwrap_or(0);
                if (1..=asset.size).contains(&length) {
                    self.append(asset, &path, 0, length)?;
                    if length == asset.size {
                        promote(&self.stage, asset)?;
                    }
                }
                self.plan()
            }
            TransferAction::AcceptRange { asset, path, status, from, to, total } => {
                let plan = self.plan()?;
                let next = plan.next_range.ok_or("Пакет зараз не приймає дані.")?;
                if (asset.as_str(), from, to, total) != (next.asset, next.from, next.to, next.total)
                    || !(status == 206 || status == 200 && from == 0 && to + 1 == total)
                {
                    return Err("Відповідь транспорту не відповідає запитаному діапазону.".into());
                }
                let asset = self.manifest.assets.iter().find(|asset| asset.name == next.asset)
                    .ok_or("Файл відсутній у каталозі.")?;
                self.append(asset, &path, from, to + 1 - from)?;
                if to + 1 == total && !promote(&self.stage, asset)? {
                    return Err(format!("Контрольна сума файлу {} не збігається.", asset.name));
                }
                self.plan()
            }
            TransferAction::Activate => {
                let plan = self.plan()?;
                if plan.status != TransferStatus::ReadyToInstall {
                    return Err("Завершіть завантаження всіх файлів пакета.".into());
                }
                match self.installer {
                    Installer::Optional(job) => job.run()?,
                    Installer::Translation { files, source, target } =>
                        files.install_downloaded(&source, &target, &self.stage)?,
                }
                let _ = fs::remove_dir_all(&self.stage);
                Ok(TransferPlan { status: TransferStatus::Installed, ..plan })
            }
        }
    }

    fn inactive(&self, status: TransferStatus) -> TransferPlan {
        TransferPlan {
            revision: self.manifest.revision, status, received_bytes: 0,
            total_bytes: self.manifest.assets.iter().map(|asset| asset.size).sum(),
            next_range: None,
        }
    }

    fn plan(&self) -> Result<TransferPlan> {
        verify_root(&self.stage)?;
        let paused = self.stage.join(PAUSED).exists();
        let mut plan = self.inactive(TransferStatus::ReadyToInstall);
        for asset in &self.manifest.assets {
            let path = self.stage.join(asset.name);
            if let Some(length) = file_length(&path)? {
                if length == asset.size {
                    plan.received_bytes += length;
                    continue;
                }
                fs::remove_file(path).map_err(model_error)?;
            }
            let partial = part_path(&self.stage, asset);
            let mut from = file_length(&partial)?.unwrap_or(0);
            if from == asset.size && promote(&self.stage, asset)? {
                plan.received_bytes += from;
                continue;
            }
            if from >= asset.size {
                if file_length(&partial)?.is_some() {
                    fs::remove_file(partial).map_err(model_error)?;
                }
                from = 0;
            }
            plan.received_bytes += from;
            if plan.next_range.is_none() {
                plan.next_range = Some(TransferRange {
                    asset: asset.name,
                    url: asset.url.clone(),
                    from,
                    to: (from + CHUNK_BYTES).min(asset.size) - 1,
                    total: asset.size,
                });
            }
        }
        if paused {
            plan.status = TransferStatus::Paused;
            plan.next_range = None;
        } else if plan.next_range.is_some() {
            plan.status = TransferStatus::AwaitingRange;
        }
        Ok(plan)
    }

    fn append(&self, asset: &DownloadAsset, source: &Path, from: u64, length: u64) -> Result<()> {
        if file_length(source)? != Some(length) {
            return Err("Розмір отриманого діапазону не збігається.".into());
        }
        let input = File::open(source).map_err(model_error)?;
        if !input.metadata().map_err(model_error)?.is_file() {
            return Err("Відповідь транспорту має бути звичайним файлом.".into());
        }
        let partial = part_path(&self.stage, asset);
        let mut output = OpenOptions::new().write(true).create(true).truncate(false).open(partial)
            .map_err(model_error)?;
        if output.seek(SeekFrom::End(0)).map_err(model_error)? != from {
            return Err("Позиція завантаження змінилася. Оновіть план.".into());
        }
        let copy = (|| {
            if io::copy(&mut input.take(length + 1), &mut output).map_err(model_error)? != length {
                return Err("Розмір отриманого діапазону змінився.".into());
            }
            output.sync_all().map_err(model_error)
        })();
        if copy.is_err() {
            output.set_len(from).and_then(|_| output.sync_all()).map_err(model_error)?;
        }
        copy
    }
}

fn part_path(stage: &Path, asset: &DownloadAsset) -> PathBuf {
    stage.join(format!(".{}.part", asset.name))
}

fn promote(stage: &Path, asset: &DownloadAsset) -> Result<bool> {
    let part = part_path(stage, asset);
    let (size, digest) = copy_and_hash(File::open(&part).map_err(model_error)?, io::sink(), asset.size)?;
    if size != asset.size || digest != asset.sha256 {
        fs::remove_file(part).map_err(model_error)?;
        return Ok(false);
    }
    fs::rename(part, stage.join(asset.name)).map_err(model_error)?;
    sync_directory(stage)?;
    Ok(true)
}

fn file_length(path: &Path) -> Result<Option<u64>> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata.len())),
        Ok(_) => Err("Дані пакета мають бути звичайним файлом.".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(model_error(error)),
    }
}
