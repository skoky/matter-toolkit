use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

pub const DEFAULT_CONTROLLER_ID: u64 = 100;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AppState {
    pub controller_id: u64,
    pub fabric_label: String,
    pub devices: Vec<KnownDevice>,
    pub endpoint_aliases: Vec<EndpointAlias>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KnownDevice {
    pub label: String,
    pub node_id: u64,
    pub last_address: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EndpointAlias {
    pub node_id: u64,
    pub endpoint_id: u16,
    pub label: String,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            controller_id: DEFAULT_CONTROLLER_ID,
            fabric_label: "matter-client".to_string(),
            devices: Vec::new(),
            endpoint_aliases: Vec::new(),
        }
    }
}

impl AppState {
    pub fn load(path: &str) -> Result<Self> {
        if !Path::new(path).exists() {
            return Ok(Self::default());
        }

        Ok(toml::from_str(&fs::read_to_string(path)?)?)
    }

    pub fn save(&self, path: &str) -> Result<()> {
        fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn next_node_id(&self) -> u64 {
        self.devices
            .iter()
            .map(|d| d.node_id)
            .max()
            .unwrap_or(0x1000)
            + 1
    }
}
