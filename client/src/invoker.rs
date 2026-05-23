#![allow(dead_code)]

#[path = "matter.rs"]
mod matter;
#[path = "state.rs"]
mod state;
#[path = "types.rs"]
mod types;
#[path = "utils.rs"]
mod utils;

use anyhow::{bail, Context, Result};
use matc::clusters::defs;
use std::env;

const STATE_PATH: &str = "./client-state.toml";

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let mut args = env::args().skip(1);
    let device_name = args.next().context("missing device name")?;
    let endpoint_name = args.next().context("missing endpoint name")?;
    let action = args.next().context("missing action")?;
    if args.next().is_some() {
        bail!("usage: invoker <device name> <endpoint name> <action>");
    }

    let state = state::AppState::load(STATE_PATH)
        .with_context(|| format!("failed to read {STATE_PATH}"))?;
    let device = state
        .devices
        .iter()
        .find(|device| same_name(&device.label, &device_name))
        .with_context(|| format!("device not found in state: {device_name}"))?
        .clone();
    let endpoint_alias = state
        .endpoint_aliases
        .iter()
        .find(|alias| alias.node_id == device.node_id && same_name(&alias.label, &endpoint_name))
        .with_context(|| {
            format!(
                "endpoint not found in state for device \"{}\": {endpoint_name}",
                device.label
            )
        })?;

    let mut connection = matter::connect_known_device(&state, &device).await?;
    let mut endpoints = matter::load_endpoints(&mut connection).await?;
    utils::apply_endpoint_aliases(&state, device.node_id, &mut endpoints);
    let endpoint = endpoints
        .iter()
        .find(|endpoint| endpoint.id == endpoint_alias.endpoint_id)
        .with_context(|| {
            format!(
                "endpoint {} is saved for \"{}\" but was not found on the device",
                endpoint_alias.endpoint_id, device.label
            )
        })?;

    if let Some(command_id) = on_off_command(endpoint, &action)? {
        connection
            .invoke_request(endpoint.id, defs::CLUSTER_ID_ON_OFF, command_id, &[])
            .await?;
        println!(
            "Sent \"{}\" to {} / {}.",
            action.trim(),
            device.label,
            endpoint_alias.label
        );
        return Ok(());
    }

    let dialog = matter::build_action_dialog(endpoint, 0)?;
    let option = find_action_option(&dialog.options, &action)?;
    connection
        .invoke_request(
            endpoint.id,
            defs::CLUSTER_ID_ACTIONS,
            option.command_id,
            &option.payload,
        )
        .await?;

    println!(
        "Sent \"{}\" to {} / {}.",
        option.label, device.label, endpoint_alias.label
    );
    Ok(())
}

fn on_off_command(endpoint: &types::EndpointSummary, requested: &str) -> Result<Option<u32>> {
    let command_id = match requested.trim().to_ascii_lowercase().as_str() {
        "on" => defs::CLUSTER_ON_OFF_CMD_ID_ON,
        "off" => defs::CLUSTER_ON_OFF_CMD_ID_OFF,
        "toggle" => defs::CLUSTER_ON_OFF_CMD_ID_TOGGLE,
        _ => return Ok(None),
    };

    if !endpoint.has_on_off {
        bail!("endpoint does not support On/Off commands");
    }

    Ok(Some(command_id))
}

fn find_action_option<'a>(
    options: &'a [types::ActionOption],
    requested: &str,
) -> Result<&'a types::ActionOption> {
    if options.is_empty() {
        bail!("endpoint has no Actions cluster entries");
    }

    if let Some(option) = options
        .iter()
        .find(|option| same_name(&option.label, requested))
    {
        return Ok(option);
    }

    let instant_matches: Vec<_> = options
        .iter()
        .filter(|option| {
            let Some((verb, name)) = option.label.split_once(": ") else {
                return false;
            };
            same_name(verb, "Instant") && same_name(name, requested)
        })
        .collect();
    if instant_matches.len() == 1 {
        return Ok(instant_matches[0]);
    }

    let name_matches: Vec<_> = options
        .iter()
        .filter(|option| {
            option
                .label
                .split_once(": ")
                .map(|(_, name)| same_name(name, requested))
                .unwrap_or(false)
        })
        .collect();
    if name_matches.len() == 1 {
        return Ok(name_matches[0]);
    }

    bail!(
        "action not found: {requested}. Available actions: {}",
        options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

fn same_name(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}
