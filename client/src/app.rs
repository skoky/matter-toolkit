use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use matc::clusters::defs;

use crate::matter;
use crate::network;
use crate::setup_code;
use crate::state::{AppState, EndpointAlias, KnownDevice};
use crate::types::{
    ActionDialog, CommissionableDevice, CommissionedDevice, ConfirmAction, ConfirmDialog,
    FocusPane, InputDialog, ManageState, Modal, PendingCommission, PendingTask, Screen,
    StatusKind, SubmitAction,
};
use crate::utils::{
    apply_endpoint_aliases, clamp_selection, format_device_types, first_socket_addr,
    next_index, upsert_endpoint_alias, upsert_known_device,
};

pub const STATE_PATH: &str = "./client-state.txt";

pub struct App {
    pub state: AppState,
    pub screen: Screen,
    pub focus: FocusPane,
    pub commissionable: Vec<CommissionableDevice>,
    pub commissioned: Vec<CommissionedDevice>,
    pub selected_commissionable: usize,
    pub selected_commissioned: usize,
    pub modal: Option<Modal>,
    pub pending_task: Option<PendingTask>,
    pub status: String,
    pub status_kind: StatusKind,
    pub quit: bool,
}

impl App {
    pub fn new(state: AppState) -> Self {
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
            status: "Press r to scan for devices.".to_string(),
            status_kind: StatusKind::Normal,
            quit: false,
        }
    }

    pub fn queue_task(&mut self, task: PendingTask, message: &str) {
        self.pending_task = Some(task);
        self.modal = Some(Modal::Message(message.to_string()));
        self.status = message.to_string();
        self.status_kind = StatusKind::Progress;
    }

    pub async fn refresh_scan(&mut self) {
        self.status = "Scanning LAN for Matter devices...".to_string();
        self.status_kind = StatusKind::Progress;
        match network::scan_network(&self.state) {
            Ok((commissionable, commissioned)) => {
                self.commissionable = commissionable;
                self.commissioned = commissioned;
                self.selected_commissionable =
                    clamp_selection(self.selected_commissionable, self.commissionable.len());
                self.selected_commissioned =
                    clamp_selection(self.selected_commissioned, self.commissioned.len());
                self.status = format!(
                    "Found {} commissionable and {} commissioned device(s).",
                    self.commissionable.len(),
                    self.commissioned.len()
                );
                self.status_kind = StatusKind::Success;
            }
            Err(err) => {
                self.status = format!("Scan failed: {err}");
                self.status_kind = StatusKind::Error;
            }
        }
    }

    pub async fn on_key(&mut self, key: KeyEvent) -> Result<()> {
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
                    self.selected_commissionable =
                        self.selected_commissionable.saturating_sub(1);
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
                        help: "Enter setup code: 8-digit passcode, manual pairing code, or MT: QR payload".to_string(),
                        submit: SubmitAction::CommissionSetupCode {
                            device_index: self.selected_commissionable,
                        },
                    }));
                }
            }
            KeyCode::Char('m') | KeyCode::Enter => {
                if self.focus == FocusPane::Commissioned && !self.commissioned.is_empty() {
                    self.queue_task(
                        PendingTask::OpenSelectedCommissioned,
                        "Connecting to device...",
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
                self.status = "Returned to overview.".to_string();
                self.status_kind = StatusKind::Normal;
            }
            KeyCode::Up => {
                manage.selected_endpoint = manage.selected_endpoint.saturating_sub(1);
            }
            KeyCode::Down => {
                manage.selected_endpoint =
                    next_index(manage.selected_endpoint, manage.endpoints.len());
            }
            KeyCode::Char('o') => {
                let idx = manage.selected_endpoint;
                if let Some(ep) = manage.endpoints.get(idx) {
                    if ep.has_on_off {
                        let id = ep.id;
                        self.queue_task(
                            PendingTask::EndpointOn { endpoint_index: idx },
                            &format!("Sending On to endpoint {id}..."),
                        );
                    }
                }
            }
            KeyCode::Char('p') => {
                let idx = manage.selected_endpoint;
                if let Some(ep) = manage.endpoints.get(idx) {
                    if ep.has_on_off {
                        let id = ep.id;
                        self.queue_task(
                            PendingTask::EndpointOff { endpoint_index: idx },
                            &format!("Sending Off to endpoint {id}..."),
                        );
                    }
                }
            }
            KeyCode::Char('n') => {
                if let Some(ep) = manage.endpoints.get(manage.selected_endpoint) {
                    let default = ep
                        .label
                        .clone()
                        .unwrap_or_else(|| format_device_types(&ep.device_types));
                    self.modal = Some(Modal::Input(InputDialog {
                        title: format!("Rename Endpoint {}", ep.id),
                        value: default,
                        help: "Local alias stored by this client only".to_string(),
                        submit: SubmitAction::RenameEndpoint {
                            endpoint_index: manage.selected_endpoint,
                        },
                    }));
                }
            }
            KeyCode::Char('f') => {
                self.modal = Some(Modal::Input(InputDialog {
                    title: "Update Fabric Label".to_string(),
                    value: self.state.fabric_label.clone(),
                    help: "This label is written to the device's fabric entry".to_string(),
                    submit: SubmitAction::ChangeFabricLabel,
                }));
            }
            KeyCode::Char('a') => {
                if let Some(ep) = manage.endpoints.get(manage.selected_endpoint) {
                    if !ep.actions.is_empty() {
                        self.modal = Some(Modal::Action(matter::build_action_dialog(
                            ep,
                            manage.selected_endpoint,
                        )?));
                    } else {
                        self.status =
                            format!("Endpoint {} has no Actions cluster entries.", ep.id);
                        self.status_kind = StatusKind::Normal;
                    }
                }
            }
            KeyCode::Char('d') => {
                self.modal = Some(Modal::Confirm(ConfirmDialog {
                    title: "Decommission Device".to_string(),
                    message: "Remove this client's fabric from the device? Press y to confirm."
                        .to_string(),
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
                    self.status_kind = StatusKind::Normal;
                }
                _ => self.modal = Some(Modal::Confirm(dialog)),
            },
            Modal::Input(mut dialog) => match key.code {
                KeyCode::Esc => {
                    self.status = "Canceled.".to_string();
                    self.status_kind = StatusKind::Normal;
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
                    self.status_kind = StatusKind::Normal;
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
                    self.status_kind = StatusKind::Normal;
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
                    self.status_kind = StatusKind::Normal;
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
                        let default = endpoint_default_label(&pending.endpoints[0]);
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
                    self.status_kind = StatusKind::Normal;
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
                        let default = endpoint_default_label(&pending.endpoints[next]);
                        self.modal = Some(Modal::CommissionEndpointName {
                            pending,
                            index: next,
                            value: default,
                        });
                    }
                }
                KeyCode::Backspace => {
                    value.pop();
                    self.modal = Some(Modal::CommissionEndpointName { pending, index, value });
                }
                KeyCode::Char(ch) => {
                    value.push(ch);
                    self.modal = Some(Modal::CommissionEndpointName { pending, index, value });
                }
                _ => self.modal = Some(Modal::CommissionEndpointName { pending, index, value }),
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
                let ep = manage
                    .endpoints
                    .get_mut(endpoint_index)
                    .context("endpoint not found")?;
                ep.label = Some(dialog.value.trim().to_string());
                upsert_endpoint_alias(
                    &mut self.state.endpoint_aliases,
                    EndpointAlias {
                        node_id: manage.device.node_id,
                        endpoint_id: ep.id,
                        label: dialog.value.trim().to_string(),
                    },
                );
                self.state.save(STATE_PATH)?;
                self.status = format!("Saved endpoint {} label.", ep.id);
                self.status_kind = StatusKind::Success;
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

    pub async fn run_pending_task(&mut self) -> Result<()> {
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
                let pin = setup_code::parse_setup_code(setup_code.trim())?;
                let device = self
                    .commissionable
                    .get(device_index)
                    .cloned()
                    .context("commissionable device not found")?;
                self.status = format!("Commissioning {}...", device.display_name);
                self.status_kind = StatusKind::Progress;
                let pending = matter::start_commission(&self.state, &device, pin).await?;
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
                let ep = manage
                    .endpoints
                    .get(endpoint_index)
                    .context("endpoint not found")?;
                manage
                    .connection
                    .invoke_request(
                        ep.id,
                        defs::CLUSTER_ID_ON_OFF,
                        defs::CLUSTER_ON_OFF_CMD_ID_ON,
                        &[],
                    )
                    .await?;
                self.status = format!("Sent On to endpoint {}.", ep.id);
                self.status_kind = StatusKind::Success;
            }
            PendingTask::EndpointOff { endpoint_index } => {
                let Screen::Manage(manage) = &mut self.screen else {
                    return Ok(());
                };
                let ep = manage
                    .endpoints
                    .get(endpoint_index)
                    .context("endpoint not found")?;
                manage
                    .connection
                    .invoke_request(
                        ep.id,
                        defs::CLUSTER_ID_ON_OFF,
                        defs::CLUSTER_ON_OFF_CMD_ID_OFF,
                        &[],
                    )
                    .await?;
                self.status = format!("Sent Off to endpoint {}.", ep.id);
                self.status_kind = StatusKind::Success;
            }
            PendingTask::ChangeFabricLabel { label } => {
                let Screen::Manage(manage) = &mut self.screen else {
                    return Ok(());
                };
                matter::update_fabric_label(&mut manage.connection, &label).await?;
                self.state.fabric_label = label;
                self.state.save(STATE_PATH)?;
                self.status = "Fabric label updated.".to_string();
                self.status_kind = StatusKind::Success;
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
                let ep = manage
                    .endpoints
                    .get(endpoint_index)
                    .context("endpoint not found")?;
                manage
                    .connection
                    .invoke_request(ep.id, defs::CLUSTER_ID_ACTIONS, command_id, &payload)
                    .await?;
                self.status = format!("Sent \"{label}\" to endpoint {}.", ep.id);
                self.status_kind = StatusKind::Success;
            }
            PendingTask::Decommission => {
                let Screen::Manage(manage) = &mut self.screen else {
                    return Ok(());
                };
                let node_id = manage.device.node_id;
                let label = manage.device.label.clone();
                matter::decommission_device(&mut manage.connection).await?;
                self.state.devices.retain(|d| d.node_id != node_id);
                self.state.endpoint_aliases.retain(|a| a.node_id != node_id);
                self.state.save(STATE_PATH)?;
                self.screen = Screen::Overview;
                self.status = format!("Decommissioned {label}.");
                self.status_kind = StatusKind::Success;
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
                self.status =
                    "Selected device is not managed by this client.".to_string();
                self.status_kind = StatusKind::Normal;
                return Ok(());
            }
        };

        device.last_address = first_socket_addr(&commissioned.addresses, commissioned.port);
        let mut connection = matter::connect_known_device(&self.state, &device).await?;
        let mut endpoints = matter::load_endpoints(&mut connection).await?;
        apply_endpoint_aliases(&self.state, device.node_id, &mut endpoints);

        self.screen = Screen::Manage(ManageState {
            device,
            connection,
            endpoints,
            selected_endpoint: 0,
        });
        self.status = "Connected.  o on  p off  a actions  n rename  f fabric  d decommission  b back".to_string();
        self.status_kind = StatusKind::Success;
        Ok(())
    }

    async fn finish_commission(&mut self, mut pending: PendingCommission) -> Result<()> {
        matter::update_fabric_label(&mut pending.connection, &pending.fabric_label).await?;
        self.state.fabric_label = pending.fabric_label.clone();

        upsert_known_device(
            &mut self.state.devices,
            KnownDevice {
                label: pending.device_label.clone(),
                node_id: pending.node_id,
                last_address: pending.address.clone(),
            },
        );
        for ep in &pending.endpoints {
            let label = ep
                .label
                .clone()
                .unwrap_or_else(|| format_device_types(&ep.device_types));
            upsert_endpoint_alias(
                &mut self.state.endpoint_aliases,
                EndpointAlias {
                    node_id: pending.node_id,
                    endpoint_id: ep.id,
                    label,
                },
            );
        }

        self.state.save(STATE_PATH)?;
        self.status = format!(
            "Commissioned \"{}\" (node_id={}).",
            pending.device_label, pending.node_id
        );
        self.status_kind = StatusKind::Success;
        self.refresh_scan().await;
        Ok(())
    }
}

fn endpoint_default_label(ep: &crate::types::EndpointSummary) -> String {
    ep.label
        .clone()
        .unwrap_or_else(|| format_device_types(&ep.device_types))
}
