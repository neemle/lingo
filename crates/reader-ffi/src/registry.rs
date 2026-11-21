use crate::{
    dispatch, gateway,
    wire::{Command, Error, Result},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

#[derive(Default)]
pub struct Registry {
    next: u64,
    stores: HashMap<u64, gateway::Store>,
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
                self.stores.insert(id, store);
                return Ok(json!({ "handle": id }));
            }
        }
        let store = self
            .stores
            .get_mut(&handle)
            .ok_or_else(|| Error::new("invalid_handle", "Store handle is missing or closed"))?;
        dispatch::execute(store, command)
    }

    pub fn close(&mut self, handle: u64) -> bool {
        self.stores.remove(&handle).is_some()
    }
}
