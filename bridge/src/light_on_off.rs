use log::info;
use rand::RngCore;

use rs_matter::dm::clusters::decl::on_off as on_off_cluster;
use rs_matter::dm::clusters::on_off::{
    self, EffectVariantEnum, OnOffHooks, StartUpOnOffEnum,
};
use rs_matter::dm::{Cluster, Dataver};
use rs_matter::error::Error;
use rs_matter::tlv::Nullable;
use rs_matter::utils::cell::RefCell;
use rs_matter::utils::sync::Signal;
use rs_matter::utils::sync::blocking::Mutex;
use rs_matter::with;

use crate::BridgedHandler;

pub(crate) type LightOnOffHandler =
    on_off::OnOffHandler<'static, LightOnOffLogic, on_off::NoLevelControl>;

/// Creates an On/Off handler for the given bridged light endpoint.
pub(crate) fn create_light_handler(
    rand: &mut impl RngCore,
    endpoint_id: u16,
) -> LightOnOffHandler {
    on_off::OnOffHandler::new_standalone(
        Dataver::new_rand(rand),
        endpoint_id,
        LightOnOffLogic::new(endpoint_id),
    )
}

struct LightState {
    on_off: bool,
    start_up_on_off: Option<StartUpOnOffEnum>,
}

impl LightState {
    /// Creates the default persisted light state.
    const fn new() -> Self {
        Self {
            on_off: false,
            start_up_on_off: None,
        }
    }
}

pub(crate) struct LightOnOffLogic {
    endpoint_id: u16,
    state: Mutex<RefCell<LightState>>,
    auto_off_signal: Signal<Option<()>>,
}

impl LightOnOffLogic {
    /// Creates the device-specific On/Off logic for one endpoint.
    const fn new(endpoint_id: u16) -> Self {
        Self {
            endpoint_id,
            state: Mutex::new(RefCell::new(LightState::new())),
            auto_off_signal: Signal::new(None),
        }
    }

    /// Resolves the configured human-readable name for this light.
    fn light_name(&self) -> &'static str {
        BridgedHandler::default_light_name(self.endpoint_id)
    }
}

impl OnOffHooks for LightOnOffLogic {
    /// Cluster metadata for the bridged On/Off light implementation.
    const CLUSTER: Cluster<'static> = on_off_cluster::FULL_CLUSTER
        .with_revision(6)
        .with_attrs(with!(
            required;
            on_off_cluster::AttributeId::OnOff
        ))
        .with_cmds(with!(
            on_off_cluster::CommandId::Off
                | on_off_cluster::CommandId::On
                | on_off_cluster::CommandId::Toggle
        ));

    /// Returns the current On/Off state.
    fn on_off(&self) -> bool {
        self.state.lock(|state| state.borrow().on_off)
    }

    /// Updates the current On/Off state and emits log and auto-off signals.
    fn set_on_off(&self, on: bool) {
        self.state.lock(|state| state.borrow_mut().on_off = on);

        info!(
            "bulb_id={} bulb_name=\"{}\" on_off={}",
            self.endpoint_id,
            self.light_name(),
            on
        );

        if on {
            self.auto_off_signal.signal(());
        }
    }

    /// Returns the persisted startup behavior for this light.
    fn start_up_on_off(&self) -> Nullable<StartUpOnOffEnum> {
        match self.state.lock(|state| state.borrow().start_up_on_off) {
            Some(value) => Nullable::some(value),
            None => Nullable::none(),
        }
    }

    /// Stores the startup behavior for this light.
    fn set_start_up_on_off(&self, value: Nullable<StartUpOnOffEnum>) -> Result<(), Error> {
        self.state
            .lock(|state| state.borrow_mut().start_up_on_off = value.into_option());
        Ok(())
    }

    /// Ignores off-with-effect commands for this bridge-specific light logic.
    async fn handle_off_with_effect(&self, _effect: EffectVariantEnum) {}

    // INFO: enable this if you want to auto-off light after 1 second after its "on"
    // Waits for lights to turn on and emits an out-of-band off request after one second.
    // async fn run<F: Fn(OutOfBandMessage)>(&self, notify: F) {
    //     loop {
    //         self.auto_off_signal.wait_signalled().await;
    //         Timer::after(Duration::from_secs(1)).await;
    //
    //         if self.on_off() {
    //             info!(
    //                 "bulb_id={} bulb_name=\"{}\" auto_off_after_s=1",
    //                 self.endpoint_id,
    //                 self.light_name()
    //             );
    //             notify(OutOfBandMessage::Off);
    //         }
    //     }
    // }
}
