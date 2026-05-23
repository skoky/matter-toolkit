use anyhow::Result;
use std::{fs, path::Path};

pub const DEFAULT_CONTROLLER_ID: u64 = 100;

#[derive(Clone, Debug)]
pub struct AppState {
    pub controller_id: u64,
    pub fabric_label: String,
    pub devices: Vec<KnownDevice>,
    pub endpoint_aliases: Vec<EndpointAlias>,
}

#[derive(Clone, Debug)]
pub struct KnownDevice {
    pub label: String,
    pub node_id: u64,
    pub last_address: String,
}

#[derive(Clone, Debug)]
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

        let mut state = Self::default();
        for line in fs::read_to_string(path)?.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parts: Vec<_> = line.split('\t').collect();
            match parts.first().copied() {
                Some("controller_id") if parts.len() >= 2 => {
                    state.controller_id = parts[1].parse()?;
                }
                Some("fabric_label") if parts.len() >= 2 => {
                    state.fabric_label = unescape_field(parts[1]);
                }
                Some("device") if parts.len() >= 4 => {
                    state.devices.push(KnownDevice {
                        label: unescape_field(parts[1]),
                        node_id: parts[2].parse()?,
                        last_address: unescape_field(parts[3]),
                    });
                }
                Some("endpoint") if parts.len() >= 4 => {
                    state.endpoint_aliases.push(EndpointAlias {
                        node_id: parts[1].parse()?,
                        endpoint_id: parts[2].parse()?,
                        label: unescape_field(parts[3]),
                    });
                }
                _ => {}
            }
        }

        Ok(state)
    }

    pub fn save(&self, path: &str) -> Result<()> {
        let mut out = format!(
            "controller_id\t{}\nfabric_label\t{}\n",
            self.controller_id,
            escape_field(&self.fabric_label)
        );
        for device in &self.devices {
            out.push_str(&format!(
                "device\t{}\t{}\t{}\n",
                escape_field(&device.label),
                device.node_id,
                escape_field(&device.last_address)
            ));
        }
        for alias in &self.endpoint_aliases {
            out.push_str(&format!(
                "endpoint\t{}\t{}\t{}\n",
                alias.node_id,
                alias.endpoint_id,
                escape_field(&alias.label)
            ));
        }
        fs::write(path, out)?;
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

fn escape_field(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

fn unescape_field(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}
