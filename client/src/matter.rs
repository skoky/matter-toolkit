use anyhow::{Context, Result};
use matc::{
    certmanager::{self, CertManager, FileCertManager},
    clusters::{codec, defs, dt_names},
    controller, transport,
};
use std::{
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use crate::state::AppState;
use crate::types::{ActionDialog, ActionOption, CommissionableDevice, EndpointSummary, PendingCommission};
use crate::utils::first_socket_addr;

pub const CERT_PATH: &str = "./pem";
const DEFAULT_FABRIC_ID: u64 = 0x110;
const DEFAULT_LOCAL_ADDRESS: &str = "0.0.0.0:0";

pub fn ensure_credentials(state: &AppState) -> Result<()> {
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

pub async fn start_commission(
    state: &AppState,
    device: &CommissionableDevice,
    pin: u32,
) -> Result<PendingCommission> {
    ensure_credentials(state)?;

    let cm: Arc<dyn CertManager> = certmanager::FileCertManager::load(CERT_PATH)?;
    let transport = transport::Transport::new(DEFAULT_LOCAL_ADDRESS).await?;
    let controller = controller::Controller::new(&cm, &transport, cm.get_fabric_id())?;
    let address = first_socket_addr(&device.addresses, device.port);
    let connection = transport.create_connection(&address).await;
    let node_id = state.next_node_id();

    let mut connection = controller
        .commission(&connection, pin, node_id, state.controller_id)
        .await?;

    let detected_label = detect_device_label(&mut connection, 0)
        .await
        .unwrap_or_else(|| device.display_name.clone());
    let endpoints = load_endpoints(&mut connection).await.unwrap_or_default();

    Ok(PendingCommission {
        connection,
        node_id,
        address,
        device_label: detected_label,
        fabric_label: state.fabric_label.clone(),
        endpoints,
    })
}

pub async fn connect_known_device(
    state: &AppState,
    device: &crate::state::KnownDevice,
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

pub async fn update_fabric_label(
    connection: &mut controller::Connection,
    label: &str,
) -> Result<()> {
    let payload =
        codec::operational_credential_cluster::encode_update_fabric_label(label.to_string())?;
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

pub async fn decommission_device(connection: &mut controller::Connection) -> Result<()> {
    let current_fabric = connection
        .read_request2(
            0,
            defs::CLUSTER_ID_OPERATIONAL_CREDENTIALS,
            defs::CLUSTER_OPERATIONAL_CREDENTIALS_ATTR_ID_CURRENTFABRICINDEX,
        )
        .await?;
    let fabric_index =
        codec::operational_credential_cluster::decode_current_fabric_index(&current_fabric)?;
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

pub async fn load_endpoints(
    connection: &mut controller::Connection,
) -> Result<Vec<EndpointSummary>> {
    let mut endpoint_ids = vec![0u16];
    if let Ok(parts) = connection
        .read_request2(
            0,
            defs::CLUSTER_ID_DESCRIPTOR,
            defs::CLUSTER_DESCRIPTOR_ATTR_ID_PARTSLIST,
        )
        .await
    {
        for endpoint_id in codec::descriptor_cluster::decode_parts_list(&parts)? {
            if !endpoint_ids.contains(&endpoint_id) {
                endpoint_ids.push(endpoint_id);
            }
        }
    }

    let mut out = Vec::new();
    for endpoint_id in endpoint_ids {
        if let Ok(summary) = load_endpoint_summary(connection, endpoint_id).await {
            out.push(summary);
        }
    }

    Ok(out)
}

async fn load_endpoint_summary(
    connection: &mut controller::Connection,
    endpoint_id: u16,
) -> Result<EndpointSummary> {
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
        .collect();

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

    Ok(EndpointSummary {
        id: endpoint_id,
        label,
        device_types,
        has_on_off: clusters.contains(&defs::CLUSTER_ID_ON_OFF),
        actions,
    })
}

pub async fn detect_device_label(
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

pub fn build_action_dialog(
    endpoint: &EndpointSummary,
    endpoint_index: usize,
) -> Result<ActionDialog> {
    let mut options = Vec::new();
    for action in &endpoint.actions {
        let action_id = action.action_id.context("action missing action_id")?;
        let invoke_id = next_invoke_id();
        let name = action
            .name
            .clone()
            .unwrap_or_else(|| "unnamed".to_string());

        let action_id_u16 = action_id as u16;
        let entries: &[(&str, u32, fn(u16, u32) -> Result<Vec<u8>>)] = &[
            ("Instant", defs::CLUSTER_ACTIONS_CMD_ID_INSTANTACTION, |a, i| {
                codec::actions_cluster::encode_instant_action(a, Some(i))
            }),
            ("Start", defs::CLUSTER_ACTIONS_CMD_ID_STARTACTION, |a, i| {
                codec::actions_cluster::encode_start_action(a, Some(i))
            }),
            ("Stop", defs::CLUSTER_ACTIONS_CMD_ID_STOPACTION, |a, i| {
                codec::actions_cluster::encode_stop_action(a, Some(i))
            }),
            ("Pause", defs::CLUSTER_ACTIONS_CMD_ID_PAUSEACTION, |a, i| {
                codec::actions_cluster::encode_pause_action(a, Some(i))
            }),
            ("Resume", defs::CLUSTER_ACTIONS_CMD_ID_RESUMEACTION, |a, i| {
                codec::actions_cluster::encode_resume_action(a, Some(i))
            }),
            ("Enable", defs::CLUSTER_ACTIONS_CMD_ID_ENABLEACTION, |a, i| {
                codec::actions_cluster::encode_enable_action(a, Some(i))
            }),
            ("Disable", defs::CLUSTER_ACTIONS_CMD_ID_DISABLEACTION, |a, i| {
                codec::actions_cluster::encode_disable_action(a, Some(i))
            }),
        ];

        for (verb, command_id, encode) in entries {
            options.push(ActionOption {
                label: format!("{verb}: {name}"),
                command_id: *command_id,
                payload: encode(action_id_u16, invoke_id)?,
            });
        }
    }

    Ok(ActionDialog {
        title: format!("Endpoint {} Actions", endpoint.id),
        endpoint_index,
        options,
        selected: 0,
    })
}

fn next_invoke_id() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u32)
        .unwrap_or(1)
}
