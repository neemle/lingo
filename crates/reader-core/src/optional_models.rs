use crate::{
    model_storage::{
        activate_verified_package, copy_and_hash, lock_package, model_error, package_directory,
        remove_package_directory,
    },
    resources::{MemoryReadiness, ModelBudget, RESOURCES},
    space::{ensure_capacity, PACKAGE_OVERHEAD},
    Result, Store,
};
use serde::Serialize;
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

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

// Pins are part of the application; downloaded files cannot update this catalogue.
const MODELS: &[Model] = &[
    Model {
        id: "qwen-0.8b",
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
        budget: ModelBudget {
            working_mib: 2560,
            minimum_total_mib: 6144,
        },
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
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionalModel {
    id: &'static str,
    name: &'static str,
    license: &'static str,
    revision: &'static str,
    files_present: bool,
    has_local_files: bool,
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
    let corrupted = || format!("Файл моделі пошкоджений або відсутній: {}", asset.name);
    if local_size(path) != Some(asset.bytes) {
        return Err(corrupted());
    }
    let input = File::open(path).map_err(model_error)?;
    let (bytes, digest) = copy_and_hash(input, output, asset.bytes)?;
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
        let memory = self.model.budget.readiness();
        if memory.available_mib.is_none() {
            return Err("Не вдалося визначити доступну пам’ять. Модель пропущено.".into());
        }
        if memory
            .total_mib
            .is_some_and(|total| total < memory.minimum_total_mib)
        {
            return Err(format!(
                "Для {} потрібно щонайменше {} МіБ RAM.",
                self.model.name, memory.minimum_total_mib
            ));
        }
        if !memory.ready {
            return Err(format!(
                "Для {} потрібно {} МіБ доступної пам’яті.",
                self.model.name, memory.required_mib
            ));
        }
        let installed = self.paths.installed();
        if !package_directory(installed)? {
            return Err("Модель відсутня.".into());
        }
        let _gate = lock_package(&self.paths.lock, false)?;
        let installed = self.paths.installed();
        let requested_yield = || permit.should_yield();
        for asset in self.model.assets {
            if requested_yield() {
                return Err("Аналіз відкладено, щоб виконати переклад.".into());
            }
            verify_asset(&installed.join(asset.name), asset, io::sink())?;
        }
        if requested_yield() {
            return Err("Аналіз відкладено, щоб виконати переклад.".into());
        }
        inference(installed, &requested_yield)
    }
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

    pub fn install_optional_model(&self, id: &str, staging_root: &Path) -> Result<()> {
        self.install_optional_model_files(model(id)?, staging_root)
    }

    fn install_optional_model_files(&self, model: &Model, staging_root: &Path) -> Result<()> {
        let source = staging_root.join(model.revision);
        if !package_directory(&source)? {
            return Err("Завантажені файли моделі відсутні.".into());
        }
        let root = &self.optional_model_directory;
        fs::create_dir_all(root).map_err(model_error)?;
        let paths = ModelPaths::new(root, model.id);
        let _gate = lock_package(&paths.lock, true)?;
        ensure_capacity([(
            root,
            model.assets.iter().map(|asset| asset.bytes).sum::<u64>() + PACKAGE_OVERHEAD,
        )])?;
        let staged = tempfile::tempdir_in(root).map_err(model_error)?;
        for asset in model.assets {
            let mut output = File::create(staged.path().join(asset.name)).map_err(model_error)?;
            verify_asset(&source.join(asset.name), asset, &mut output)?;
            output.sync_all().map_err(model_error)?;
        }
        activate_verified_package(
            staged.path(),
            &paths.active,
            &paths.previous,
            &paths.removed,
        )?;
        remove_package_directory(&paths.previous)?;
        remove_package_directory(&paths.removed)?;
        Ok(())
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
        let model = model(id)?;
        if !package_directory(&self.optional_model_directory)? {
            return Ok(false);
        }
        let paths = ModelPaths::new(&self.optional_model_directory, model.id);
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
        store
            .install_optional_model_files(&model, root.path())
            .unwrap();

        fs::write(&candidate, b"bad").unwrap();
        assert!(store
            .install_optional_model_files(&model, root.path())
            .is_err());
        assert_eq!(fs::read(&active).unwrap(), b"abc");

        fs::write(&active, b"bad").unwrap();
        fs::write(&candidate, b"abc").unwrap();
        store
            .install_optional_model_files(&model, root.path())
            .unwrap();
        assert_eq!(fs::read(active).unwrap(), b"abc");
    }
}
