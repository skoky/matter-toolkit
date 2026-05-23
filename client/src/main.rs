use anyhow::{anyhow, Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use flume::RecvTimeoutError;
use matc::{
    certmanager::{self, CertManager, FileCertManager},
    clusters::{codec, defs, dt_names},
    controller, transport,
};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Stdout},
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

struct ManageState {
    device: KnownDevice,
    connection: controller::Connection,
    endpoints: Vec<EndpointSummary>,
    selected_endpoint: usize,
}

struct PendingCommission {
    connection: controller::Connection,
    node_id: u64,
    address: String,
    device_label: String,
    fabric_label: String,
    endpoints: Vec<EndpointSummary>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FocusPane {
    Commissionable,
    Commissioned,
}

enum Screen {
    Overview,
    Manage(ManageState),
}

enum Modal {
    Input(InputDialog),
    Message(String),
    Confirm(ConfirmDialog),
    Action(ActionDialog),
    CommissionDeviceName {
        pending: PendingCommission,
        value: String,
    },
    CommissionFabricName {
        pending: PendingCommission,
        value: String,
    },
    CommissionEndpointName {
        pending: PendingCommission,
        index: usize,
        value: String,
    },
}

struct InputDialog {
    title: String,
    value: String,
    help: String,
    submit: SubmitAction,
}

struct ConfirmDialog {
    title: String,
    message: String,
    confirm: ConfirmAction,
}

struct ActionDialog {
    title: String,
    endpoint_index: usize,
    options: Vec<ActionOption>,
    selected: usize,
}

struct ActionOption {
    label: String,
    command_id: u32,
    payload: Vec<u8>,
}

enum SubmitAction {
    CommissionSetupCode { device_index: usize },
    RenameEndpoint { endpoint_index: usize },
    ChangeFabricLabel,
}

enum ConfirmAction {
    Decommission,
}

struct App {
    state: AppState,
    screen: Screen,
    focus: FocusPane,
    commissionable: Vec<CommissionableDevice>,
    commissioned: Vec<CommissionedDevice>,
    selected_commissionable: usize,
    selected_commissioned: usize,
    modal: Option<Modal>,
    pending_task: Option<PendingTask>,
    status: String,
    quit: bool,
}

enum PendingTask {
    RefreshScan,
    StartCommission { device_index: usize, setup_code: String },
    OpenSelectedCommissioned,
    EndpointOn { endpoint_index: usize },
    EndpointOff { endpoint_index: usize },
    ChangeFabricLabel { label: String },
    InvokeAction {
        endpoint_index: usize,
        command_id: u32,
        payload: Vec<u8>,
        label: String,
    },
    Decommission,
    FinishCommission(PendingCommission),
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

impl App {
    fn new(state: AppState) -> Self {
        Self {
            state,
            screen: Screen::Overview,
            focus: FocusPane::Commissionable,
            commissionable: Vec::new(),
            commissioned: Vec::new(),
            selected_commissionable: 0,
            selected_commissioned: 0,
            modal: None,
            pending_task: None,
            status: "Startup: r scan | c commission selected commissionable device | m manage selected commissioned device | q quit".to_string(),
            quit: false,
        }
    }

    async fn refresh_scan(&mut self) {
        self.status = "Scanning LAN for Matter devices...".to_string();
        match scan_network(&self.state) {
            Ok((commissionable, commissioned)) => {
                self.commissionable = commissionable;
                self.commissioned = commissioned;
                self.selected_commissionable =
                    clamp_selection(self.selected_commissionable, self.commissionable.len());
                self.selected_commissioned =
                    clamp_selection(self.selected_commissioned, self.commissioned.len());
                self.status = format!(
                    "Found {} commissionable and {} commissioned devices.",
                    self.commissionable.len(),
                    self.commissioned.len()
                );
            }
            Err(err) => {
                self.status = format!("Scan failed: {err}");
            }
        }
    }

    async fn on_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        if self.modal.is_some() {
            return self.on_modal_key(key).await;
        }

        match &self.screen {
            Screen::Overview => self.on_overview_key(key).await,
            Screen::Manage(_) => self.on_manage_key(key).await,
        }
    }

