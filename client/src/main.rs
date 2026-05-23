use anyhow::{anyhow, Context, Result};
use flume::RecvTimeoutError;
use matc::{
    certmanager::{self, CertManager, FileCertManager},
    clusters::{codec, defs, dt_names},
    controller, transport,
};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    net::IpAddr,
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const CERT_PATH: &str = "./pem";
const STATE_PATH: &str = "./client-state.txt";
const DEFAULT_FABRIC_ID: u64 = 0x110;
const DEFAULT_CONTROLLER_ID: u64 = 100;
const DEFAULT_LOCAL_ADDRESS: &str = "0.0.0.0:0";
const SCAN_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
struct AppState {
    controller_id: u64,
    fabric_label: String,
    devices: Vec<KnownDevice>,
    endpoint_aliases: Vec<EndpointAlias>,
}

#[derive(Clone, Debug)]
struct KnownDevice {
    label: String,
    node_id: u64,
    last_address: String,
}

#[derive(Clone, Debug)]
struct EndpointAlias {
    node_id: u64,
    endpoint_id: u16,
    label: String,
}

#[derive(Clone, Debug)]
struct CommissionableDevice {
    display_name: String,
    device_type: String,
    addresses: Vec<IpAddr>,
    port: u16,
    discriminator: Option<String>,
    vendor_id: Option<String>,
    product_id: Option<String>,
}

#[derive(Clone, Debug)]
struct CommissionedDevice {
    display_name: String,
    addresses: Vec<IpAddr>,
    port: u16,
    known: Option<KnownDevice>,
}

