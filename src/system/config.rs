//! Centralized configuration management

use crate::types::BrewConfig;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use std::sync::Arc;

pub struct ConfigManager {
    config: Arc<Mutex<CriticalSectionRawMutex, BrewConfig>>,
}

impl ConfigManager {
    pub fn new() -> Self {
        Self {
            config: Arc::new(Mutex::new(BrewConfig::default())),
        }
    }

    pub fn get_handle(&self) -> Arc<Mutex<CriticalSectionRawMutex, BrewConfig>> {
        Arc::clone(&self.config)
    }

    pub async fn get_config(&self) -> BrewConfig {
        self.config.lock().await.clone()
    }

    pub async fn update_config<F>(&self, update_fn: F)
    where
        F: FnOnce(&mut BrewConfig),
    {
        let mut config = self.config.lock().await;
        update_fn(&mut config);
    }
}