    async fn on_overview_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('r') => {
                self.queue_task(PendingTask::RefreshScan, "Scanning LAN for Matter devices...");
            }
            KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                self.focus = match self.focus {
                    FocusPane::Commissionable => FocusPane::Commissioned,
                    FocusPane::Commissioned => FocusPane::Commissionable,
                };
            }
            KeyCode::Up => match self.focus {
                FocusPane::Commissionable => {
                    self.selected_commissionable = self.selected_commissionable.saturating_sub(1);
                }
                FocusPane::Commissioned => {
                    self.selected_commissioned = self.selected_commissioned.saturating_sub(1);
                }
            },
            KeyCode::Down => match self.focus {
                FocusPane::Commissionable => {
                    self.selected_commissionable =
                        next_index(self.selected_commissionable, self.commissionable.len());
                }
                FocusPane::Commissioned => {
                    self.selected_commissioned =
                        next_index(self.selected_commissioned, self.commissioned.len());
                }
            },
            KeyCode::Char('c') => {
                if self.focus == FocusPane::Commissionable && !self.commissionable.is_empty() {
                    self.modal = Some(Modal::Input(InputDialog {
                        title: "Commission Device".to_string(),
                        value: String::new(),
                        help: "Matter setup code: 8-digit passcode, manual code, or MT: QR payload"
                            .to_string(),
                        submit: SubmitAction::CommissionSetupCode {
                            device_index: self.selected_commissionable,
                        },
                    }));
                }
            }
            KeyCode::Char('m') | KeyCode::Enter => {
                if self.focus == FocusPane::Commissioned {
                    self.queue_task(
                        PendingTask::OpenSelectedCommissioned,
                        "Connecting to commissioned device...",
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn on_manage_key(&mut self, key: KeyEvent) -> Result<()> {
        let Screen::Manage(manage) = &mut self.screen else {
            return Ok(());
        };

        match key.code {
            KeyCode::Esc | KeyCode::Char('b') => {
                self.screen = Screen::Overview;
                self.status = "Returned to device overview.".to_string();
            }
            KeyCode::Up => {
                manage.selected_endpoint = manage.selected_endpoint.saturating_sub(1);
            }
            KeyCode::Down => {
                manage.selected_endpoint =
                    next_index(manage.selected_endpoint, manage.endpoints.len());
            }
            KeyCode::Char('o') => {
                let selected_endpoint = manage.selected_endpoint;
                if let Some(endpoint) = manage.endpoints.get(selected_endpoint) {
                    if endpoint.has_on_off {
                        let endpoint_id = endpoint.id;
                        self.queue_task(
                            PendingTask::EndpointOn {
                                endpoint_index: selected_endpoint,
                            },
                            &format!("Sending On to endpoint {}...", endpoint_id),
                        );
                    }
                }
            }
            KeyCode::Char('f') => {
                self.modal = Some(Modal::Input(InputDialog {
                    title: "Fabric Name".to_string(),
                    value: self.state.fabric_label.clone(),
                    help: "Update the fabric label stored on this device".to_string(),
                    submit: SubmitAction::ChangeFabricLabel,
                }));
            }
            KeyCode::Char('p') => {
                let selected_endpoint = manage.selected_endpoint;
                if let Some(endpoint) = manage.endpoints.get(selected_endpoint) {
                    if endpoint.has_on_off {
                        let endpoint_id = endpoint.id;
                        self.queue_task(
                            PendingTask::EndpointOff {
                                endpoint_index: selected_endpoint,
                            },
                            &format!("Sending Off to endpoint {}...", endpoint_id),
                        );
                    }
                }
            }
            KeyCode::Char('n') => {
                if let Some(endpoint) = manage.endpoints.get(manage.selected_endpoint) {
                    let default = endpoint
                        .label
                        .clone()
                        .unwrap_or_else(|| format_device_types(&endpoint.device_types));
                    self.modal = Some(Modal::Input(InputDialog {
                        title: format!("Rename Endpoint {}", endpoint.id),
                        value: default,
                        help: "Local alias stored by this client".to_string(),
                        submit: SubmitAction::RenameEndpoint {
                            endpoint_index: manage.selected_endpoint,
                        },
                    }));
                }
            }
            KeyCode::Char('a') => {
                if let Some(endpoint) = manage.endpoints.get(manage.selected_endpoint) {
                    if !endpoint.actions.is_empty() {
                        self.modal = Some(Modal::Action(build_action_dialog(
                            endpoint,
                            manage.selected_endpoint,
                        )?));
                    } else {
                        self.status = format!(
                            "Endpoint {} does not expose Actions cluster entries.",
                            endpoint.id
                        );
                    }
                }
            }
            KeyCode::Char('d') => {
                self.modal = Some(Modal::Confirm(ConfirmDialog {
                    title: "Decommission Device".to_string(),
                    message: "Remove this fabric from the selected device? [y/n]".to_string(),
                    confirm: ConfirmAction::Decommission,
                }));
            }
            _ => {}
        }
        Ok(())
    }

    async fn on_modal_key(&mut self, key: KeyEvent) -> Result<()> {
        match self.modal.take().context("modal missing")? {
            Modal::Message(message) => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                    self.status = message;
                    self.modal = None;
                } else {
                    self.modal = Some(Modal::Message(message));
                }
            }
            Modal::Confirm(dialog) => match key.code {
                KeyCode::Char('y') => self.run_confirm(dialog.confirm).await?,
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.status = "Canceled.".to_string();
                }
                _ => self.modal = Some(Modal::Confirm(dialog)),
            },
            Modal::Input(mut dialog) => match key.code {
                KeyCode::Esc => {
                    self.status = "Canceled.".to_string();
                }
                KeyCode::Enter => self.run_input_submit(dialog).await?,
                KeyCode::Backspace => {
                    dialog.value.pop();
                    self.modal = Some(Modal::Input(dialog));
                }
                KeyCode::Char(ch) => {
                    dialog.value.push(ch);
                    self.modal = Some(Modal::Input(dialog));
                }
                _ => self.modal = Some(Modal::Input(dialog)),
            },
            Modal::Action(mut dialog) => match key.code {
                KeyCode::Esc => {
                    self.status = "Canceled.".to_string();
                }
                KeyCode::Up => {
                    dialog.selected = dialog.selected.saturating_sub(1);
                    self.modal = Some(Modal::Action(dialog));
                }
                KeyCode::Down => {
                    dialog.selected = next_index(dialog.selected, dialog.options.len());
                    self.modal = Some(Modal::Action(dialog));
                }
                KeyCode::Enter => self.run_action_submit(dialog).await?,
                _ => self.modal = Some(Modal::Action(dialog)),
            },
            Modal::CommissionDeviceName { pending, mut value } => match key.code {
                KeyCode::Esc => {
                    self.status = "Commissioning canceled.".to_string();
                }
                KeyCode::Enter => {
                    let mut pending = pending;
                    if !value.trim().is_empty() {
                        pending.device_label = value.trim().to_string();
                    }
                    self.modal = Some(Modal::CommissionFabricName {
                        value: self.state.fabric_label.clone(),
                        pending,
                    });
                }
                KeyCode::Backspace => {
                    value.pop();
                    self.modal = Some(Modal::CommissionDeviceName { pending, value });
                }
                KeyCode::Char(ch) => {
                    value.push(ch);
                    self.modal = Some(Modal::CommissionDeviceName { pending, value });
                }
                _ => self.modal = Some(Modal::CommissionDeviceName { pending, value }),
            },
            Modal::CommissionFabricName { pending, mut value } => match key.code {
                KeyCode::Esc => {
                    self.status = "Commissioning canceled.".to_string();
                }
                KeyCode::Enter => {
                    let mut pending = pending;
                    if !value.trim().is_empty() {
                        pending.fabric_label = value.trim().to_string();
                    }
                    if pending.endpoints.is_empty() {
                        self.queue_task(
                            PendingTask::FinishCommission(pending),
                            "Finalizing commissioning...",
                        );
                    } else {
                        let default = pending.endpoints[0]
                            .label
                            .clone()
                            .unwrap_or_else(|| {
                                format_device_types(&pending.endpoints[0].device_types)
                            });
                        self.modal = Some(Modal::CommissionEndpointName {
                            pending,
                            index: 0,
                            value: default,
                        });
                    }
                }
                KeyCode::Backspace => {
                    value.pop();
                    self.modal = Some(Modal::CommissionFabricName { pending, value });
                }
                KeyCode::Char(ch) => {
                    value.push(ch);
                    self.modal = Some(Modal::CommissionFabricName { pending, value });
                }
                _ => self.modal = Some(Modal::CommissionFabricName { pending, value }),
            },
            Modal::CommissionEndpointName {
                mut pending,
                index,
                mut value,
            } => match key.code {
                KeyCode::Esc => {
                    self.status = "Commissioning canceled.".to_string();
                }
                KeyCode::Enter => {
                    if !value.trim().is_empty() {
                        pending.endpoints[index].label = Some(value.trim().to_string());
                    }
                    let next = index + 1;
                    if next >= pending.endpoints.len() {
                        self.queue_task(
                            PendingTask::FinishCommission(pending),
                            "Finalizing commissioning...",
                        );
                    } else {
                        let default = pending.endpoints[next]
                            .label
                            .clone()
                            .unwrap_or_else(|| {
                                format_device_types(&pending.endpoints[next].device_types)
                            });
                        self.modal = Some(Modal::CommissionEndpointName {
                            pending,
                            index: next,
                            value: default,
                        });
                    }
                }
                KeyCode::Backspace => {
                    value.pop();
                    self.modal = Some(Modal::CommissionEndpointName {
                        pending,
                        index,
                        value,
                    });
                }
                KeyCode::Char(ch) => {
                    value.push(ch);
                    self.modal = Some(Modal::CommissionEndpointName {
                        pending,
                        index,
                        value,
                    });
                }
                _ => {
                    self.modal = Some(Modal::CommissionEndpointName {
                        pending,
                        index,
                        value,
                    });
                }
            },
        }

        Ok(())
    }

