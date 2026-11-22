use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::marian::{Config, MTModel};
use sentencepiece_rs::SentencePieceProcessor;
use sha2::{Digest, Sha256};

use crate::{
    translation::{
        input_for_sentence, AlignedTranslation, ModelEdge, TranslationProvider, TranslationRequest,
        TranslationSettings,
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

struct Package {
    pub source: &'static str,
    pub target: &'static str,
    pub revision: &'static str,
    pub sizes: [u64; 5],
    pub hashes: [&'static str; 5],
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationPackageStatus {
    pub source: &'static str,
    pub target: &'static str,
    pub files_present: bool,
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

    pub fn install_translation_package(&self, archive_path: &Path) -> Result<String> {
        let file = File::open(archive_path).map_err(model_error)?;
        if file.metadata().map_err(model_error)?.len() > 400_000_000 {
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
        let destination = MarianProvider {
            directory: &self.model_directory,
        }
        .path(package);
        fs::create_dir_all(&self.model_directory).map_err(model_error)?;
        let stage = tempfile::tempdir_in(&self.model_directory).map_err(model_error)?;
        for (asset, expected) in ASSETS.iter().zip(package.hashes) {
            let input = archive.by_name(asset).map_err(model_error)?;
            let output = File::create(stage.path().join(asset)).map_err(model_error)?;
            let (_, digest) = copy_and_hash(input, output, asset_limit(asset))?;
            if digest != expected {
                return Err(format!("Файл мовного пакета пошкоджений: {asset}"));
            }
        }
        if destination.exists() {
            verify_files(&destination, package, &ASSETS)?;
        } else {
            fs::rename(stage.path(), &destination).map_err(model_error)?;
        }
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
        let path = self.path(model);
        ASSETS.iter().zip(model.sizes).all(|(asset, expected)| {
            fs::metadata(path.join(asset))
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == expected)
        })
    }

    fn path(&self, model: &Package) -> PathBuf {
        self.directory
            .join(format!("{}-{}", model.source, model.target))
    }
}

impl TranslationProvider for MarianProvider<'_> {
    fn installed_models(&self) -> Result<Vec<ModelEdge>> {
        Ok(catalog::PACKAGES
            .iter()
            .filter(|model| self.files_present(model))
            .map(|model| ModelEdge {
                source: model.source.into(),
                target: model.target.into(),
                revision: model.revision.into(),
                quality: None,
            })
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
        let directory = self.path(package);
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

fn copy_and_hash(
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

fn model_error(error: impl std::fmt::Display) -> String {
    format!("Помилка локального мовного пакета: {error}")
}
