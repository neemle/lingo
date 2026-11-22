use std::{
    collections::HashMap,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::marian::{Config, MTModel};
use sentencepiece_rs::SentencePieceProcessor;

use crate::{
    model_storage::{
        activate_verified_package, copy_and_hash, lock_package, model_error, package_directory,
        remove_package_directory,
    },
    space::{ensure_capacity, PACKAGE_OVERHEAD},
    translation::{
        input_for_sentence, route_readiness, AlignedTranslation, ModelEdge, TranslationProvider,
        TranslationReadiness, TranslationRequest, TranslationSettings,
    },
    Result, Store,
};

mod catalog {
    include!("marian_catalog.rs");
}

const ASSETS: [&str; 5] = [
    "config.json",
    "source.spm",
    "target.spm",
    "vocab.json",
    "model.safetensors",
];
const MAX_ARCHIVE_BYTES: u64 = 400_000_000;

struct Package {
    pub source: &'static str,
    pub target: &'static str,
    pub revision: &'static str,
    pub sizes: [u64; 5],
    pub hashes: [&'static str; 5],
}

impl Package {
    fn edge(&self) -> ModelEdge {
        ModelEdge {
            source: self.source.into(),
            target: self.target.into(),
            revision: self.revision.into(),
            quality: None,
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationPackageStatus {
    pub source: &'static str,
    pub target: &'static str,
    pub files_present: bool,
    pub has_local_files: bool,
    pub revision: &'static str,
    pub assets: Vec<TranslationAsset>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationAsset {
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

struct MarianProvider<'a> {
    directory: &'a Path,
}

impl Store {
    pub fn check_translation_download_space(
        &self,
        source: &str,
        target: &str,
        staging_root: &Path,
        archive_directory: &Path,
    ) -> Result<()> {
        let package = catalog::PACKAGES
            .iter()
            .find(|package| package.source == source && package.target == target)
            .ok_or("Для цієї мовної пари немає пакета.")?;
        let stage = staging_root.join(package.revision);
        fs::create_dir_all(&stage).map_err(model_error)?;
        fs::create_dir_all(archive_directory).map_err(model_error)?;
        fs::create_dir_all(&self.model_directory).map_err(model_error)?;
        let total: u64 = package.sizes.iter().sum();
        let completed: u64 = ASSETS
            .into_iter()
            .zip(package.sizes)
            .map(|(name, expected)| {
                fs::metadata(stage.join(name))
                    .ok()
                    .filter(|info| info.is_file())
                    .map_or(0, |info| info.len().min(expected))
            })
            .sum();
        ensure_capacity([
            (&stage, total - completed),
            (archive_directory, total + PACKAGE_OVERHEAD),
            (&self.model_directory, total + PACKAGE_OVERHEAD),
        ])
    }

    pub fn check_translation_archive_space(&self, directory: &Path, bytes: u64) -> Result<()> {
        if bytes > MAX_ARCHIVE_BYTES {
            return Err("Архів мовного пакета завеликий".into());
        }
        fs::create_dir_all(directory).map_err(model_error)?;
        ensure_capacity([(directory, bytes + PACKAGE_OVERHEAD)])
    }

    pub fn translation_packages(&self) -> Vec<TranslationPackageStatus> {
        let provider = MarianProvider {
            directory: &self.model_directory,
        };
        catalog::PACKAGES
            .iter()
            .map(|package| TranslationPackageStatus {
                source: package.source,
                target: package.target,
                files_present: provider.files_present(package),
                has_local_files: provider.path(package).symlink_metadata().is_ok()
                    || provider.removed_path(package).symlink_metadata().is_ok()
                    || provider.previous_path(package).symlink_metadata().is_ok(),
                revision: package.revision,
                assets: ASSETS
                    .into_iter()
                    .zip(package.sizes)
                    .zip(package.hashes)
                    .map(|((name, bytes), sha256)| TranslationAsset {
                        name,
                        bytes,
                        sha256,
                    })
                    .collect(),
            })
            .collect()
    }

    pub fn remove_translation_package(&self, source: &str, target: &str) -> Result<bool> {
        let package = catalog::PACKAGES
            .iter()
            .find(|package| package.source == source && package.target == target)
            .ok_or("Для цієї мовної пари немає пакета.")?;
        let provider = MarianProvider {
            directory: &self.model_directory,
        };
        if !self.model_directory.is_dir() {
            return Ok(false);
        }
        let _lock = provider.lock(package, true)?;
        let active = provider.path(package);
        let removed = provider.removed_path(package);
        let mut cleared_previous = remove_package_directory(&removed)?;
        cleared_previous |= remove_package_directory(&provider.previous_path(package))?;
        if !package_directory(&active)? {
            return Ok(cleared_previous);
        }
        match fs::rename(&active, &removed) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(cleared_previous),
            Err(error) => return Err(model_error(error)),
        }
        remove_package_directory(&removed)
            .map_err(|error| format!("Пакет відключено. Не вдалося прибрати всі файли. {error}"))?;
        Ok(true)
    }

    pub fn install_translation_package(&self, archive_path: &Path) -> Result<String> {
        let file = File::open(archive_path).map_err(model_error)?;
        if file.metadata().map_err(model_error)?.len() > MAX_ARCHIVE_BYTES {
            return Err("Архів мовного пакета завеликий".into());
        }
        let mut archive = zip::ZipArchive::new(file).map_err(model_error)?;
        if archive.len() != ASSETS.len() {
            return Err("Архів має містити п’ять файлів мовного пакета".into());
        }
        let (size, config_hash) = copy_and_hash(
            archive.by_name("config.json").map_err(model_error)?,
            io::sink(),
            64 * 1024,
        )?;
        if size == 0 {
            return Err("Конфігурація мовного пакета порожня".into());
        }
        let package = catalog::PACKAGES
            .iter()
            .find(|model| model.hashes[0] == config_hash)
            .ok_or("Версію мовного пакета не підтримано")?;
        let provider = MarianProvider {
            directory: &self.model_directory,
        };
        fs::create_dir_all(&self.model_directory).map_err(model_error)?;
        ensure_capacity([(
            &self.model_directory,
            package.sizes.iter().sum::<u64>() + PACKAGE_OVERHEAD,
        )])?;
        let stage = tempfile::tempdir_in(&self.model_directory).map_err(model_error)?;
        for (asset, expected) in ASSETS.iter().zip(package.hashes) {
            let input = archive.by_name(asset).map_err(model_error)?;
            let mut output = File::create(stage.path().join(asset)).map_err(model_error)?;
            let (_, digest) = copy_and_hash(input, &mut output, asset_limit(asset))?;
            if digest != expected {
                return Err(format!("Файл мовного пакета пошкоджений: {asset}"));
            }
            output.sync_all().map_err(model_error)?;
        }
        let _lock = provider.lock(package, true)?;
        let active = provider.path(package);
        let previous = provider.previous_path(package);
        if !package_directory(&active)? || verify_files(&active, package, &ASSETS).is_err() {
            activate_verified_package(
                stage.path(),
                &active,
                &previous,
                &provider.removed_path(package),
            )?;
        }
        remove_package_directory(&previous).map_err(|error| {
            format!("Пакет готовий. Не вдалося прибрати попередні файли. {error}")
        })?;
        remove_package_directory(&provider.removed_path(package)).map_err(|error| {
            format!("Пакет готовий. Не вдалося прибрати попередні файли. {error}")
        })?;
        Ok(format!("{} → {}", package.source, package.target))
    }

    pub fn translate_installed_sentence(
        &mut self,
        document_id: &str,
        sentence_id: &str,
        target: &str,
    ) -> Result<Option<AlignedTranslation>> {
        self.translate_sentence(
            &self.model_directory.clone(),
            document_id,
            sentence_id,
            target,
        )
    }

    pub fn translation_readiness(
        &self,
        source: &str,
        target: &str,
    ) -> Result<TranslationReadiness> {
        let provider = MarianProvider {
            directory: &self.model_directory,
        };
        let catalog: Vec<_> = catalog::PACKAGES.iter().map(Package::edge).collect();
        route_readiness(&catalog, &provider.installed_models()?, source, target)
    }

    fn translate_sentence(
        &mut self,
        package_dir: &Path,
        document_id: &str,
        sentence_id: &str,
        target: &str,
    ) -> Result<Option<AlignedTranslation>> {
        let body: String = self
            .connection
            .query_row(
                "SELECT body FROM documents WHERE id=?1",
                [document_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let document: crate::Document =
            serde_json::from_str(&body).map_err(|error| error.to_string())?;
        let selected = document
            .sentences
            .iter()
            .position(|sentence| sentence.id == sentence_id)
            .ok_or("Речення для перекладу не знайдено")?;
        let (text, joined_lines) = input_for_sentence(&document, selected);
        self.translate_aligned(
            &mut MarianProvider {
                directory: package_dir,
            },
            TranslationRequest {
                source: &document.language,
                target,
                segments: &[text],
                context: None,
                settings: TranslationSettings { beam_size: 1 },
            },
        )
        .map(|translation| {
            translation.map(|mut translation| {
                translation.joined_lines = joined_lines;
                translation
            })
        })
    }
}

impl MarianProvider<'_> {
    fn files_present(&self, model: &Package) -> bool {
        let path = self.read_path(model);
        ASSETS.iter().zip(model.sizes).all(|(asset, expected)| {
            fs::metadata(path.join(asset))
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == expected)
        })
    }

    fn path(&self, model: &Package) -> PathBuf {
        self.directory
            .join(format!("{}-{}", model.source, model.target))
    }

    fn removed_path(&self, model: &Package) -> PathBuf {
        self.directory
            .join(format!(".removed-{}-{}", model.source, model.target))
    }

    fn previous_path(&self, model: &Package) -> PathBuf {
        self.directory
            .join(format!(".previous-{}-{}", model.source, model.target))
    }

    fn read_path(&self, model: &Package) -> PathBuf {
        let active = self.path(model);
        if active.exists() {
            active
        } else {
            self.previous_path(model)
        }
    }

    fn lock(&self, model: &Package, exclusive: bool) -> Result<File> {
        let path = self
            .directory
            .join(format!(".{}-{}.lock", model.source, model.target));
        lock_package(&path, exclusive)
    }
}

impl TranslationProvider for MarianProvider<'_> {
    fn installed_models(&self) -> Result<Vec<ModelEdge>> {
        Ok(catalog::PACKAGES
            .iter()
            .filter(|model| self.files_present(model))
            .map(Package::edge)
            .collect())
    }

    fn translate(
        &mut self,
        edge: &ModelEdge,
        segments: &[String],
        _context: Option<&str>,
        settings: TranslationSettings,
    ) -> Result<Vec<String>> {
        if settings.beam_size != 1 {
            return Err("Цей мовний пакет підтримує лише один варіант декодування".into());
        }
        let package = catalog::PACKAGES
            .iter()
            .find(|package| {
                package.revision == edge.revision
                    && package.source == edge.source
                    && package.target == edge.target
            })
            .ok_or("Невідомий мовний пакет")?;
        let _lock = self.lock(package, false)?;
        let directory = self.read_path(package);
        verify_files(&directory, package, &ASSETS[..4])?;
        let config: Config =
            serde_json::from_slice(&fs::read(directory.join("config.json")).map_err(model_error)?)
                .map_err(model_error)?;
        let vocabulary: HashMap<String, u32> =
            serde_json::from_slice(&fs::read(directory.join("vocab.json")).map_err(model_error)?)
                .map_err(model_error)?;
        let output_pieces: HashMap<_, _> = vocabulary
            .iter()
            .map(|(piece, id)| (*id, piece.as_str()))
            .collect();
        let input =
            SentencePieceProcessor::open(directory.join("source.spm")).map_err(model_error)?;
        let output =
            SentencePieceProcessor::open(directory.join("target.spm")).map_err(model_error)?;
        let mut bytes = Vec::new();
        let (_, digest) = copy_and_hash(
            File::open(directory.join("model.safetensors")).map_err(model_error)?,
            &mut bytes,
            asset_limit("model.safetensors"),
        )?;
        if digest != package.hashes[4] {
            return Err("Файл ваг мовного пакета пошкоджений".into());
        }
        let weights = VarBuilder::from_buffered_safetensors(bytes, DType::F32, &Device::Cpu)
            .map_err(model_error)?;
        // SafeTensors stores a tied embedding once. The verified French→English
        // revision names that tensor under the decoder instead of model.shared.
        let weights = if config.share_encoder_decoder_embeddings
            && !weights.contains_tensor("model.shared.weight")
            && weights.contains_tensor("model.decoder.embed_tokens.weight")
        {
            weights.rename_f(|name| match name {
                "model.shared.weight" => "model.decoder.embed_tokens.weight".into(),
                name => name.into(),
            })
        } else {
            weights
        };
        let mut model = MTModel::new(&config, weights).map_err(model_error)?;
        let unknown = *vocabulary
            .get("<unk>")
            .ok_or("У мовному пакеті немає маркера невідомого слова")?;

        let mut results = Vec::with_capacity(segments.len());
        for segment in segments {
            let mut tokens: Vec<_> = input
                .encode(segment)
                .map_err(model_error)?
                .iter()
                .map(|piece| vocabulary.get(piece).copied().unwrap_or(unknown))
                .collect();
            let limit = config.max_position_embeddings.min(256);
            if tokens.len() >= limit {
                return Err("Речення задовге для цього мовного пакета".into());
            }
            tokens.push(config.eos_token_id);
            model.reset_kv_cache();
            let encoder_input = Tensor::new(tokens.as_slice(), &Device::Cpu)
                .and_then(|x| x.unsqueeze(0))
                .map_err(model_error)?;
            let encoded = model
                .encoder()
                .forward(&encoder_input, 0)
                .map_err(model_error)?;
            let mut token = config.decoder_start_token_id;
            let mut pieces = Vec::new();
            let mut complete = false;
            for index in 0..limit {
                let decoder_input = Tensor::new(&[[token]], &Device::Cpu).map_err(model_error)?;
                // Marian uses padding as the decoder start token but disallows generating it.
                let scores = model
                    .decode(&decoder_input, &encoded, index)
                    .and_then(|output| output.get(0)?.get(0)?.to_vec1::<f32>())
                    .map_err(model_error)?;
                token = scores
                    .into_iter()
                    .enumerate()
                    .filter(|(id, score)| *id != config.pad_token_id as usize && score.is_finite())
                    .max_by(|(_, a), (_, b)| a.total_cmp(b))
                    .map(|(id, _)| id as u32)
                    .ok_or("Модель не повернула доступного токена")?;
                if token == config.eos_token_id || token == config.forced_eos_token_id {
                    complete = true;
                    break;
                }
                if token != config.pad_token_id {
                    pieces.push(
                        *output_pieces
                            .get(&token)
                            .ok_or("Невідомий токен перекладу")?,
                    );
                }
            }
            if !complete {
                return Err("Модель не завершила переклад речення".into());
            }
            let translated = output.decode(&pieces).map_err(model_error)?;
            results.push(translated.trim().to_owned());
        }
        Ok(results)
    }
}

fn asset_limit(asset: &str) -> u64 {
    if asset == "model.safetensors" {
        400_000_000
    } else {
        8_000_000
    }
}

fn verify_files(directory: &Path, package: &Package, assets: &[&str]) -> Result<()> {
    for (asset, expected) in assets.iter().zip(package.hashes) {
        let file = File::open(directory.join(asset)).map_err(model_error)?;
        let (_, actual) = copy_and_hash(file, io::sink(), asset_limit(asset))?;
        if actual != expected {
            return Err(format!("Файл мовного пакета пошкоджений: {asset}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_activation_restores_active_files_and_keeps_prior_copy() {
        let root = tempfile::tempdir().unwrap();
        let active = root.path().join("active");
        let previous = root.path().join("previous");
        let discarded = root.path().join("discarded");
        let stage = root.path().join("stage");
        fs::create_dir(&active).unwrap();
        fs::write(active.join("model"), "active").unwrap();
        assert!(activate_verified_package(&stage, &active, &previous, &discarded).is_err());
        assert_eq!(fs::read_to_string(active.join("model")).unwrap(), "active");
        assert!(!previous.exists());

        fs::create_dir(&previous).unwrap();
        fs::write(previous.join("model"), "previous").unwrap();
        assert!(activate_verified_package(&stage, &active, &previous, &discarded).is_err());
        assert_eq!(fs::read_to_string(active.join("model")).unwrap(), "active");
        assert_eq!(
            fs::read_to_string(previous.join("model")).unwrap(),
            "previous"
        );

        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("model"), "verified").unwrap();
        activate_verified_package(&stage, &active, &previous, &discarded).unwrap();
        assert_eq!(
            fs::read_to_string(active.join("model")).unwrap(),
            "verified"
        );
        assert_eq!(
            fs::read_to_string(discarded.join("model")).unwrap(),
            "active"
        );
    }

    #[test]
    fn package_removal_is_idempotent_and_clears_prior_interrupted_removal() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(&directory.path().join("reader.db")).unwrap();
        let root = &store.model_directory;
        let active = root.join("uk-fr");
        let interrupted = root.join(".removed-uk-fr");
        let other = root.join("fr-en");
        for directory in [&active, &interrupted, &other] {
            fs::create_dir_all(directory).unwrap();
            fs::write(directory.join("config.json"), b"partial").unwrap();
        }
        let status = store
            .translation_packages()
            .into_iter()
            .find(|package| package.source == "uk" && package.target == "fr")
            .unwrap();
        assert!(!status.files_present && status.has_local_files);
        assert!(store.remove_translation_package("uk", "fr").unwrap());
        assert!(!active.exists() && !interrupted.exists());
        assert_eq!(fs::read(other.join("config.json")).unwrap(), b"partial");
        assert!(!store.remove_translation_package("uk", "fr").unwrap());
        assert!(store.remove_translation_package("../fr", "en").is_err());
    }
}
