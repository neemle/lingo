use crate::{
    model_storage::{
        activate_verified_package, copy_and_hash_checked, lock_package, model_error, package_directory,
        remove_package_directory, restore_verified_package,
    },
    resources::{InferencePriority, MemoryReadiness, ModelBudget, RESOURCES},
    space::{ensure_capacity, PACKAGE_OVERHEAD},
    Result, Store,
};
use serde::Serialize;
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[path = "optional_model_archive.rs"]
mod archive;
pub use archive::OptionalModelArchiveJob;

#[cfg(any(target_os = "macos", target_abi = "macabi"))]
#[path = "optional_model_verification.rs"]
mod verification;

struct Model {
    id: &'static str,
    name: &'static str,
    license: &'static str,
    repo: &'static str,
    revision: &'static str,
    budget: ModelBudget,
    assets: &'static [Asset],
}

struct Asset {
    name: &'static str,
    remote_path: &'static str,
    bytes: u64,
    sha256: &'static str,
}

const RETIRED_MODEL: &str = "qwen-0.8b";

// Pins are part of the application; downloaded files cannot update this catalogue.
const MODELS: &[Model] = &[
    Model {
        id: RETIRED_MODEL,
        name: "Qwen3.5-0.8B",
        license: "Apache-2.0",
        repo: "ggml-org/Qwen3.5-0.8B-GGUF",
        revision: "004fd086eccea9f7cf867aa07ba328773beef81d",
        budget: ModelBudget {
            working_mib: 1536,
            minimum_total_mib: 0,
        },
        assets: &[Asset {
            name: "model.gguf",
            remote_path: "Qwen3.5-0.8B-Q4_0.gguf",
            bytes: 563_036_064,
            sha256: "57d1997790d1744fba5b40a7317df71ea5e2acee28c47e78f0cce39c0703f8cf",
        }],
    },
    Model {
        id: "qwen-2b",
        name: "Qwen3.5-2B",
        license: "Apache-2.0",
        repo: "unsloth/Qwen3.5-2B-GGUF",
        revision: "f6d5376be1edb4d416d56da11e5397a961aca8ae",
        budget: crate::qwen::CPU_BUDGET,
        assets: &[Asset {
            name: "model.gguf",
            remote_path: "Qwen3.5-2B-Q4_K_M.gguf",
            bytes: 1_280_835_840,
            sha256: "aaf42c8b7c3cab2bf3d69c355048d4a0ee9973d48f16c731c0520ee914699223",
        }],
    },
    Model {
        id: "multilingual-e5-small",
        name: "Multilingual E5 Small",
        license: "MIT",
        repo: "intfloat/multilingual-e5-small",
        revision: "614241f622f53c4eeff9890bdc4f31cfecc418b3",
        budget: ModelBudget {
            working_mib: 1536,
            minimum_total_mib: 0,
        },
        assets: &[
            Asset {
                name: "model.onnx",
                remote_path: "onnx/model.onnx",
                bytes: 470_268_510,
                sha256: "ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665",
            },
            Asset {
                name: "tokenizer.json",
                remote_path: "onnx/tokenizer.json",
                bytes: 17_082_730,
                sha256: "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
            },
        ],
    },
    Model {
        id: "piper-uk",
        name: "Українські голоси Piper",
        license: "MIT",
        repo: "rhasspy/piper-voices",
        revision: "e9c8a39444359388adaa94255becb37a5a2b8e51",
        budget: ModelBudget {
            working_mib: 384,
            minimum_total_mib: 0,
        },
        assets: &[
            Asset {
                name: "piper-uk.onnx",
                remote_path: "uk/uk_UA/ukrainian_tts/medium/uk_UA-ukrainian_tts-medium.onnx",
                bytes: 76_735_663,
                sha256: "7920419ac5f6fd8b6450520f24b52ed5a319cb53dd018fbcd71c9e079cbac84f",
            },
            Asset {
                name: "piper-uk.json",
                remote_path: "uk/uk_UA/ukrainian_tts/medium/uk_UA-ukrainian_tts-medium.onnx.json",
                bytes: 2_002,
                sha256: "4e96e72917ca9b94edc77d6ccfee03a73f450ba2fc1ca93c2e562bc014e5aa55",
            },
        ],
    },
    Model {
        id: "supertonic-3",
        name: "Supertonic 3",
        license: "OpenRAIL",
        repo: "Supertone/supertonic-3",
        revision: "3cadd1ee6394adea1bd021217a0e650ede09a323",
        budget: ModelBudget {
            working_mib: 1536,
            minimum_total_mib: 0,
        },
        assets: &[
            Asset {
                name: "duration_predictor.onnx",
                remote_path: "onnx/duration_predictor.onnx",
                bytes: 3_700_147,
                sha256: "c3eb91414d5ff8a7a239b7fe9e34e7e2bf8a8140d8375ffb14718b1c639325db",
            },
            Asset {
                name: "text_encoder.onnx",
                remote_path: "onnx/text_encoder.onnx",
                bytes: 36_416_150,
                sha256: "c7befd5ea8c3119769e8a6c1486c4edc6a3bc8365c67621c881bbb774b9902ff",
            },
            Asset {
                name: "vector_estimator.onnx",
                remote_path: "onnx/vector_estimator.onnx",
                bytes: 256_534_781,
                sha256: "883ac868ea0275ef0e991524dc64f16b3c0376efd7c320af6b53f5b780d7c61c",
            },
            Asset {
                name: "vocoder.onnx",
                remote_path: "onnx/vocoder.onnx",
                bytes: 101_424_195,
                sha256: "085de76dd8e8d5836d6ca66826601f615939218f90e519f70ee8a36ed2a4c4ba",
            },
            Asset {
                name: "tts.json",
                remote_path: "onnx/tts.json",
                bytes: 8_253,
                sha256: "42078d3aef1cd43ab43021f3c54f47d2d75ceb4e75f627f118890128b06a0d09",
            },
            Asset {
                name: "unicode_indexer.json",
                remote_path: "onnx/unicode_indexer.json",
                bytes: 277_676,
                sha256: "9bf7346e43883a81f8645c81224f786d43c5b57f3641f6e7671a7d6c493cb24f",
            },
            Asset {
                name: "M1.json",
                remote_path: "voice_styles/M1.json",
                bytes: 291_748,
                sha256: "e35604687f5d23694b8e91593a93eec0e4eca6c0b02bb8ed69139ab2ea6b0a5b",
            },
            Asset {
                name: "F1.json",
                remote_path: "voice_styles/F1.json",
                bytes: 292_046,
                sha256: "bbdec6ee00231c2c742ad05483df5334cab3b52fda3ba38e6a07059c4563dbc2",
            },
        ],
    },
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionalModel {
    pub(crate) id: &'static str,
    name: &'static str,
    license: &'static str,
    pub(crate) revision: &'static str,
    pub(crate) files_present: bool,
    has_local_files: bool,
    has_recovery_copy: bool,
    can_generate_language_hints: bool,
    memory: MemoryReadiness,
    assets: Vec<OptionalAsset>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OptionalAsset {
    name: &'static str,
    download_url: String,
    bytes: u64,
    sha256: &'static str,
}

struct ModelPaths {
    active: PathBuf,
    previous: PathBuf,
    removed: PathBuf,
    lock: PathBuf,
}

impl ModelPaths {
    fn new(root: &Path, id: &str) -> Self {
        Self {
            active: root.join(id),
            previous: root.join(format!(".previous-{id}")),
            removed: root.join(format!(".removed-{id}")),
            lock: root.join(format!(".{id}.lock")),
        }
    }

    fn installed(&self) -> &Path {
        if self.active.exists() {
            &self.active
        } else {
            &self.previous
        }
    }
}

fn model(id: &str) -> Result<&'static Model> {
    if id == RETIRED_MODEL {
        return Err("Підтримку Qwen 0.8B припинено. Для підказок доступна Qwen 2B.".into());
    }
    MODELS
        .iter()
        .find(|model| model.id == id)
        .ok_or_else(|| "Невідома додаткова модель.".into())
}

fn local_size(path: &Path) -> Option<u64> {
    fs::symlink_metadata(path)
        .ok()
        .filter(|file| file.is_file())
        .map(|file| file.len())
}

fn verify_asset(path: &Path, asset: &Asset, output: impl Write) -> Result<()> {
    verify_asset_checked(path, asset, output, &|| Ok(()))
}

fn verify_asset_checked(
    path: &Path,
    asset: &Asset,
    output: impl Write,
    checkpoint: &impl Fn() -> Result<()>,
) -> Result<()> {
    let corrupted = || format!("Файл моделі пошкоджений або відсутній: {}", asset.name);
    if local_size(path) != Some(asset.bytes) {
        return Err(corrupted());
    }
    let input = File::open(path).map_err(model_error)?;
    let (bytes, digest) = copy_and_hash_checked(input, output, asset.bytes, checkpoint)?;
    if bytes != asset.bytes || digest != asset.sha256 {
        return Err(corrupted());
    }
    Ok(())
}

/// A pinned model and its storage location, independent of the SQLite connection.
pub struct OptionalModelJob {
    model: &'static Model,
    paths: ModelPaths,
}

impl OptionalModelJob {
    pub(crate) fn revision(&self) -> &'static str {
        self.model.revision
    }

    /// Keep the installed weights stable throughout a bounded inference call.
    /// Providers should check the yield callback between batches and tokens.
    pub fn run<T>(
        self,
        inference: impl FnOnce(&Path, &dyn Fn() -> bool) -> Result<T>,
    ) -> Result<T> {
        let permit = RESOURCES.optional()?;
        self.run_checked(inference, &|| permit.should_yield())
    }

    pub(crate) fn run_requested_with_cache<T>(
        &self,
        canceled: Option<&dyn Fn() -> bool>,
        cached: impl Fn() -> Result<Option<T>>,
        inference: impl FnOnce(&Path, &dyn Fn() -> bool) -> Result<T>,
    ) -> Result<T> {
        if let Some(result) = cached()? {
            return Ok(result);
        }
        let permit = match canceled {
            Some(canceled) => RESOURCES.requested_model_until(canceled).ok_or("Переклад скасовано.")?,
            None => RESOURCES.requested_model(),
        };
        if let Some(result) = cached()? {
            return Ok(result);
        }
        self.run_checked(inference, &|| canceled.is_some_and(|check| check()) || permit.should_yield())
    }

    pub(crate) fn run_cached<T>(
        self,
        priority: InferencePriority,
        cached: impl Fn(&Path) -> Option<T>,
        inference: impl FnOnce(&Path, &dyn Fn() -> bool) -> Result<T>,
    ) -> Result<T> {
        if let Some(audio) = self.read_installed_cache(&cached)? {
            return Ok(audio);
        }
        match priority {
            InferencePriority::Foreground => {
                let _permit = RESOURCES.interactive();
                if let Some(audio) = self.read_installed_cache(cached)? {
                    return Ok(audio);
                }
                self.run_checked(inference, &|| false)
            }
            InferencePriority::Background => {
                let permit = RESOURCES.optional()?;
                if let Some(audio) = self.read_installed_cache(cached)? {
                    return Ok(audio);
                }
                self.run_checked(inference, &|| permit.should_yield())
            }
        }
    }

    fn read_installed_cache<T>(
        &self,
        cached: impl FnOnce(&Path) -> Option<T>,
    ) -> Result<Option<T>> {
        if !package_directory(self.paths.installed())? {
            return Ok(None);
        }
        let _gate = lock_package(&self.paths.lock, false)?;
        let installed = self.paths.installed();
        if !package_directory(installed)?
            || self
                .model
                .assets
                .iter()
                .any(|asset| local_size(&installed.join(asset.name)) != Some(asset.bytes))
        {
            return Ok(None);
        }
        Ok(cached(installed))
    }

    fn run_checked<T>(
        &self,
        inference: impl FnOnce(&Path, &dyn Fn() -> bool) -> Result<T>,
        requested_yield: &dyn Fn() -> bool,
    ) -> Result<T> {
        check_memory(self.model)?;
        let installed = self.paths.installed();
        if !package_directory(installed)? {
            return Err("Модель відсутня.".into());
        }
        let _gate = lock_package(&self.paths.lock, false)?;
        let installed = self.paths.installed();
        let checkpoint = || {
            if requested_yield() {
                return Err("Фонову обробку відкладено для активного запиту.".into());
            }
            Ok(())
        };
        #[cfg(any(target_os = "macos", target_abi = "macabi"))]
        verification::verify_for_inference(installed, self.model, &checkpoint)?;
        #[cfg(not(any(target_os = "macos", target_abi = "macabi")))]
        verify_inference_assets(installed, self.model, &checkpoint)?;
        checkpoint()?;
        inference(installed, requested_yield)
    }
}