    async fn run_input_submit(&mut self, dialog: InputDialog) -> Result<()> {
        match dialog.submit {
            SubmitAction::CommissionSetupCode { device_index } => {
                self.queue_task(
                    PendingTask::StartCommission {
                        device_index,
                        setup_code: dialog.value,
                    },
                    "Starting commissioning...",
                );
            }
            SubmitAction::RenameEndpoint { endpoint_index } => {
                let Screen::Manage(manage) = &mut self.screen else {
                    return Ok(());
                };
                let endpoint = manage
                    .endpoints
                    .get_mut(endpoint_index)
                    .context("endpoint not found")?;
                endpoint.label = Some(dialog.value.trim().to_string());
                upsert_endpoint_alias(
                    &mut self.state.endpoint_aliases,
                    EndpointAlias {
                        node_id: manage.device.node_id,
                        endpoint_id: endpoint.id,
                        label: dialog.value.trim().to_string(),
                    },
                );
                self.state.save(STATE_PATH)?;
                self.status = format!("Saved endpoint {} name.", endpoint.id);
            }
            SubmitAction::ChangeFabricLabel => {
                self.queue_task(
                    PendingTask::ChangeFabricLabel {
                        label: dialog.value.trim().to_string(),
                    },
                    "Updating fabric label...",
                );
            }
        }
        Ok(())
    }

