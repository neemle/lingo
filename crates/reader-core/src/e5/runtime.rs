use crate::Result;
use ort::sys::{OrtApiBase, ORT_API_VERSION};
use std::sync::OnceLock;

type ApiBase = unsafe extern "system" fn() -> *const OrtApiBase;

pub(super) fn environment() -> Result<&'static ort::environment::Environment> {
    static ENVIRONMENT: OnceLock<Result<ort::environment::Environment>> = OnceLock::new();
    ENVIRONMENT
        .get_or_init(|| {
            install_api()?;
            ort::init()
                .with_name("Lingo E5")
                .with_telemetry(false)
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn install_api() -> Result<()> {
    let base = symbol()?;
    // This function is resolved from the bundled ORT library. GetApi checks
    // the requested ABI version before providing its immutable function table.
    unsafe {
        let base = base().as_ref().ok_or("ONNX Runtime не повернув ABI.")?;
        let api = (base.GetApi)(ORT_API_VERSION)
            .as_ref()
            .ok_or("Версія ONNX Runtime не підтримується.")?;
        ort::set_api(api.clone());
    }
    Ok(())
}

#[cfg(target_abi = "macabi")]
fn symbol() -> Result<ApiBase> {
    // The app statically links the native Catalyst slice and retains this symbol.
    unsafe { libloading::os::unix::Library::this().get::<ApiBase>(b"OrtGetApiBase") }
        .map(|symbol| *symbol)
        .map_err(|_| "ONNX Runtime відсутній у збірці Читай.".into())
}

#[cfg(not(target_abi = "macabi"))]
fn symbol() -> Result<ApiBase> {
    static LIBRARY: OnceLock<Result<libloading::Library>> = OnceLock::new();
    let library = LIBRARY
        .get_or_init(|| {
            let executable = std::env::current_exe().map_err(|error| error.to_string())?;
            let path = executable.with_file_name(format!(
                "{}onnxruntime{}",
                std::env::consts::DLL_PREFIX,
                std::env::consts::DLL_SUFFIX
            ));
            if !path.is_file() {
                return Err("ONNX Runtime відсутній у збірці Читай.".into());
            }
            load_runtime(&path)
        })
        .as_ref()
        .map_err(Clone::clone)?;
    // The runtime and its function table are retained for the process lifetime.
    unsafe { library.get::<ApiBase>(b"OrtGetApiBase") }
        .map(|symbol| *symbol)
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
fn load_runtime(path: &std::path::Path) -> Result<libloading::Library> {
    use libloading::os::windows::{
        Library, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };

    // Restrict dependency resolution to the install directory and Windows itself.
    unsafe {
        Library::load_with_flags(
            path,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    }
    .map(Into::into)
    .map_err(|error| error.to_string())
}

#[cfg(all(not(target_abi = "macabi"), not(windows)))]
fn load_runtime(path: &std::path::Path) -> Result<libloading::Library> {
    // The absolute library path is derived from the running executable.
    unsafe { libloading::Library::new(path) }.map_err(|error| error.to_string())
}