fn verify_inference_assets(directory: &Path, model: &Model, checkpoint: &impl Fn() -> Result<()>) -> Result<()> {
    for asset in model.assets {
        verify_asset_checked(&directory.join(asset.name), asset, io::sink(), checkpoint)?;
    }
    Ok(())
}

fn check_memory(model: &Model) -> Result<()> {
    let memory = model.budget.readiness();
    let Some(available) = memory.available_mib else {
        return Err("Не вдалося визначити доступну пам’ять. Модель пропущено.".into());
    };
    if memory
        .total_mib
        .is_some_and(|total| total < memory.minimum_total_mib)
    {
        return Err(format!(
            "Для {} потрібно щонайменше {} МіБ RAM.",
            model.name, memory.minimum_total_mib
        ));
    }
    if !memory.ready {
        return Err(format!(
            "Для {} потрібно {} МіБ доступної пам’яті. Зараз доступно {available} МіБ.",
            model.name, memory.required_mib
        ));
    }
    Ok(())
}

/// A verified local candidate installation independent of the SQLite connection.
pub struct OptionalModelInstallJob {
    model: &'static Model,
    root: PathBuf,
    staging_root: PathBuf,
}

pub(crate) fn download_manifest(id: &str) -> Result<crate::model_transfer::DownloadManifest> {
    let model = model(id)?;
    Ok(crate::model_transfer::DownloadManifest {
        revision: model.revision,
        assets: model.assets.iter().map(|asset| crate::model_transfer::DownloadAsset {
            name: asset.name,
            url: format!("https://huggingface.co/{}/resolve/{}/{}", model.repo, model.revision, asset.remote_path),
            size: asset.bytes,
            sha256: asset.sha256,
        }).collect(),
    })
}

