use crate::{
    model_storage::{
        activate_verified_package, copy_and_hash, lock_package, model_error, package_directory,
        remove_package_directory,
    },
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

impl Store {
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
