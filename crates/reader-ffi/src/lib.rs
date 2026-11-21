mod dispatch;
mod gateway;
mod registry;
mod validation;
mod wire;

use std::panic::{catch_unwind, AssertUnwindSafe};
use wire::{Error, Result, MAX_WIRE, VERSION};

#[repr(C)]
#[derive(Default)]
pub struct ReaderBuffer {
    pub data: *const u8,
    pub len: u64,
    pub allocation: u64,
}

fn publish(result: Result<serde_json::Value>) -> Result<ReaderBuffer> {
    let envelope = match result {
        Ok(data) => serde_json::json!({ "version": VERSION, "ok": true, "data": data }),
        Err(error) => serde_json::json!({ "version": VERSION, "ok": false, "error": error }),
    };
    let bytes = serde_json::to_vec(&envelope)
        .map_err(|_| Error::new("serialization_error", "Cannot encode response"))?
        .into_boxed_slice();
    if bytes.len() > MAX_WIRE {
        return publish(Err(Error::new(
            "response_too_large",
            "Response exceeds 256MiB wire bound",
        )));
    }
    let mut state = registry::lock()?;
    let allocation = state.next_id()?;
    let result = ReaderBuffer {
        data: bytes.as_ptr(),
        len: bytes.len() as u64,
        allocation,
    };
    state.buffers.insert(allocation, bytes);
    Ok(result)
}

unsafe fn invoke(handle: u64, input: *const u8, len: u64) -> Result<serde_json::Value> {
    if len > MAX_WIRE as u64 || len > isize::MAX as u64 || (input.is_null() && len != 0) {
        return Err(Error::new(
            "invalid_argument",
            "Invalid input pointer/length or wire bound",
        ));
    }
    let bytes = if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(input, len as usize) }
    };
    let command = validation::decode(bytes)?;
    registry::lock()?.call(handle, command)
}

#[no_mangle]
pub extern "C" fn reader_abi_version() -> u32 {
    VERSION
}

/// # Safety
/// Non-null input must point to len readable bytes for the entire synchronous call.
#[no_mangle]
pub unsafe extern "C" fn reader_call(handle: u64, input: *const u8, len: u64) -> ReaderBuffer {
    let result = catch_unwind(AssertUnwindSafe(|| unsafe { invoke(handle, input, len) }))
        .unwrap_or_else(|_| Err(Error::new("internal_error", "Rust operation panicked")));
    catch_unwind(AssertUnwindSafe(|| publish(result).unwrap_or_default())).unwrap_or_default()
}

#[no_mangle]
pub extern "C" fn reader_close(handle: u64) -> u32 {
    catch_unwind(|| {
        registry::lock()
            .map(|mut state| u32::from(!state.close(handle)))
            .unwrap_or(2)
    })
    .unwrap_or(2)
}

#[no_mangle]
pub extern "C" fn reader_release(allocation: u64) -> u32 {
    catch_unwind(|| {
        registry::lock()
            .map(|mut state| u32::from(state.buffers.remove(&allocation).is_none()))
            .unwrap_or(2)
    })
    .unwrap_or(2)
}