impl OptionalModelInstallJob {
    pub fn run(self) -> Result<()> {
        install_optional_model_files(&self.root, self.model, &self.staging_root, |directory| {
            probe_optional_model(self.model, directory)
        })
    }
}

fn probe_optional_model(model: &Model, directory: &Path) -> Result<()> {
    let permit = RESOURCES.optional()?;
    check_memory(model)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let interrupted = || permit.should_yield() || Instant::now() >= deadline;
    if interrupted() {
        return Err("Контрольний запуск відкладено. Повторіть встановлення.".into());
    }
    match model.id {
        crate::qwen::HINT_GENERATION_MODEL => crate::qwen::smoke(directory, &interrupted)?,
        "multilingual-e5-small" => crate::e5::smoke(directory, &interrupted)?,
        "piper-uk" => crate::tts::smoke(directory, &interrupted)?,
        "supertonic-3" => crate::supertonic::smoke(directory, &interrupted)?,
        _ => return Err("Контрольний запуск цієї моделі не підтримується.".into()),
    }
    if interrupted() {
        return Err("Контрольний запуск відкладено. Повторіть встановлення.".into());
    }
    Ok(())
}

impl Store {
    pub fn prepare_optional_model(&self, id: &str) -> Result<OptionalModelJob> {
        let model = model(id)?;
        Ok(OptionalModelJob {
            model,
            paths: ModelPaths::new(&self.optional_model_directory, model.id),
        })
    }