    async fn run_confirm(&mut self, confirm: ConfirmAction) -> Result<()> {
        match confirm {
            ConfirmAction::Decommission => {
                self.queue_task(PendingTask::Decommission, "Decommissioning device...");
            }
        }
        Ok(())
    }

    async fn run_action_submit(&mut self, dialog: ActionDialog) -> Result<()> {
        let option = dialog
            .options
            .get(dialog.selected)
            .context("action option not found")?;
        self.queue_task(
            PendingTask::InvokeAction {
                endpoint_index: dialog.endpoint_index,
                command_id: option.command_id,
                payload: option.payload.clone(),
                label: option.label.clone(),
            },
            "Sending Actions cluster command...",
        );
        Ok(())
    }

    async fn finish_commission(&mut self, mut pending: PendingCommission) -> Result<()> {
        update_fabric_label(&mut pending.connection, &pending.fabric_label).await?;
        self.state.fabric_label = pending.fabric_label.clone();

        upsert_known_device(
            &mut self.state.devices,
            KnownDevice {
                label: pending.device_label.clone(),
                node_id: pending.node_id,
                last_address: pending.address.clone(),
            },
        );
        for endpoint in &pending.endpoints {
            let label = endpoint
                .label
                .clone()
                .unwrap_or_else(|| format_device_types(&endpoint.device_types));
            upsert_endpoint_alias(
                &mut self.state.endpoint_aliases,
                EndpointAlias {
                    node_id: pending.node_id,
                    endpoint_id: endpoint.id,
                    label,
                },
            );
        }

        self.state.save(STATE_PATH)?;
        self.status = format!(
            "Commissioned {} with node_id={}.",
            pending.device_label, pending.node_id
        );
        self.refresh_scan().await;
        Ok(())
    }

    fn queue_task(&mut self, task: PendingTask, message: &str) {
        self.pending_task = Some(task);
        self.modal = Some(Modal::Message(message.to_string()));
        self.status = message.to_string();
    }

