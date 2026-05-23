use anyhow::{anyhow, Context, Result};
use flume::RecvTimeoutError;
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use crate::state::AppState;
use crate::types::{CommissionableDevice, CommissionedDevice};
use crate::utils::{first_socket_addr, parse_device_type_name, parse_vendor_product, sort_ips, trim_service_name};

const SCAN_TIMEOUT: Duration = Duration::from_secs(3);

pub fn scan_network(
    state: &AppState,
) -> Result<(Vec<CommissionableDevice>, Vec<CommissionedDevice>)> {
    let commissionable = browse_devices("_matterc._udp.local.", SCAN_TIMEOUT)?
        .into_iter()
        .map(to_commissionable_device)
        .collect();

    let commissioned = browse_devices("_matter._tcp.local.", SCAN_TIMEOUT)?
        .into_iter()
        .map(|info| to_commissioned_device(info, state))
        .collect();

    Ok((commissionable, commissioned))
}

fn browse_devices(service_type: &str, timeout: Duration) -> Result<Vec<ResolvedService>> {
    let mdns = ServiceDaemon::new().context("failed to create mDNS daemon")?;
    let receiver = mdns
        .browse(service_type)
        .with_context(|| format!("failed to browse {service_type}"))?;

    let started = Instant::now();
    let mut devices = BTreeMap::new();
    while started.elapsed() < timeout {
        let remaining = timeout.saturating_sub(started.elapsed());
        let wait_time = remaining.min(Duration::from_millis(250));
        match receiver.recv_timeout(wait_time) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                devices.insert(info.get_fullname().to_string(), *info);
            }
            Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                devices.remove(&fullname);
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(err) => return Err(anyhow!(err)),
        }
    }

    let _ = mdns.shutdown();
    Ok(devices.into_values().collect())
}

fn to_commissionable_device(info: ResolvedService) -> CommissionableDevice {
    let display_name = info
        .get_property_val_str("DN")
        .map(str::to_string)
        .unwrap_or_else(|| trim_service_name(info.get_fullname()));
    let device_type = info
        .get_property_val_str("DT")
        .and_then(parse_device_type_name)
        .unwrap_or_else(|| "Unknown".to_string());
    let (vendor_id, product_id) = parse_vendor_product(info.get_property_val_str("VP"));

    CommissionableDevice {
        display_name,
        device_type,
        addresses: sort_ips(info.get_addresses().iter().map(|ip| ip.to_ip_addr()).collect()),
        port: info.get_port(),
        discriminator: info.get_property_val_str("D").map(str::to_string),
        vendor_id,
        product_id,
    }
}

fn to_commissioned_device(info: ResolvedService, state: &AppState) -> CommissionedDevice {
    let address = first_socket_addr(
        &sort_ips(info.get_addresses().iter().map(|ip| ip.to_ip_addr()).collect()),
        info.get_port(),
    );
    let known = state
        .devices
        .iter()
        .find(|d| d.last_address == address)
        .cloned();

    CommissionedDevice {
        display_name: trim_service_name(info.get_fullname()),
        addresses: sort_ips(info.get_addresses().iter().map(|ip| ip.to_ip_addr()).collect()),
        port: info.get_port(),
        known,
    }
}