    pub fn optional_models(&self) -> Vec<OptionalModel> {
        MODELS
            .iter()
            .map(|model| {
                let paths = ModelPaths::new(&self.optional_model_directory, model.id);
                let installed = paths.installed();
                OptionalModel {
                    id: model.id,
                    name: model.name,
                    license: model.license,
                    revision: model.revision,
                    files_present: package_directory(installed).unwrap_or(false)
                        && model.assets.iter().all(|asset| {
                            local_size(&installed.join(asset.name)) == Some(asset.bytes)
                        }),
                    has_local_files: [&paths.active, &paths.previous, &paths.removed]
                        .iter()
                        .any(|path| path.symlink_metadata().is_ok()),
                    has_recovery_copy: [&paths.removed, &paths.previous].iter().any(|path| {
                        package_directory(path).unwrap_or(false)
                            && model.assets.iter().all(|asset| {
                                local_size(&path.join(asset.name)) == Some(asset.bytes)
                            })
                    }),
                    can_generate_language_hints: model.id == crate::qwen::HINT_GENERATION_MODEL,
                    memory: model.budget.readiness(),
                    assets: model
                        .assets
                        .iter()
                        .map(|asset| OptionalAsset {
                            name: asset.name,
                            download_url: format!(
                                "https://huggingface.co/{}/resolve/{}/{}",
                                model.repo, model.revision, asset.remote_path
                            ),
                            bytes: asset.bytes,
                            sha256: asset.sha256,
                        })
                        .collect(),
                }
            })
            .collect()
    }