    async fn run_pending_task(&mut self) -> Result<()> {
        let Some(task) = self.pending_task.take() else {
            return Ok(());
        };
        self.modal = None;

        match task {
            PendingTask::RefreshScan => {
                self.refresh_scan().await;
            }
            PendingTask::StartCommission {
                device_index,
                setup_code,
            } => {
                let pin = parse_setup_code(setup_code.trim())?;
                let device = self
                    .commissionable
                    .get(device_index)
                    .cloned()
                    .context("commissionable device not found")?;
                self.status = format!("Commissioning {}...", device.display_name);
                let pending = start_commission(&self.state, &device, pin).await?;
                let device_name = pending.device_label.clone();
                self.modal = Some(Modal::CommissionDeviceName {
                    pending,
                    value: device_name,
                });
            }
            PendingTask::OpenSelectedCommissioned => {
                self.open_selected_commissioned().await?;
            }
            PendingTask::EndpointOn { endpoint_index } => {
                let Screen::Manage(manage) = &mut self.screen else {
                    return Ok(());
                };
                let endpoint = manage
                    .endpoints
                    .get(endpoint_index)
                    .context("endpoint not found")?;
                manage
                    .connection
                    .invoke_request(
                        endpoint.id,
                        defs::CLUSTER_ID_ON_OFF,
                        defs::CLUSTER_ON_OFF_CMD_ID_ON,
                        &[],
                    )
                    .await?;
                self.status = format!("Sent On command to endpoint {}.", endpoint.id);
            }
            PendingTask::EndpointOff { endpoint_index } => {
                let Screen::Manage(manage) = &mut self.screen else {
                    return Ok(());
                };
                let endpoint = manage
                    .endpoints
                    .get(endpoint_index)
                    .context("endpoint not found")?;
                manage
                    .connection
                    .invoke_request(
                        endpoint.id,
                        defs::CLUSTER_ID_ON_OFF,
                        defs::CLUSTER_ON_OFF_CMD_ID_OFF,
                        &[],
                    )
                    .await?;
                self.status = format!("Sent Off command to endpoint {}.", endpoint.id);
            }
            PendingTask::ChangeFabricLabel { label } => {
                let Screen::Manage(manage) = &mut self.screen else {
                    return Ok(());
                };
                update_fabric_label(&mut manage.connection, &label).await?;
                self.state.fabric_label = label;
                self.state.save(STATE_PATH)?;
                self.status = "Fabric label updated.".to_string();
            }
            PendingTask::InvokeAction {
                endpoint_index,
                command_id,
                payload,
                label,
            } => {
                let Screen::Manage(manage) = &mut self.screen else {
                    return Ok(());
                };
                let endpoint = manage
                    .endpoints
                    .get(endpoint_index)
                    .context("endpoint not found")?;
                manage
                    .connection
                    .invoke_request(endpoint.id, defs::CLUSTER_ID_ACTIONS, command_id, &payload)
                    .await?;
                self.status = format!("Sent {label} to endpoint {}.", endpoint.id);
            }
            PendingTask::Decommission => {
                let Screen::Manage(manage) = &mut self.screen else {
                    return Ok(());
                };
                let node_id = manage.device.node_id;
                let label = manage.device.label.clone();
                decommission_device(&mut manage.connection).await?;
                self.state.devices.retain(|device| device.node_id != node_id);
                self.state
                    .endpoint_aliases
                    .retain(|alias| alias.node_id != node_id);
                self.state.save(STATE_PATH)?;
                self.screen = Screen::Overview;
                self.status = format!("Decommissioned {label}.");
                self.refresh_scan().await;
            }
            PendingTask::FinishCommission(pending) => {
                self.finish_commission(pending).await?;
            }
        }

        Ok(())
    }

    async fn open_selected_commissioned(&mut self) -> Result<()> {
        if self.commissioned.is_empty() {
            return Ok(());
        }

        let commissioned = self
            .commissioned
            .get(self.selected_commissioned)
            .context("commissioned device not found")?;
        let mut device = match &commissioned.known {
            Some(known) => known.clone(),
            None => {
                self.status = "Selected commissioned device is not managed by this client."
                    .to_string();
                return Ok(());
            }
        };

        device.last_address = first_socket_addr(&commissioned.addresses, commissioned.port);
        let mut connection = connect_known_device(&self.state, &device).await?;
        let mut endpoints = load_endpoints(&mut connection).await?;
        apply_endpoint_aliases(&self.state, device.node_id, &mut endpoints);

        self.screen = Screen::Manage(ManageState {
            device,
            connection,
            endpoints,
            selected_endpoint: 0,
        });
        self.status =
            "Manage mode: o=on, p=off, a=actions, n=rename endpoint, f=fabric, d=decommission."
                .to_string();
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let state = AppState::load(STATE_PATH)?;
    ensure_credentials(&state)?;

    let mut app = App::new(state);
    let mut terminal = setup_terminal()?;
    app.queue_task(PendingTask::RefreshScan, "Scanning LAN for Matter devices...");

    let result = run_app(&mut terminal, &mut app).await;
    restore_terminal(&mut terminal)?;
    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    while !app.quit {
        terminal.draw(|frame| draw(frame, app))?;
        if app.pending_task.is_some() {
            if let Err(err) = app.run_pending_task().await {
                app.modal = Some(Modal::Message(format!("{err:#}")));
                app.status = format!("{err:#}");
            }
            continue;
        }
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if let Err(err) = app.on_key(key).await {
                    app.modal = Some(Modal::Message(format!("{err:#}")));
                }
            }
        }
    }
    Ok(())
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn draw(frame: &mut Frame<'_>, app: &App) {
    match &app.screen {
        Screen::Overview => draw_overview(frame, app),
        Screen::Manage(manage) => draw_manage(frame, app, manage),
    }

    if let Some(modal) = &app.modal {
        draw_modal(frame, modal);
    }
}

