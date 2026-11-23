use super::{SpeechAudio, MAX_SECONDS};
use crate::model_storage::lock_package;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, DirBuilder, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

// Bump when audio generation or the cached format changes.
const VERSION: &[u8] = b"reader-speech-v2";
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 96;
const WAV_HEADER_BYTES: usize = 44;
const STAGING_PREFIX: &str = ".pending-";

pub(crate) struct SpeechCacheKey {
    name: String,
    sample_rate: u32,
}

impl SpeechCacheKey {
    pub(crate) fn new(
        engine: &str,
        revision: &str,
        language: &str,
        voice: &str,
        rate: f32,
        spoken: &str,
        sample_rate: u32,
    ) -> Self {
        let text_hash = Sha256::digest(spoken.as_bytes());
        let mut digest = Sha256::new();
        for field in [
            VERSION,
            engine.as_bytes(),
            revision.as_bytes(),
            language.as_bytes(),
            voice.as_bytes(),
            &rate.to_bits().to_le_bytes(),
            text_hash.as_slice(),
            &sample_rate.to_le_bytes(),
        ] {
            digest.update((field.len() as u64).to_le_bytes());
            digest.update(field);
        }
        Self {
            name: format!("{:x}.wav", digest.finalize()),
            sample_rate,
        }
    }

    pub(crate) fn read(&self, package: &Path) -> Option<SpeechAudio> {
        let directory = cache_directory(package, false)?;
        let _lock = lock_package(&directory.join(".lock"), true).ok()?;
        self.read_file(&directory.join(&self.name))
    }

    pub(crate) fn write(&self, package: &Path, audio: &SpeechAudio) -> bool {
        // Failure to cache generated audio must not interrupt playback.
        self.persist(package, audio).is_some()
    }

    fn persist(&self, package: &Path, audio: &SpeechAudio) -> Option<()> {
        let directory = cache_directory(package, true)?;
        let _lock = lock_package(&directory.join(".lock"), true).ok()?;
        let path = directory.join(&self.name);
        if self.read_file(&path).is_some() {
            return Some(());
        }
        make_room(&directory, audio.wav_bytes.len() as u64)?;
        let mut staging = tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .tempfile_in(&directory)
            .ok()?;
        staging.write_all(&audio.wav_bytes).ok()?;
        staging.as_file().sync_all().ok()?;
        staging.persist(path).ok()?;
        Some(())
    }

    fn read_file(&self, path: &Path) -> Option<SpeechAudio> {
        let metadata = path.symlink_metadata().ok()?;
        let max_len = (WAV_HEADER_BYTES + self.sample_rate as usize * MAX_SECONDS * 2) as u64;
        if !metadata.is_file()
            || metadata.len() <= WAV_HEADER_BYTES as u64
            || metadata.len() > max_len
        {
            return None;
        }
        let file = File::options().read(true).write(true).open(path).ok()?;
        let mut wav_bytes = Vec::with_capacity(metadata.len() as usize);
        (&file).take(max_len + 1).read_to_end(&mut wav_bytes).ok()?;
        if wav_bytes.len() as u64 != metadata.len() {
            return None;
        }
        let pcm_len = wav_bytes.len() - WAV_HEADER_BYTES;
        if !pcm_len.is_multiple_of(2)
            || &wav_bytes[..4] != b"RIFF"
            || wav_bytes[4..8] != (pcm_len as u32 + 36).to_le_bytes()
            || &wav_bytes[8..16] != b"WAVEfmt "
            || wav_bytes[16..24] != [16, 0, 0, 0, 1, 0, 1, 0]
            || wav_bytes[24..28] != self.sample_rate.to_le_bytes()
            || wav_bytes[28..32] != (self.sample_rate * 2).to_le_bytes()
            || wav_bytes[32..36] != [2, 0, 16, 0]
            || &wav_bytes[36..40] != b"data"
            || wav_bytes[40..44] != (pcm_len as u32).to_le_bytes()
        {
            return None;
        }
        let (minimum, maximum) = wav_bytes[WAV_HEADER_BYTES..]
            .chunks_exact(2)
            .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
            .fold((i16::MAX, i16::MIN), |(minimum, maximum), sample| {
                (minimum.min(sample), maximum.max(sample))
            });
        // The writer's minimum normalization peak is 0.01; a 1e-5
        // signal span then occupies at most 33 levels in cached PCM.
        if i32::from(maximum) - i32::from(minimum) <= 33 {
            return None;
        }
        let _ = file.set_modified(SystemTime::now());
        Some(SpeechAudio {
            sample_rate: self.sample_rate,
            samples: (pcm_len / 2) as u32,
            wav_bytes,
            playback_rate: 1.0,
        })
    }
}

fn cache_directory(package: &Path, create: bool) -> Option<PathBuf> {
    // The model package lock protects its lifetime, including uninstall.
    let path = package.join(".speech-cache");
    if create {
        let mut builder = DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    path.symlink_metadata().ok()?.is_dir().then_some(path)
}

fn make_room(directory: &Path, incoming: u64) -> Option<()> {
    let mut entries = Vec::new();
    let mut bytes = incoming;
    for entry in fs::read_dir(directory).ok()? {
        let entry = entry.ok()?;
        if !entry.file_type().ok()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_str().unwrap_or_default();
        if name.starts_with(STAGING_PREFIX) {
            fs::remove_file(entry.path()).ok()?;
        } else if name
            .strip_suffix(".wav")
            .is_some_and(|stem| stem.len() == 64 && stem.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            let metadata = entry.metadata().ok()?;
            bytes = bytes.saturating_add(metadata.len());
            entries.push((metadata.modified().ok()?, metadata.len(), entry.path()));
        }
    }
    entries.sort_unstable_by_key(|entry| entry.0);
    let mut count = entries.len() + 1;
    for (_, size, path) in entries {
        if count <= MAX_ENTRIES && bytes <= MAX_BYTES {
            return Some(());
        }
        fs::remove_file(path).ok()?;
        bytes -= size;
        count -= 1;
    }
    (bytes <= MAX_BYTES).then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_speech_rejects_silent_waveform_and_a_silent_cached_clip() {
        let rate = 22_050;
        assert!(super::super::to_wav(&[0.0; 32], rate).is_err());
        assert!(super::super::to_wav(&[0.2; 32], rate).is_err());
        let mut audio = super::super::to_wav(&[0.0, 0.4, -0.4, 0.0], rate).unwrap();
        let root = tempfile::tempdir().unwrap();
        let key = SpeechCacheKey::new("test", "1", "fr", "voice", 1.0, "bonjour", rate);
        assert!(key.write(root.path(), &audio));
        assert!(key.read(root.path()).is_some());

        audio.wav_bytes[WAV_HEADER_BYTES..].fill(0);
        fs::write(
            root.path().join(".speech-cache").join(&key.name),
            audio.wav_bytes,
        )
        .unwrap();
        assert!(key.read(root.path()).is_none());
    }
}
