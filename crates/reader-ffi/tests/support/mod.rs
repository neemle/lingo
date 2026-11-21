use reader_ffi::{reader_call, reader_close, reader_release, ReaderBuffer};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

pub struct Response(ReaderBuffer);

impl Response {
    pub fn call(handle: u64, bytes: &[u8]) -> Self {
        // The slice remains readable for the entire synchronous call.
        unsafe { Self::raw(handle, bytes.as_ptr(), bytes.len() as u64) }
    }

    pub unsafe fn raw(handle: u64, data: *const u8, len: u64) -> Self {
        let response = Self(unsafe { reader_call(handle, data, len) });
        assert!(!response.0.data.is_null(), "bridge returned no response");
        assert!(response.0.len > 0);
        assert_ne!(response.0.allocation, 0);
        response
    }

    pub fn json(&self) -> Value {
        // This response owns the allocation; release cannot occur during this borrow.
        let bytes = unsafe {
            std::slice::from_raw_parts(self.0.data, usize::try_from(self.0.len).unwrap())
        };
        let value: Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(value["version"], 1);
        value
    }

    pub fn allocation(&self) -> u64 {
        self.0.allocation
    }

    pub fn release(mut self) -> u32 {
        let allocation = std::mem::take(&mut self.0.allocation);
        reader_release(allocation)
    }
}

impl Drop for Response {
    fn drop(&mut self) {
        if self.0.allocation != 0 {
            reader_release(self.0.allocation);
        }
    }
}

pub fn call(handle: u64, request: Value) -> Value {
    Response::call(handle, &serde_json::to_vec(&request).unwrap()).json()
}

pub fn assert_error(value: &Value, code: &str) {
    assert_eq!(value["ok"], false, "{value}");
    assert_eq!(value["error"]["version"], 1);
    assert_eq!(value["error"]["code"], code, "{value}");
    assert_eq!(value["error"]["retryable"], false);
    assert!(value["error"]["message"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    assert!(value.get("data").is_none());
}

pub struct StoreFixture {
    pub handle: u64,
    dir: PathBuf,
}

impl StoreFixture {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "reader-ffi-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let mut fixture = Self { handle: 0, dir };
        let result = call(
            0,
            json!({"version": 1, "operation": "open", "path": fixture.dir.join("reader.db")}),
        );
        assert_eq!(result["ok"], true, "{result}");
        fixture.handle = result["data"]["handle"].as_u64().unwrap();
        assert_ne!(fixture.handle, 0);
        fixture
    }
}

impl Drop for StoreFixture {
    fn drop(&mut self) {
        reader_close(self.handle);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