fn draw_overview(frame: &mut Frame<'_>, app: &App) {
    let areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(8),
        Constraint::Length(3),
    ])
    .split(frame.area());

    let header = Paragraph::new(format!(
        "Matter Client\nController ID: {} | Fabric Label: {}",
        app.state.controller_id, app.state.fabric_label
    ))
    .block(Block::default().borders(Borders::ALL).title("Session"));
    frame.render_widget(header, areas[0]);

    let body = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(areas[1]);

    let commissionable_items = app
        .commissionable
        .iter()
        .map(|device| {
            let mut lines = vec![
                Line::from(device.display_name.clone()),
                Line::from(format!(
                    "{} | {}",
                    device.device_type,
                    first_socket_addr(&device.addresses, device.port)
                ))
                .dim(),
            ];
            let metadata = format!(
                "disc={} vid={} pid={}",
                device.discriminator.as_deref().unwrap_or("-"),
                device.vendor_id.as_deref().unwrap_or("-"),
                device.product_id.as_deref().unwrap_or("-")
            );
            lines.push(Line::from(metadata).dim());
            ListItem::new(lines)
        })
        .collect::<Vec<_>>();
    let mut commissionable_state =
        list_state(app.selected_commissionable, app.commissionable.is_empty());
    let commissionable = List::new(commissionable_items)
        .block(focus_block(
            "Commissionable Devices",
            app.focus == FocusPane::Commissionable,
        ))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("> ");
    frame.render_stateful_widget(commissionable, body[0], &mut commissionable_state);

    let commissioned_items = app
        .commissioned
        .iter()
        .map(|device| {
            let name = device
                .known
                .as_ref()
                .map(|known| known.label.clone())
                .unwrap_or_else(|| device.display_name.clone());
            let status = device
                .known
                .as_ref()
                .map(|known| format!("managed, node_id={}", known.node_id))
                .unwrap_or_else(|| "discovered only".to_string());
            let mut lines = vec![
                Line::from(name),
                Line::from(format!(
                    "{} | {}",
                    first_socket_addr(&device.addresses, device.port),
                    status
                ))
                .dim(),
            ];
            if device.known.is_some() && device.display_name != lines[0].to_string() {
                lines.push(Line::from(format!("service: {}", device.display_name)).dim());
            }
            ListItem::new(lines)
        })
        .collect::<Vec<_>>();
    let mut commissioned_state =
        list_state(app.selected_commissioned, app.commissioned.is_empty());
    let commissioned = List::new(commissioned_items)
        .block(focus_block(
            "Commissioned Devices",
            app.focus == FocusPane::Commissioned,
        ))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Green))
        .highlight_symbol("> ");
    frame.render_stateful_widget(commissioned, body[1], &mut commissioned_state);

    let saved = Paragraph::new(saved_devices_lines(&app.state))
        .block(Block::default().borders(Borders::ALL).title("Saved Devices"))
        .wrap(Wrap { trim: true });
    frame.render_widget(saved, areas[2]);

    let footer = Paragraph::new(format!(
        "{}\nLegend: Tab switch pane | Up/Down move | r refresh scan | c commission selected commissionable device | m or Enter manage selected commissioned device | q quit",
        app.status
    ))
    .block(Block::default().borders(Borders::ALL).title("Footer"));
    frame.render_widget(footer, areas[3]);
}