    pub fn check_optional_model_space(&self, id: &str, staging_root: &Path) -> Result<()> {
        let model = model(id)?;
        let staged = staging_root.join(model.revision);
        fs::create_dir_all(&staged).map_err(model_error)?;
        fs::create_dir_all(&self.optional_model_directory).map_err(model_error)?;
        let mut remaining = 0;
        let mut total = 0;
        for asset in model.assets {
            total += asset.bytes;
            let path = staged.join(asset.name);
            let existing = local_size(&path).unwrap_or(0);
            remaining += match existing.cmp(&asset.bytes) {
                std::cmp::Ordering::Equal if verify_asset(&path, asset, io::sink()).is_ok() => 0,
                std::cmp::Ordering::Less => asset.bytes - existing,
                _ => asset.bytes,
            };
        }
        ensure_capacity([
            (&staged, remaining),
            (&self.optional_model_directory, total + PACKAGE_OVERHEAD),
        ])
    }

    pub fn prepare_optional_model_install(
        &self,
        id: &str,
        staging_root: &Path,
    ) -> Result<OptionalModelInstallJob> {
        Ok(OptionalModelInstallJob {
            model: model(id)?,
            root: self.optional_model_directory.clone(),
            staging_root: staging_root.to_owned(),
        })
    }

    pub fn install_optional_model(&self, id: &str, staging_root: &Path) -> Result<()> {
        self.prepare_optional_model_install(id, staging_root)?.run()
    }

    pub fn rollback_optional_model(&self, id: &str) -> Result<()> {
        self.rollback_optional_model_files(model(id)?)
    }

    fn rollback_optional_model_files(&self, model: &Model) -> Result<()> {
        let root = &self.optional_model_directory;
        if !package_directory(root)? {
            return Err("Збережена копія моделі відсутня.".into());
        }
        let paths = ModelPaths::new(root, model.id);
        let _gate = lock_package(&paths.lock, true)?;
        for candidate in [&paths.removed, &paths.previous] {
            if package_directory(candidate)?
                && model.assets.iter().all(|asset| {
                    verify_asset(&candidate.join(asset.name), asset, io::sink()).is_ok()
                })
            {
                return restore_verified_package(candidate, &paths.active);
            }
        }
        Err("Немає збереженої копії, яка відповідає контрольним сумам поточного каталогу. Скористайтеся перевіреним локальним завантаженням або завантажте модель повторно. Поточні файли збережено.".into())
    }

    pub fn verify_optional_model(&self, id: &str) -> Result<()> {
        let model = model(id)?;
        let paths = ModelPaths::new(&self.optional_model_directory, id);
        if !package_directory(&self.optional_model_directory)? {
            return Err("Модель відсутня.".into());
        }
        let _gate = lock_package(&paths.lock, false)?;
        let installed = paths.installed();
        if !package_directory(installed)? {
            return Err("Модель відсутня.".into());
        }
        for asset in model.assets {
            verify_asset(&installed.join(asset.name), asset, io::sink())?;
        }
        Ok(())
    }