#[derive(Debug)]
struct EndpointSummary {
    id: u16,
    label: Option<String>,
    device_types: Vec<String>,
    has_on_off: bool,
    actions: Vec<codec::actions_cluster::Action>,
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
    fn load(path: &str) -> Result<Self> {
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

    fn save(&self, path: &str) -> Result<()> {
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

    fn next_node_id(&self) -> u64 {
        self.devices
            .iter()
            .map(|device| device.node_id)
            .max()
            .unwrap_or(0x1000)
            + 1
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut state = AppState::load(STATE_PATH)?;
    ensure_credentials(&state)?;

    loop {
        let (commissionable, commissioned) = scan_network(&state)?;
        print_overview(&state, &commissionable, &commissioned);

        match prompt("Select: [r]efresh, [c]ommission, [m]anage, [q]uit")?.as_str() {
            "q" | "quit" => break,
            "r" | "refresh" => continue,
            "c" | "commission" => {
                if let Err(err) = commission_flow(&mut state, &commissionable).await {
                    println!("Commissioning failed: {err:#}");
                    wait_for_enter()?;
                }
            }
            "m" | "manage" => {
                if let Err(err) = manage_flow(&mut state, &commissioned).await {
                    println!("Device operation failed: {err:#}");
                    wait_for_enter()?;
                }
            }
            _ => {
                println!("Unknown command.");
                wait_for_enter()?;
            }
        }
    }

    Ok(())
}

fn print_overview(
    state: &AppState,
    commissionable: &[CommissionableDevice],
    commissioned: &[CommissionedDevice],
) {
    clear_screen();
    println!("Matter terminal client");
    println!("Controller ID: {}", state.controller_id);
    println!("Fabric Label: {}", state.fabric_label);
    println!();

    println!("Commissionable devices");
    if commissionable.is_empty() {
        println!("  none found");
    } else {
        for (idx, device) in commissionable.iter().enumerate() {
            println!(
                "  [{}] {} | type: {} | addr: {}",
                idx + 1,
                device.display_name,
                device.device_type,
                first_socket_addr(&device.addresses, device.port)
            );
            if device.discriminator.is_some()
                || device.vendor_id.is_some()
                || device.product_id.is_some()
            {
                println!(
                    "      discriminator={:?} vendor={:?} product={:?}",
                    device.discriminator, device.vendor_id, device.product_id
                );
            }
        }
    }

    println!();
    println!("Commissioned devices");
    if commissioned.is_empty() {
        println!("  none found");
    } else {
        for (idx, device) in commissioned.iter().enumerate() {
            let status = match &device.known {
                Some(known) => format!("managed, node_id={}", known.node_id),
                None => "discovered only".to_string(),
            };
            let name = device
                .known
                .as_ref()
                .map(|known| known.label.as_str())
                .unwrap_or(device.display_name.as_str());
            println!(
                "  [{}] {} | addr: {} | {}",
                idx + 1,
                name,
                first_socket_addr(&device.addresses, device.port),
                status
            );
            if device.known.is_some() && device.display_name != name {
                println!("      service: {}", device.display_name);
            }
        }
    }

    if !state.devices.is_empty() {
        println!();
        println!("Saved devices");
        for device in &state.devices {
            println!(
                "  {} | node_id={} | last_addr={}",
                device.label, device.node_id, device.last_address
            );
            let aliases = state
                .endpoint_aliases
                .iter()
                .filter(|alias| alias.node_id == device.node_id)
                .collect::<Vec<_>>();
            if !aliases.is_empty() {
                let summary = aliases
                    .iter()
                    .map(|alias| format!("ep{}={}", alias.endpoint_id, alias.label))
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("      endpoints: {summary}");
            }
        }
    }

    println!();
}

async fn commission_flow(state: &mut AppState, devices: &[CommissionableDevice]) -> Result<()> {
    if devices.is_empty() {
        return Err(anyhow!("no commissionable devices found"));
    }

    let index = select_index("Commission which device", devices.len())?;
    let device = devices[index].clone();
    let setup_code = prompt(
        "Matter setup code (8-digit passcode, 11-digit/xxxx-xxx-xxxx manual code, or MT: QR payload)",
    )?;
    let pin = parse_setup_code(&setup_code)?;

    ensure_credentials(state)?;

    let cm: Arc<dyn CertManager> = certmanager::FileCertManager::load(CERT_PATH)?;
    let transport = transport::Transport::new(DEFAULT_LOCAL_ADDRESS).await?;
    let controller = controller::Controller::new(&cm, &transport, cm.get_fabric_id())?;
    let address = first_socket_addr(&device.addresses, device.port);
    let connection = transport.create_connection(&address).await;
    let node_id = state.next_node_id();

    println!("Commissioning {} at {}...", device.display_name, address);
    let mut session = controller
        .commission(&connection, pin, node_id, state.controller_id)
        .await?;

    let detected_label = detect_device_label(&mut session, 0)
        .await
        .unwrap_or_else(|| device.display_name.clone());
    let label = prompt_with_default("Device name", &detected_label)?;
    let fabric_label = prompt_with_default("Fabric name", &state.fabric_label)?;
    update_fabric_label(&mut session, &fabric_label).await?;
    state.fabric_label = fabric_label;

    upsert_known_device(
        &mut state.devices,
        KnownDevice {
            label: label.clone(),
            node_id,
            last_address: address.clone(),
        },
    );
    state.save(STATE_PATH)?;

    println!("Commissioned {label} with node_id={node_id}.");
    let mut endpoints = load_endpoints(&mut session).await.unwrap_or_default();
    apply_endpoint_aliases(state, node_id, &mut endpoints);
    if !endpoints.is_empty() {
        prompt_for_endpoint_aliases(state, node_id, &mut endpoints)?;
        state.save(STATE_PATH)?;
        println!("Endpoints:");
        for endpoint in endpoints {
            println!(
                "  endpoint {} | {} | {}",
                endpoint.id,
                endpoint
                    .label
                    .clone()
                    .unwrap_or_else(|| "unnamed".to_string()),
                format_device_types(&endpoint.device_types)
            );
        }
    }
    wait_for_enter()?;
    Ok(())
}

async fn manage_flow(state: &mut AppState, devices: &[CommissionedDevice]) -> Result<()> {
    let mut choices: Vec<KnownDevice> = devices
        .iter()
        .filter_map(|device| device.known.clone())
        .collect();
    if choices.is_empty() {
        choices = state.devices.clone();
    }
    if choices.is_empty() {
        return Err(anyhow!("no managed commissioned devices available"));
    }

    println!("Managed devices:");
    for (idx, device) in choices.iter().enumerate() {
        println!(
            "  [{}] {} | node_id={} | addr={}",
            idx + 1,
            device.label,
            device.node_id,
            device.last_address
        );
    }

    let index = select_index("Open which device", choices.len())?;
    let mut device = choices[index].clone();
    let current_address = devices
        .iter()
        .filter_map(|entry| entry.known.as_ref().map(|known| (entry, known)))
        .find(|(_, known)| known.node_id == device.node_id)
        .map(|(entry, _)| first_socket_addr(&entry.addresses, entry.port))
        .unwrap_or_else(|| device.last_address.clone());
    device.last_address = current_address.clone();

    let mut connection = connect_known_device(state, &device).await?;
    let mut endpoints = load_endpoints(&mut connection).await?;
    apply_endpoint_aliases(state, device.node_id, &mut endpoints);

    loop {
        clear_screen();
        println!(
            "{} | node_id={} | addr={}",
            device.label, device.node_id, device.last_address
        );
        println!();

        if endpoints.is_empty() {
            println!("No endpoints discovered.");
        } else {
            for endpoint in &endpoints {
                println!(
                    "  endpoint {} | {} | {}{}{}",
                    endpoint.id,
                    endpoint
                        .label
                        .clone()
                        .unwrap_or_else(|| "unnamed".to_string()),
                    format_device_types(&endpoint.device_types),
                    if endpoint.has_on_off { " | on/off" } else { "" },
                    if endpoint.actions.is_empty() {
                        ""
                    } else {
                        " | actions"
                    }
                );
            }
        }

        println!();
        match prompt("Select: [e]ndpoint action, [f]abric name, [x] decommission, [b]ack")?
            .as_str()
        {
            "b" | "back" => break,
            "e" | "endpoint" => {
                endpoint_action_flow(&mut connection, &endpoints).await?;
            }
            "f" | "fabric" => {
                let new_label = prompt_with_default("Fabric name", &state.fabric_label)?;
                update_fabric_label(&mut connection, &new_label).await?;
                state.fabric_label = new_label;
                state.save(STATE_PATH)?;
                println!("Fabric label updated.");
                wait_for_enter()?;
            }
            "x" | "decommission" => {
                decommission_device(&mut connection).await?;
                state.devices.retain(|saved| saved.node_id != device.node_id);
                state.endpoint_aliases.retain(|alias| alias.node_id != device.node_id);
                state.save(STATE_PATH)?;
                println!("Device decommissioned from this controller.");
                wait_for_enter()?;
                break;
            }
            _ => {
                println!("Unknown command.");
                wait_for_enter()?;
            }
        }
    }

    Ok(())
}

async fn endpoint_action_flow(
    connection: &mut controller::Connection,
    endpoints: &[EndpointSummary],
) -> Result<()> {
    if endpoints.is_empty() {
        return Err(anyhow!("no endpoints available"));
    }

    let index = select_index("Choose endpoint", endpoints.len())?;
    let endpoint = &endpoints[index];

    println!("Endpoint {} selected.", endpoint.id);
    if endpoint.has_on_off {
        println!("  [1] On");
        println!("  [2] Off");
    }
    if !endpoint.actions.is_empty() {
        println!("  [3] Actions cluster command");
    }

    let choice = prompt("Action")?;
    match choice.as_str() {
        "1" if endpoint.has_on_off => {
            connection
                .invoke_request(
                    endpoint.id,
                    defs::CLUSTER_ID_ON_OFF,
                    defs::CLUSTER_ON_OFF_CMD_ID_ON,
                    &[],
                )
                .await?;
            println!("On command sent.");
        }
        "2" if endpoint.has_on_off => {
            connection
                .invoke_request(
                    endpoint.id,
                    defs::CLUSTER_ID_ON_OFF,
                    defs::CLUSTER_ON_OFF_CMD_ID_OFF,
                    &[],
                )
                .await?;
            println!("Off command sent.");
        }
        "3" if !endpoint.actions.is_empty() => {
            invoke_cluster_action(connection, endpoint).await?;
        }
        _ => return Err(anyhow!("unsupported action selection")),
    }

    wait_for_enter()?;
    Ok(())
}

async fn invoke_cluster_action(
    connection: &mut controller::Connection,
    endpoint: &EndpointSummary,
) -> Result<()> {
    for (idx, action) in endpoint.actions.iter().enumerate() {
        println!(
            "  [{}] {} (action_id={})",
            idx + 1,
            action.name.clone().unwrap_or_else(|| "unnamed".to_string()),
            action.action_id.unwrap_or_default()
        );
    }
    let action_index = select_index("Choose action", endpoint.actions.len())?;
    let action = &endpoint.actions[action_index];
    let action_id = action
        .action_id
        .context("selected action does not have action_id")?;

    println!("  [1] Instant");
    println!("  [2] Start");
    println!("  [3] Stop");
    println!("  [4] Pause");
    println!("  [5] Resume");
    println!("  [6] Enable");
    println!("  [7] Disable");

    let invoke_id = next_invoke_id();
    let selection = prompt("Action command")?;
    let (command_id, payload) = match selection.as_str() {
        "1" => (
            defs::CLUSTER_ACTIONS_CMD_ID_INSTANTACTION,
            codec::actions_cluster::encode_instant_action(action_id, invoke_id)?,
        ),
        "2" => (
            defs::CLUSTER_ACTIONS_CMD_ID_STARTACTION,
            codec::actions_cluster::encode_start_action(action_id, invoke_id)?,
        ),
        "3" => (
            defs::CLUSTER_ACTIONS_CMD_ID_STOPACTION,
            codec::actions_cluster::encode_stop_action(action_id, invoke_id)?,
        ),
        "4" => (
            defs::CLUSTER_ACTIONS_CMD_ID_PAUSEACTION,
            codec::actions_cluster::encode_pause_action(action_id, invoke_id)?,
        ),
        "5" => (
            defs::CLUSTER_ACTIONS_CMD_ID_RESUMEACTION,
            codec::actions_cluster::encode_resume_action(action_id, invoke_id)?,
        ),
        "6" => (
            defs::CLUSTER_ACTIONS_CMD_ID_ENABLEACTION,
            codec::actions_cluster::encode_enable_action(action_id, invoke_id)?,
        ),
        "7" => (
            defs::CLUSTER_ACTIONS_CMD_ID_DISABLEACTION,
            codec::actions_cluster::encode_disable_action(action_id, invoke_id)?,
        ),
        _ => return Err(anyhow!("unsupported action command")),
    };

    connection
        .invoke_request(endpoint.id, defs::CLUSTER_ID_ACTIONS, command_id, &payload)
        .await?;
    println!("Actions cluster command sent.");
    Ok(())
}

async fn decommission_device(connection: &mut controller::Connection) -> Result<()> {
    let current_fabric = connection
        .read_request2(
            0,
            defs::CLUSTER_ID_OPERATIONAL_CREDENTIALS,
            defs::CLUSTER_OPERATIONAL_CREDENTIALS_ATTR_ID_CURRENTFABRICINDEX,
        )
        .await?;
    let fabric_index = codec::operational_credential_cluster::decode_current_fabric_index(
        &current_fabric,
    )?;
    let payload = codec::operational_credential_cluster::encode_remove_fabric(fabric_index)?;
    connection
        .invoke_request(
            0,
            defs::CLUSTER_ID_OPERATIONAL_CREDENTIALS,
            defs::CLUSTER_OPERATIONAL_CREDENTIALS_CMD_ID_REMOVEFABRIC,
            &payload,
        )
        .await?;
    Ok(())
}

async fn update_fabric_label(
    connection: &mut controller::Connection,
    label: &str,
) -> Result<()> {
    let payload = codec::operational_credential_cluster::encode_update_fabric_label(
        label.to_string(),
    )?;
    connection
        .invoke_request(
            0,
            defs::CLUSTER_ID_OPERATIONAL_CREDENTIALS,
            defs::CLUSTER_OPERATIONAL_CREDENTIALS_CMD_ID_UPDATEFABRICLABEL,
            &payload,
        )
        .await?;
    Ok(())
}

async fn connect_known_device(
    state: &AppState,
    device: &KnownDevice,
) -> Result<controller::Connection> {
    ensure_credentials(state)?;
    let cm: Arc<dyn CertManager> = certmanager::FileCertManager::load(CERT_PATH)?;
    let transport = transport::Transport::new(DEFAULT_LOCAL_ADDRESS).await?;
    let controller = controller::Controller::new(&cm, &transport, cm.get_fabric_id())?;
    let connection = transport.create_connection(&device.last_address).await;
    controller
        .auth_sigma(&connection, device.node_id, state.controller_id)
        .await
}

async fn load_endpoints(connection: &mut controller::Connection) -> Result<Vec<EndpointSummary>> {
    let parts = connection
        .read_request2(
            0,
            defs::CLUSTER_ID_DESCRIPTOR,
            defs::CLUSTER_DESCRIPTOR_ATTR_ID_PARTSLIST,
        )
        .await?;
    let mut out = Vec::new();
    for endpoint_id in codec::descriptor_cluster::decode_parts_list(&parts)? {
        let server_list = connection
            .read_request2(
                endpoint_id,
                defs::CLUSTER_ID_DESCRIPTOR,
                defs::CLUSTER_DESCRIPTOR_ATTR_ID_SERVERLIST,
            )
            .await?;
        let clusters = codec::descriptor_cluster::decode_server_list(&server_list)?;

        let device_types_raw = connection
            .read_request2(
                endpoint_id,
                defs::CLUSTER_ID_DESCRIPTOR,
                defs::CLUSTER_DESCRIPTOR_ATTR_ID_DEVICETYPELIST,
            )
            .await?;
        let device_types = codec::descriptor_cluster::decode_device_type_list(&device_types_raw)?
            .into_iter()
            .filter_map(|item| item.device_type)
            .map(|id| {
                dt_names::get_device_type_name(id)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("0x{id:04X}"))
            })
            .collect::<Vec<_>>();

        let label = detect_device_label(connection, endpoint_id).await;
        let actions = if clusters.contains(&defs::CLUSTER_ID_ACTIONS) {
            let tlv = connection
                .read_request2(
                    endpoint_id,
                    defs::CLUSTER_ID_ACTIONS,
                    defs::CLUSTER_ACTIONS_ATTR_ID_ACTIONLIST,
                )
                .await?;
            codec::actions_cluster::decode_action_list(&tlv)?
        } else {
            Vec::new()
        };

        out.push(EndpointSummary {
            id: endpoint_id,
            label,
            device_types,
            has_on_off: clusters.contains(&defs::CLUSTER_ID_ON_OFF),
            actions,
        });
    }
    Ok(out)
}

async fn detect_device_label(
    connection: &mut controller::Connection,
    endpoint_id: u16,
) -> Option<String> {
    let bridged = connection
        .read_request2(
            endpoint_id,
            defs::CLUSTER_ID_BRIDGED_DEVICE_BASIC_INFORMATION,
            defs::CLUSTER_BRIDGED_DEVICE_BASIC_INFORMATION_ATTR_ID_NODELABEL,
        )
        .await
        .ok();
    if let Some(matc::tlv::TlvItemValue::String(label)) = bridged {
        if !label.is_empty() {
            return Some(label);
        }
    }

    let basic = connection
        .read_request2(
            endpoint_id,
            defs::CLUSTER_ID_BASIC_INFORMATION,
            defs::CLUSTER_BASIC_INFORMATION_ATTR_ID_NODELABEL,
        )
        .await
        .ok();
    if let Some(matc::tlv::TlvItemValue::String(label)) = basic {
        if !label.is_empty() {
            return Some(label);
        }
    }

    None
}

fn scan_network(state: &AppState) -> Result<(Vec<CommissionableDevice>, Vec<CommissionedDevice>)> {
    let commissionable = browse_devices("_matterc._udp.local.", SCAN_TIMEOUT)?
        .into_iter()
        .map(to_commissionable_device)
        .collect::<Vec<_>>();

    let commissioned = browse_devices("_matter._tcp.local.", SCAN_TIMEOUT)?
        .into_iter()
        .map(|info| to_commissioned_device(info, state))
        .collect::<Vec<_>>();

    Ok((commissionable, commissioned))
}

fn browse_devices(service_type: &str, timeout: Duration) -> Result<Vec<ServiceInfo>> {
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
                devices.insert(info.get_fullname().to_string(), info);
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

fn to_commissionable_device(info: ServiceInfo) -> CommissionableDevice {
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
        addresses: sort_ips(info.get_addresses().iter().copied().collect()),
        port: info.get_port(),
        discriminator: info.get_property_val_str("D").map(str::to_string),
        vendor_id,
        product_id,
    }
}

fn to_commissioned_device(info: ServiceInfo, state: &AppState) -> CommissionedDevice {
    let address = first_socket_addr(
        &sort_ips(info.get_addresses().iter().copied().collect()),
        info.get_port(),
    );
    let known = state
        .devices
        .iter()
        .find(|device| device.last_address == address)
        .cloned();

    CommissionedDevice {
        display_name: trim_service_name(info.get_fullname()),
        addresses: sort_ips(info.get_addresses().iter().copied().collect()),
        port: info.get_port(),
        known,
    }
}

fn ensure_credentials(state: &AppState) -> Result<()> {
    if !Path::new(CERT_PATH).exists() {
        FileCertManager::new(DEFAULT_FABRIC_ID, CERT_PATH).bootstrap()?;
    }

    let cm = FileCertManager::load(CERT_PATH)?;
    let cert_path = format!("{CERT_PATH}/{}-cert.pem", state.controller_id);
    let key_path = format!("{CERT_PATH}/{}-private.pem", state.controller_id);
    if !Path::new(&cert_path).exists() || !Path::new(&key_path).exists() {
        cm.create_user(state.controller_id)?;
    }

    Ok(())
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

fn prompt_with_default(label: &str, default: &str) -> Result<String> {
    let value = prompt(&format!("{label} [{default}]"))?;
    if value.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(value)
    }
}

fn select_index(label: &str, len: usize) -> Result<usize> {
    let raw = prompt(label)?;
    let index: usize = raw.parse().context("invalid number")?;
    if index == 0 || index > len {
        return Err(anyhow!("selection out of range"));
    }
    Ok(index - 1)
}

fn wait_for_enter() -> Result<()> {
    let _ = prompt("Press Enter")?;
    Ok(())
}

fn first_socket_addr(addresses: &[IpAddr], port: u16) -> String {
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

fn sort_ips(mut ips: Vec<IpAddr>) -> Vec<IpAddr> {
    ips.sort_by_key(|ip| (!ip.is_ipv4(), ip.to_string()));
    ips
}

fn trim_service_name(fullname: &str) -> String {
    fullname
        .trim_end_matches(".local.")
        .trim_end_matches("._udp")
        .trim_end_matches("._tcp")
        .split('.')
        .next()
        .unwrap_or(fullname)
        .to_string()
}

fn parse_vendor_product(raw: Option<&str>) -> (Option<String>, Option<String>) {
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

fn parse_device_type_name(raw: &str) -> Option<String> {
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

fn upsert_known_device(devices: &mut Vec<KnownDevice>, device: KnownDevice) {
    if let Some(existing) = devices.iter_mut().find(|saved| saved.node_id == device.node_id) {
        *existing = device;
    } else {
        devices.push(device);
    }
}

fn apply_endpoint_aliases(state: &AppState, node_id: u64, endpoints: &mut [EndpointSummary]) {
    for endpoint in endpoints {
        if let Some(alias) = state
            .endpoint_aliases
            .iter()
            .find(|alias| alias.node_id == node_id && alias.endpoint_id == endpoint.id)
        {
            endpoint.label = Some(alias.label.clone());
        }
    }
}

fn prompt_for_endpoint_aliases(
    state: &mut AppState,
    node_id: u64,
    endpoints: &mut [EndpointSummary],
) -> Result<()> {
    println!("Endpoint naming");
    println!("Press Enter to keep the detected name.");
    for endpoint in endpoints {
        let detected = endpoint
            .label
            .clone()
            .unwrap_or_else(|| format_device_types(&endpoint.device_types));
        let answer = prompt_with_default(&format!("Endpoint {} name", endpoint.id), &detected)?;
        if !answer.is_empty() {
            endpoint.label = Some(answer.clone());
            upsert_endpoint_alias(
                &mut state.endpoint_aliases,
                EndpointAlias {
                    node_id,
                    endpoint_id: endpoint.id,
                    label: answer,
                },
            );
        } else if let Some(label) = endpoint.label.clone() {
            upsert_endpoint_alias(
                &mut state.endpoint_aliases,
                EndpointAlias {
                    node_id,
                    endpoint_id: endpoint.id,
                    label,
                },
            );
        }
    }
    Ok(())
}

fn upsert_endpoint_alias(aliases: &mut Vec<EndpointAlias>, alias: EndpointAlias) {
    if let Some(existing) = aliases
        .iter_mut()
        .find(|saved| saved.node_id == alias.node_id && saved.endpoint_id == alias.endpoint_id)
    {
        *existing = alias;
    } else {
        aliases.push(alias);
    }
}

fn format_device_types(device_types: &[String]) -> String {
    if device_types.is_empty() {
        "unknown type".to_string()
    } else {
        device_types.join(", ")
    }
}

fn next_invoke_id() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u32)
        .unwrap_or(1)
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

fn clear_screen() {
    print!("\x1B[2J\x1B[H");
    let _ = io::stdout().flush();
}

fn parse_setup_code(input: &str) -> Result<u32> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("setup code is empty"));
    }

    if let Some(qr_payload) = extract_qr_payload(trimmed) {
        return decode_qr_payload_passcode(&qr_payload);
    }

    let numeric = trimmed.replace('-', "").replace(' ', "");
    if numeric.len() == 8 && numeric.chars().all(|ch| ch.is_ascii_digit()) {
        return numeric
            .parse::<u32>()
            .context("invalid 8-digit setup passcode");
    }

    if (numeric.len() == 11 || numeric.len() == 21) && numeric.chars().all(|ch| ch.is_ascii_digit())
    {
        return matc::onboarding::decode_manual_pairing_code(&numeric)
            .map(|info| info.passcode)
            .context("invalid manual pairing code");
    }

    Err(anyhow!(
        "unsupported setup code format; use an 8-digit passcode, manual pairing code, or MT: QR payload"
    ))
}

fn extract_qr_payload(input: &str) -> Option<String> {
    if input.starts_with("MT:") {
        return Some(input.to_string());
    }

    if let Some(pos) = input.find("MT%3A") {
        let encoded = &input[pos..];
        let end = encoded.find('&').unwrap_or(encoded.len());
        let token = &encoded[..end];
        return Some(token.replacen("MT%3A", "MT:", 1));
    }

    if let Some(pos) = input.find("MT:") {
        let tail = &input[pos..];
        let end = tail.find('&').unwrap_or(tail.len());
        return Some(tail[..end].to_string());
    }

    None
}

fn decode_qr_payload_passcode(payload: &str) -> Result<u32> {
    let encoded = payload
        .strip_prefix("MT:")
        .context("QR payload must start with MT:")?;
    let bytes = base38_decode(encoded)?;
    if bytes.len() < 11 {
        return Err(anyhow!("QR payload is too short"));
    }

    let packed = bytes
        .iter()
        .take(16)
        .enumerate()
        .fold(0u128, |acc, (idx, byte)| acc | ((*byte as u128) << (idx * 8)));

    let _version = extract_bits(packed, 0, 3) as u8;
    let _vendor_id = extract_bits(packed, 3, 16) as u16;
    let _product_id = extract_bits(packed, 19, 16) as u16;
    let _commissioning_flow = extract_bits(packed, 35, 2) as u8;
    let _discovery_capabilities = extract_bits(packed, 37, 8) as u8;
    let _discriminator = extract_bits(packed, 45, 12) as u16;
    let passcode = extract_bits(packed, 57, 27) as u32;

    Ok(passcode)
}

fn extract_bits(value: u128, offset: u32, width: u32) -> u128 {
    (value >> offset) & ((1u128 << width) - 1)
}

fn base38_decode(input: &str) -> Result<Vec<u8>> {
    const ALPHABET: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-.";

    let chars = input.as_bytes();
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        let remaining = chars.len() - index;
        let (chunk_len, output_len) = if remaining >= 5 {
            (5, 3)
        } else if remaining == 4 {
            (4, 2)
        } else if remaining == 2 {
            (2, 1)
        } else {
            return Err(anyhow!("invalid base38 payload length"));
        };

        let mut value = 0u32;
        let mut factor = 1u32;
        for ch in &chars[index..index + chunk_len] {
            let chr = *ch as char;
            let digit = ALPHABET
                .find(chr)
                .with_context(|| format!("invalid base38 character: {chr}"))? as u32;
            value = value
                .checked_add(digit.saturating_mul(factor))
                .context("base38 overflow")?;
            factor = factor.checked_mul(38).context("base38 overflow")?;
        }

        for _ in 0..output_len {
            out.push((value & 0xFF) as u8);
            value >>= 8;
        }

        index += chunk_len;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{base38_decode, decode_qr_payload_passcode, parse_setup_code};

    #[test]
    fn parses_raw_passcode() {
        assert_eq!(parse_setup_code("20202021").unwrap(), 20202021);
    }

    #[test]
    fn parses_manual_pairing_code() {
        assert_eq!(parse_setup_code("34970112332").unwrap(), 20202021);
        assert_eq!(parse_setup_code("2585-103-3238").unwrap(), 54453390);
    }

    #[test]
    fn parses_qr_payload() {
        assert_eq!(
            decode_qr_payload_passcode("MT:Y.K9042C00KA0648G00").unwrap(),
            20202021
        );
    }

    #[test]
    fn parses_qr_payload_from_url() {
        assert_eq!(
            parse_setup_code(
                "https://project-chip.github.io/connectedhomeip/qrcode.html?data=MT%3AY.K9042C00KA0648G00"
            )
            .unwrap(),
            20202021
        );
    }

    #[test]
    fn decodes_base38_known_payload() {
        let bytes = base38_decode("Y.K9042C00KA0648G00").unwrap();
        assert!(bytes.len() >= 11);
    }
}