fn draw_manage(frame: &mut Frame<'_>, app: &App, manage: &ManageState) {
    let areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(3),
    ])
    .split(frame.area());

    let header = Paragraph::new(format!(
        "{} | node_id={} | addr={}",
        manage.device.label, manage.device.node_id, manage.device.last_address
    ))
    .block(Block::default().borders(Borders::ALL).title("Managed Device"));
    frame.render_widget(header, areas[0]);

    let body = Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(areas[1]);

    let endpoint_items = manage
        .endpoints
        .iter()
        .map(|endpoint| {
            let label = endpoint
                .label
                .clone()
                .unwrap_or_else(|| "unnamed".to_string());
            let capabilities = endpoint_capabilities(endpoint);
            ListItem::new(vec![
                Line::from(format!("ep{} {}", endpoint.id, label)),
                Line::from(capabilities).dim(),
            ])
        })
        .collect::<Vec<_>>();
    let mut endpoint_state = list_state(manage.selected_endpoint, manage.endpoints.is_empty());
    let endpoint_list = List::new(endpoint_items)
        .block(Block::default().borders(Borders::ALL).title("Endpoints"))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Yellow))
        .highlight_symbol("> ");
    frame.render_stateful_widget(endpoint_list, body[0], &mut endpoint_state);

    let details = selected_endpoint_details(manage);
    let detail_widget = Paragraph::new(details)
        .block(Block::default().borders(Borders::ALL).title("Details"))
        .wrap(Wrap { trim: true });
    frame.render_widget(detail_widget, body[1]);

    let footer = Paragraph::new(format!(
        "{}\nLegend: Up/Down move | o on | p off | a actions | n rename endpoint | f fabric label | d decommission | b or Esc back",
        app.status
    ))
    .block(Block::default().borders(Borders::ALL).title("Footer"));
    frame.render_widget(footer, areas[2]);
}

fn draw_modal(frame: &mut Frame<'_>, modal: &Modal) {
    let area = centered_rect(70, 35, frame.area());
    frame.render_widget(Clear, area);

    match modal {
        Modal::Message(message) => {
            let paragraph = Paragraph::new(format!("{message}\n\nEnter or Esc closes this dialog"))
                .block(Block::default().borders(Borders::ALL).title("Message"))
                .wrap(Wrap { trim: true });
            frame.render_widget(paragraph, area);
        }
        Modal::Confirm(dialog) => {
            let paragraph = Paragraph::new(format!("{}\n\n{}", dialog.message, dialog.title))
                .block(Block::default().borders(Borders::ALL).title("Confirm"))
                .wrap(Wrap { trim: true });
            frame.render_widget(paragraph, area);
        }
        Modal::Input(dialog) => {
            draw_input_box(frame, area, &dialog.title, &dialog.value, &dialog.help);
        }
        Modal::Action(dialog) => {
            let items = dialog
                .options
                .iter()
                .map(|option| ListItem::new(option.label.clone()))
                .collect::<Vec<_>>();
            let mut state = list_state(dialog.selected, dialog.options.is_empty());
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(dialog.title.clone()))
                .highlight_style(Style::default().fg(Color::Black).bg(Color::Magenta))
                .highlight_symbol("> ");
            frame.render_stateful_widget(list, area, &mut state);
        }
        Modal::CommissionDeviceName { pending, value } => {
            draw_input_box(
                frame,
                area,
                "Commissioning: Device Name",
                value,
                &format!(
                    "Name the device. Suggested default: {}",
                    pending.device_label
                ),
            );
        }
        Modal::CommissionFabricName { pending, value } => {
            draw_input_box(
                frame,
                area,
                "Commissioning: Fabric Name",
                value,
                &format!(
                    "Set the fabric label for node {}. Default: {}",
                    pending.node_id, pending.fabric_label
                ),
            );
        }
        Modal::CommissionEndpointName {
            pending,
            index,
            value,
        } => {
            let endpoint = &pending.endpoints[*index];
            draw_input_box(
                frame,
                area,
                &format!("Commissioning: Endpoint {} Name", endpoint.id),
                value,
                "Press Enter to accept this endpoint label.",
            );
        }
    }
}

fn draw_input_box(frame: &mut Frame<'_>, area: Rect, title: &str, value: &str, help: &str) {
    let paragraph = Paragraph::new(format!("{help}\n\n{value}_"))
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn focus_block<'a>(title: &'a str, focused: bool) -> Block<'a> {
    let style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(style)
}

fn saved_devices_lines(state: &AppState) -> Text<'static> {
    let mut lines = Vec::new();
    if state.devices.is_empty() {
        lines.push(Line::from("No saved devices."));
    } else {
        for device in &state.devices {
            lines.push(Line::from(format!(
                "{} | node_id={} | {}",
                device.label, device.node_id, device.last_address
            )));
            let aliases = state
                .endpoint_aliases
                .iter()
                .filter(|alias| alias.node_id == device.node_id)
                .map(|alias| format!("ep{}={}", alias.endpoint_id, alias.label))
                .collect::<Vec<_>>();
            if !aliases.is_empty() {
                lines.push(Line::from(format!("  {}", aliases.join(", "))).dim());
            }
        }
    }
    Text::from(lines)
}

