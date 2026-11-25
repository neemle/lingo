use crate::Result;
use std::{collections::BTreeMap, path::Path};

pub(crate) const PACKAGE_OVERHEAD: u64 = 64 * 1024;

pub(crate) fn ensure_capacity<const N: usize>(paths: [(&Path, u64); N]) -> Result<()> {
    ensure_capacity_for(paths, "мовного пакета")
}

pub(crate) fn ensure_capacity_for<const N: usize>(paths: [(&Path, u64); N], purpose: &str) -> Result<()> {
    let requirements = paths
        .into_iter()
        .filter(|(_, bytes)| *bytes != 0)
        .map(|(path, bytes)| {
            let (volume, available) = space(path).map_err(|error| {
                format!("Не вдалося перевірити місце для {purpose}. {error}")
            })?;
            Ok((volume, available, bytes))
        })
        .collect::<Result<Vec<_>>>()?;
    check_volumes(requirements, purpose)
}

fn check_volumes<K: Ord>(requirements: impl IntoIterator<Item = (K, u64, u64)>, purpose: &str) -> Result<()> {
    let mut by_volume = BTreeMap::<K, (u64, u64)>::new();
    for (volume, available, bytes) in requirements {
        let budget = by_volume.entry(volume).or_insert((available, 0));
        budget.0 = budget.0.min(available);
        budget.1 = budget.1.saturating_add(bytes);
    }
    for (available, required) in by_volume.into_values() {
        if required > available {
            const MIB: u64 = 1024 * 1024;
            return Err(format!(
                "Бракує місця для {purpose}. Потрібно {} МіБ, доступно {} МіБ.",
                required.div_ceil(MIB),
                available / MIB
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn space(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let volume = std::fs::metadata(path)?.dev();
    let stats = rustix::fs::statvfs(path)?;
    Ok((volume, stats.f_bavail.saturating_mul(stats.f_frsize)))
}

#[cfg(windows)]
fn space(path: &Path) -> std::io::Result<(String, u64)> {
    use std::{io::Error, os::windows::ffi::OsStrExt, ptr::null_mut};
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    };

    let path: Vec<u16> = std::fs::canonicalize(path)?
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect();
    let mut mount = vec![0u16; path.len().max(261)];
    let mut volume = [0u16; 64];
    let mut available = 0;
    // All buffers are writable, sized in UTF-16 code units, and the input ends in NUL.
    unsafe {
        if GetVolumePathNameW(path.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) == 0
            || GetDiskFreeSpaceExW(path.as_ptr(), &mut available, null_mut(), null_mut()) == 0
        {
            return Err(Error::last_os_error());
        }
        let identifier = if GetVolumeNameForVolumeMountPointW(
            mount.as_ptr(),
            volume.as_mut_ptr(),
            volume.len() as u32,
        ) != 0
        {
            volume.as_slice()
        } else {
            mount.as_slice()
        };
        Ok((
            String::from_utf16_lossy(
                identifier
                    .split(|&unit| unit == 0)
                    .next()
                    .unwrap_or_default(),
            )
            .to_lowercase(),
            available,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_charges_each_volume_for_all_concurrent_copies() {
        assert!(check_volumes([
            ("disk", 1_100, 250),
            ("disk", 1_050, 400),
            ("disk", 1_100, 400)
        ], "мовного пакета")
        .is_ok());
        assert!(check_volumes([
            ("disk", 1_000, 250),
            ("disk", 1_000, 400),
            ("disk", 1_000, 400)
        ], "мовного пакета")
        .is_err());
        assert!(check_volumes([
            ("cache", 650, 250),
            ("cache", 650, 400),
            ("model", 400, 400)
        ], "мовного пакета")
        .is_ok());
        assert!(check_volumes([
            ("cache", 650, 250),
            ("cache", 650, 400),
            ("model", 399, 400)
        ], "мовного пакета")
        .is_err());
    }
}
