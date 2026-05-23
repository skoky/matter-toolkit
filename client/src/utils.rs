use std::net::IpAddr;
use matc::clusters::dt_names;
use crate::state::{AppState, EndpointAlias, KnownDevice};
use crate::types::EndpointSummary;

pub fn first_socket_addr(addresses: &[IpAddr], port: u16) -> String {
    let ip = addresses
        .iter()
        .find(|addr| addr.is_ipv4())
        .or_else(|| addresses.first())
        .copied()
        .unwrap_or(IpAddr::from([127, 0, 0, 1]));
    match ip {
        IpAddr::V4(ip) => format!("{ip}:{port}"),
        IpAddr::V6(ip) => format!("[{ip}]:{port}"),
    }
}

pub fn sort_ips(mut ips: Vec<IpAddr>) -> Vec<IpAddr> {
    ips.sort_by_key(|ip| (!ip.is_ipv4(), ip.to_string()));
    ips
}

pub fn trim_service_name(fullname: &str) -> String {
    fullname
        .trim_end_matches(".local.")
        .trim_end_matches("._udp")
        .trim_end_matches("._tcp")
        .split('.')
        .next()
        .unwrap_or(fullname)
        .to_string()
}

pub fn parse_vendor_product(raw: Option<&str>) -> (Option<String>, Option<String>) {
    match raw {
        Some(value) => {
            let mut parts = value.split('+');
            (
                parts.next().map(str::to_string),
                parts.next().map(str::to_string),
            )
        }
        None => (None, None),
    }
}

pub fn parse_device_type_name(raw: &str) -> Option<String> {
    let parsed = raw
        .strip_prefix("0x")
        .map(|hex| u32::from_str_radix(hex, 16))
        .unwrap_or_else(|| raw.parse());
    parsed.ok().map(|id| {
        dt_names::get_device_type_name(id)
            .map(str::to_string)
            .unwrap_or_else(|| format!("0x{id:04X}"))
    })
}

pub fn format_device_types(device_types: &[String]) -> String {
    if device_types.is_empty() {
        "unknown type".to_string()
    } else {
        device_types.join(", ")
    }
}

pub fn upsert_known_device(devices: &mut Vec<KnownDevice>, device: KnownDevice) {
    if let Some(existing) = devices.iter_mut().find(|d| d.node_id == device.node_id) {
        *existing = device;
    } else {
        devices.push(device);
    }
}

pub fn upsert_endpoint_alias(aliases: &mut Vec<EndpointAlias>, alias: EndpointAlias) {
    if let Some(existing) = aliases
        .iter_mut()
        .find(|a| a.node_id == alias.node_id && a.endpoint_id == alias.endpoint_id)
    {
        *existing = alias;
    } else {
        aliases.push(alias);
    }
}

pub fn apply_endpoint_aliases(state: &AppState, node_id: u64, endpoints: &mut [EndpointSummary]) {
    for endpoint in endpoints {
        if let Some(alias) = state
            .endpoint_aliases
            .iter()
            .find(|a| a.node_id == node_id && a.endpoint_id == endpoint.id)
        {
            endpoint.label = Some(alias.label.clone());
        }
    }
}

pub fn clamp_selection(current: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        current.min(len - 1)
    }
}

pub fn next_index(current: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (current + 1).min(len - 1)
    }
}
