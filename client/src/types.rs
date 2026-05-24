use std::net::IpAddr;
use std::time::Instant;
use matc::{clusters::codec, controller};
use crate::state::KnownDevice;

#[derive(Clone, Debug)]
pub struct CommissionableDevice {
    pub display_name: String,
    pub device_type: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
    pub discriminator: Option<String>,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CommissionedDevice {
    pub display_name: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
    pub known: Option<KnownDevice>,
}

#[derive(Debug)]
pub struct EndpointSummary {
    pub id: u16,
    pub label: Option<String>,
    pub device_types: Vec<String>,
    pub has_on_off: bool,
    pub on_off_state: Option<bool>,
    pub actions: Vec<codec::actions_cluster::Action>,
}

pub struct ManageState {
    pub device: KnownDevice,
    pub connection: controller::Connection,
    pub endpoints: Vec<EndpointSummary>,
    pub selected_endpoint: usize,
    pub last_endpoint_refresh: Instant,
}

pub struct PendingCommission {
    pub connection: controller::Connection,
    pub node_id: u64,
    pub address: String,
    pub device_label: String,
    pub fabric_label: String,
    pub endpoints: Vec<EndpointSummary>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusPane {
    Commissionable,
    Commissioned,
}

pub enum Screen {
    Overview,
    Manage(ManageState),
}

pub enum Modal {
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

pub struct InputDialog {
    pub title: String,
    pub value: String,
    pub help: String,
    pub submit: SubmitAction,
}

pub struct ConfirmDialog {
    pub title: String,
    pub message: String,
    pub confirm: ConfirmAction,
}

pub struct ActionDialog {
    pub title: String,
    pub endpoint_index: usize,
    pub options: Vec<ActionOption>,
    pub selected: usize,
}

pub struct ActionOption {
    pub label: String,
    pub command_id: u32,
    pub payload: Vec<u8>,
}

pub enum SubmitAction {
    CommissionSetupCode { device_index: usize },
    RenameEndpoint { endpoint_index: usize },
    ChangeFabricLabel,
}

pub enum ConfirmAction {
    Decommission,
}

pub enum PendingTask {
    RefreshScan,
    StartCommission {
        device_index: usize,
        setup_code: String,
    },
    OpenSelectedCommissioned,
    EndpointOn {
        endpoint_index: usize,
    },
    EndpointOff {
        endpoint_index: usize,
    },
    ChangeFabricLabel {
        label: String,
    },
    InvokeAction {
        endpoint_index: usize,
        command_id: u32,
        payload: Vec<u8>,
        label: String,
    },
    Decommission,
    FinishCommission(PendingCommission),
    RefreshEndpoints,
}

/// Drives the color of the status bar in the UI.
#[derive(Clone, Copy, Debug)]
pub enum StatusKind {
    Normal,
    Success,
    Progress,
    Error,
}