fn selected_endpoint_details(manage: &ManageState) -> Text<'static> {
    let Some(endpoint) = manage.endpoints.get(manage.selected_endpoint) else {
        return Text::from("No endpoints.");
    };

    let mut lines = vec![
        Line::from(format!("Endpoint: {}", endpoint.id)),
        Line::from(format!(
            "Label: {}",
            endpoint.label.clone().unwrap_or_else(|| "unnamed".to_string())
        )),
        Line::from(format!(
            "Device types: {}",
            format_device_types(&endpoint.device_types)
        )),
        Line::from(format!(
            "Capabilities: {}",
            endpoint_capabilities(endpoint)
        )),
    ];

    if !endpoint.actions.is_empty() {
        lines.push(Line::from("Actions:"));
        for action in &endpoint.actions {
            lines.push(Line::from(format!(
                "  {} (id={})",
                action.name.clone().unwrap_or_else(|| "unnamed".to_string()),
                action.action_id.unwrap_or_default()
            )));
        }
    }

    Text::from(lines)
}

fn endpoint_capabilities(endpoint: &EndpointSummary) -> String {
    let mut caps = Vec::new();
    if endpoint.has_on_off {
        caps.push("on/off");
    }
    if !endpoint.actions.is_empty() {
        caps.push("actions");
    }
    if caps.is_empty() {
        "none".to_string()
    } else {
        caps.join(", ")
    }
}

fn list_state(selected: usize, empty: bool) -> ListState {
    let mut state = ListState::default();
    if !empty {
        state.select(Some(selected));
    }
    state
}

fn centered_rect(percent_x: u16, percent_y: u16, rect: Rect) -> Rect {
    let popup = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(rect);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup[1])[1]
}

fn clamp_selection(current: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        current.min(len - 1)
    }
}

fn next_index(current: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (current + 1).min(len - 1)
    }
}

fn build_action_dialog(endpoint: &EndpointSummary, endpoint_index: usize) -> Result<ActionDialog> {
    let mut options = Vec::new();
    for action in &endpoint.actions {
        let action_id = action.action_id.context("action missing action_id")?;
        let invoke_id = next_invoke_id();
        let action_name = action.name.clone().unwrap_or_else(|| "unnamed".to_string());
        options.push(ActionOption {
            label: format!("Instant: {action_name}"),
            command_id: defs::CLUSTER_ACTIONS_CMD_ID_INSTANTACTION,
            payload: codec::actions_cluster::encode_instant_action(action_id, invoke_id)?,
        });
        options.push(ActionOption {
            label: format!("Start: {action_name}"),
            command_id: defs::CLUSTER_ACTIONS_CMD_ID_STARTACTION,
            payload: codec::actions_cluster::encode_start_action(action_id, invoke_id)?,
        });
        options.push(ActionOption {
            label: format!("Stop: {action_name}"),
            command_id: defs::CLUSTER_ACTIONS_CMD_ID_STOPACTION,
            payload: codec::actions_cluster::encode_stop_action(action_id, invoke_id)?,
        });
        options.push(ActionOption {
            label: format!("Pause: {action_name}"),
            command_id: defs::CLUSTER_ACTIONS_CMD_ID_PAUSEACTION,
            payload: codec::actions_cluster::encode_pause_action(action_id, invoke_id)?,
        });
        options.push(ActionOption {
            label: format!("Resume: {action_name}"),
            command_id: defs::CLUSTER_ACTIONS_CMD_ID_RESUMEACTION,
            payload: codec::actions_cluster::encode_resume_action(action_id, invoke_id)?,
        });
        options.push(ActionOption {
            label: format!("Enable: {action_name}"),
            command_id: defs::CLUSTER_ACTIONS_CMD_ID_ENABLEACTION,
            payload: codec::actions_cluster::encode_enable_action(action_id, invoke_id)?,
        });
        options.push(ActionOption {
            label: format!("Disable: {action_name}"),
            command_id: defs::CLUSTER_ACTIONS_CMD_ID_DISABLEACTION,
            payload: codec::actions_cluster::encode_disable_action(action_id, invoke_id)?,
        });
    }

    Ok(ActionDialog {
        title: format!("Endpoint {} Actions", endpoint.id),
        endpoint_index,
        options,
        selected: 0,
    })
}

async fn start_commission(
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

    Ok(EndpointSummary {
        id: endpoint_id,
        label,
        device_types,
        has_on_off: clusters.contains(&defs::CLUSTER_ID_ON_OFF),
        actions,
    })
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
