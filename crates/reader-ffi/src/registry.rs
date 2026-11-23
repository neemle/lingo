use crate::{
    dispatch, gateway,
    wire::{Command, Error, Result},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{atomic::AtomicU64, Arc, Mutex, OnceLock},
};

#[derive(Default)]
pub struct ModelCancellation(AtomicU64);

#[derive(Clone, Copy)]
pub enum ModelRequestKind {
    Passage,
    Speech,
    SpeechPrefetch,
    PdfPageTranslation,
}

impl ModelCancellation {
    pub fn cancel_through(&self, request_id: u64) {
        self.0.fetch_max(request_id, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn is_canceled(&self, request_id: u64) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed) >= request_id
    }
}

struct OpenStore {
    store: gateway::Store,
    model_cancellations: [Arc<ModelCancellation>; 4],
}

#[derive(Default)]
pub struct Registry {
    next: u64,
    stores: HashMap<u64, OpenStore>,
    pub buffers: HashMap<u64, Box<[u8]>>,
}

pub fn lock() -> Result<std::sync::MutexGuard<'static, Registry>> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| Error::new("bridge_unavailable", "Bridge synchronization failed"))
}

impl Registry {
    pub fn next_id(&mut self) -> Result<u64> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| Error::new("bridge_unavailable", "Handle space exhausted"))?;
        Ok(self.next)
    }

    pub fn call(&mut self, handle: u64, command: Command) -> Result<Value> {
        if handle == 0 {
            if let Command::Open { path } = command {
                let store = gateway::open(&path)?;
                let id = self.next_id()?;
                self.stores.insert(id, OpenStore { store, model_cancellations: Default::default() });
                return Ok(json!({ "handle": id }));
            }
        }
        let store = self
            .stores
            .get_mut(&handle)
            .ok_or_else(|| Error::new("invalid_handle", "Store handle is missing or closed"))?;
        dispatch::execute(&mut store.store, command)
    }

    pub fn store(&self, handle: u64) -> Result<&gateway::Store> {
        self.open_store(handle).map(|entry| &entry.store)
    }

    pub fn model_cancellation(&self, handle: u64, kind: ModelRequestKind) -> Result<Arc<ModelCancellation>> {
        self.open_store(handle).map(|entry| Arc::clone(&entry.model_cancellations[kind as usize]))
    }

    fn open_store(&self, handle: u64) -> Result<&OpenStore> {
        self.stores
            .get(&handle)
            .ok_or_else(|| Error::new("invalid_handle", "Store handle is missing or closed"))
    }

    pub fn import_pdf_file(
        &mut self,
        handle: u64,
        prepared: reader_core::file_originals::PreparedPdfImport,
    ) -> Result<reader_core::Document> {
        self.stores
            .get_mut(&handle)
            .ok_or_else(|| Error::new("invalid_handle", "Store handle is missing or closed"))?
            .store
            .import_pdf_file(prepared)
            .map_err(gateway::domain)
    }

    pub fn insert_intake(
        &mut self, handle: u64, prepared: reader_core::intake::PreparedIntake,
        request_id: Option<&str>,
    ) -> Result<reader_core::intake::IntakeItem> {
        self.stores
            .get_mut(&handle)
            .ok_or_else(|| Error::new("invalid_handle", "Store handle is missing or closed"))?
            .store
            .insert_intake(prepared, request_id)
            .map_err(gateway::domain)
    }

    pub fn close(&mut self, handle: u64) -> bool {
        if let Some(entry) = self.stores.remove(&handle) {
            for cancellation in entry.model_cancellations {
                cancellation.cancel_through(u64::MAX);
            }
            return true;
        }
        false
    }
}