    pub fn remove_optional_model(&self, id: &str) -> Result<bool> {
        let id = if id == RETIRED_MODEL { RETIRED_MODEL } else { model(id)?.id };
        if !package_directory(&self.optional_model_directory)? {
            return Ok(false);
        }
        let paths = ModelPaths::new(&self.optional_model_directory, id);
        let _gate = lock_package(&paths.lock, true)?;
        let mut removed = remove_package_directory(&paths.previous)?;
        removed |= remove_package_directory(&paths.removed)?;
        if package_directory(&paths.active)? {
            fs::rename(&paths.active, &paths.removed).map_err(model_error)?;
            removed |= remove_package_directory(&paths.removed)?;
        }
        Ok(removed)
    }
}

fn install_optional_model_files(
    root: &Path,
    model: &Model,
    staging_root: &Path,
    probe: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let source = staging_root.join(model.revision);
    if !package_directory(&source)? {
        return Err("Завантажені файли моделі відсутні.".into());
    }
    install_optional_model_candidate(
        root,
        model,
        |destination| {
            for asset in model.assets {
                let mut output = File::create(destination.join(asset.name)).map_err(model_error)?;
                verify_asset(&source.join(asset.name), asset, &mut output)?;
                output.sync_all().map_err(model_error)?;
            }
            Ok(())
        },
        probe,
    )
}

fn install_optional_model_candidate(
    root: &Path,
    model: &Model,
    stage: impl FnOnce(&Path) -> Result<()>,
    probe: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    fs::create_dir_all(root).map_err(model_error)?;
    let paths = ModelPaths::new(root, model.id);
    let _gate = lock_package(&paths.lock, true)?;
    ensure_capacity([(
        root,
        model.assets.iter().map(|asset| asset.bytes).sum::<u64>() + PACKAGE_OVERHEAD,
    )])?;
    let staged = tempfile::tempdir_in(root).map_err(model_error)?;
    stage(staged.path())?;
    probe(staged.path()).map_err(|error| {
            format!("Модель не активовано: контрольний запуск не завершено. Вихідні файли збережено для повторної спроби. {error}")
        })?;
    activate_verified_package(
        staged.path(),
        &paths.active,
        &paths.previous,
        &paths.removed,
    )?;
    // Retain the prior install for an explicit offline, checksum-verified rollback.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_candidate_preserves_installation_and_verified_candidate_repairs_it() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("reader.db")).unwrap();
        let model = Model {
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
        let staged = root.path().join("v1");
        fs::create_dir(&staged).unwrap();
        let candidate = staged.join("model.onnx");
        let active = store.optional_model_directory.join("fixture/model.onnx");
        fs::write(&candidate, b"abc").unwrap();
        install_optional_model_files(&store.optional_model_directory, &model, root.path(), |_| {
            Ok(())
        })
        .unwrap();

        install_optional_model_files(&store.optional_model_directory, &model, root.path(), |_| {
            Ok(())
        })
        .unwrap();
        let previous = store
            .optional_model_directory
            .join(".previous-fixture/model.onnx");
        assert_eq!(fs::read(&previous).unwrap(), b"abc");

        let failed_probe = install_optional_model_files(
            &store.optional_model_directory,
            &model,
            root.path(),
            |_| Err("Контрольний runtime недоступний.".into()),
        );
        assert!(failed_probe.unwrap_err().contains("не активовано"));
        assert_eq!(fs::read(&active).unwrap(), b"abc");
        assert_eq!(fs::read(&previous).unwrap(), b"abc");
        assert_eq!(fs::read(&candidate).unwrap(), b"abc");

        fs::write(&candidate, b"bad").unwrap();
        assert!(install_optional_model_files(
            &store.optional_model_directory,
            &model,
            root.path(),
            |_| Ok(())
        )
        .is_err());
        assert_eq!(fs::read(&active).unwrap(), b"abc");

        fs::write(&active, b"bad").unwrap();
        store.rollback_optional_model_files(&model).unwrap();
        assert_eq!(fs::read(&active).unwrap(), b"abc");
        assert_eq!(fs::read(&previous).unwrap(), b"bad");
        assert!(store.rollback_optional_model_files(&model).is_err());
        assert_eq!(fs::read(&active).unwrap(), b"abc");

        fs::write(&active, b"bad").unwrap();
        fs::write(&candidate, b"abc").unwrap();
        install_optional_model_files(&store.optional_model_directory, &model, root.path(), |_| {
            Ok(())
        })
        .unwrap();
        assert_eq!(fs::read(active).unwrap(), b"abc");
    }
}
