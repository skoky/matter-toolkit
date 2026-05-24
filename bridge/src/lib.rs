#![recursion_limit = "256"]

use core::pin::pin;

use std::net::UdpSocket;

use rand::RngCore;

use rs_matter::crypto::{Crypto, default_crypto};
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter::dm::clusters::groups::{self, ClusterHandler as _};
use rs_matter::dm::clusters::net_comm::SharedNetworks;
use rs_matter::dm::clusters::on_off::{self, OnOffHooks};
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter::dm::devices::{DEV_TYPE_AGGREGATOR, DEV_TYPE_BRIDGED_NODE, DEV_TYPE_ON_OFF_LIGHT};
use rs_matter::dm::endpoints;
use rs_matter::dm::events::Events;
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::networks::eth::EthNetwork;
use rs_matter::dm::subscriptions::Subscriptions;
use rs_matter::dm::{
    Access, Async, AsyncHandler, AsyncMetadata, Attribute, Cluster, DataModel, Dataver,
    EmptyHandler, Endpoint, EpClMatcher, Handler, InvokeContext, Node, NonBlockingHandler, Quality,
    ReadContext, ReadReply, Reply, WriteContext,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::persist::{DirKvBlobStore, SharedKvBlobStore};
use rs_matter::respond::DefaultResponder;
use rs_matter::sc::pase::MAX_COMM_WINDOW_TIMEOUT_SECS;
use rs_matter::tlv::{TLVBuilderParent, Utf8StrBuilder};
use rs_matter::transport::MATTER_SOCKET_BIND_ADDR;
use rs_matter::utils::storage::pooled::PooledBuffers;
use rs_matter::{MATTER_PORT, Matter, clusters, devices, with};

pub use rs_matter::dm::clusters::decl::bridged_device_basic_information::{
    self, ClusterHandler as _, KeepActiveRequest,
};
use rs_matter::pairing::qr::QrTextType;

mod light_on_off;
mod mdns;

use light_on_off::{LightOnOffHandler, LightOnOffLogic, create_light_handler};

/// Fallback name used for bridged endpoints without a specific label.
const DEFAULT_LIGHT_NAME: &str = "Example Light Bridge";
/// Matter cluster identifier for OTA Software Update Requestor.
const OTA_REQUESTOR_CLUSTER_ID: u32 = 0x002A;
/// Attribute identifier for the default OTA provider list.
const OTA_REQUESTOR_DEFAULT_PROVIDERS_ATTR_ID: u32 = 0;

/// Starts the bridge inside the current Tokio runtime.
pub async fn run_bridge() -> Result<(), Error> {
    let mut matter = Matter::new_default(&TEST_DEV_DET, TEST_DEV_COMM, &TEST_DEV_ATT, MATTER_PORT);

    let mut events: Events = Events::new_default();

    let mut kv_buf = [0; 4096];
    let mut kv = DirKvBlobStore::new(std::env::current_dir()?.join(".matter_kvs"));
    matter.load_persist(&mut kv, &mut kv_buf).await?;
    events.load_persist(&mut kv, &mut kv_buf).await?;

    let buffers = PooledBuffers::<10, _>::new(0);
    let subscriptions: Subscriptions = Subscriptions::new();
    let crypto = default_crypto(rand::thread_rng(), DAC_PRIVKEY);
    let mut rand = crypto.rand()?;

    let second_light_enabled = second_light_enabled();

    let on_off_handler_ep2 = create_light_handler(&mut rand, 2);
    let on_off_handler_ep3 = create_light_handler(&mut rand, 3);

    if second_light_enabled {
        log::info!("Second light enabled");
        let dm = DataModel::new(
            &matter,
            &crypto,
            &buffers,
            &subscriptions,
            &events,
            dm_second_light_enabled(rand, [&on_off_handler_ep2, &on_off_handler_ep3]),
            SharedKvBlobStore::new(kv, kv_buf.as_mut_slice()),
            SharedNetworks::new(EthNetwork::new_default()),
        );

        let responder = DefaultResponder::new(&dm);
        let mut respond = pin!(responder.run::<4, 4>());
        let mut dm_job = pin!(dm.run());

        let socket = async_io::Async::<UdpSocket>::bind(MATTER_SOCKET_BIND_ADDR)?;
        let mut mdns = pin!(mdns::run_mdns(&matter, &crypto));
        let mut transport = pin!(matter.run(&crypto, &socket, &socket, &socket));

        if !matter.is_commissioned() {
            matter.print_standard_qr_text(DiscoveryCapabilities::IP)?;
            matter.print_standard_qr_code(QrTextType::Unicode, DiscoveryCapabilities::IP)?;
            save_pairing_files(&matter, DiscoveryCapabilities::IP);
            matter.open_basic_comm_window(
                MAX_COMM_WINDOW_TIMEOUT_SECS,
                &crypto,
                dm.change_notify(),
            )?;
        }

        tokio::select! {
            result = &mut transport => result,
            result = &mut mdns => result,
            result = &mut respond => result,
            result = &mut dm_job => result,
        }
    } else {
        let dm = DataModel::new(
            &matter,
            &crypto,
            &buffers,
            &subscriptions,
            &events,
            dm_handler_single_light(rand, [&on_off_handler_ep2]),
            SharedKvBlobStore::new(kv, kv_buf.as_mut_slice()),
            SharedNetworks::new(EthNetwork::new_default()),
        );

        let responder = DefaultResponder::new(&dm);
        let mut respond = pin!(responder.run::<4, 4>());
        let mut dm_job = pin!(dm.run());

        let socket = async_io::Async::<UdpSocket>::bind(MATTER_SOCKET_BIND_ADDR)?;
        let mut mdns = pin!(mdns::run_mdns(&matter, &crypto));
        let mut transport = pin!(matter.run(&crypto, &socket, &socket, &socket));

        if !matter.is_commissioned() {
            matter.print_standard_qr_text(DiscoveryCapabilities::IP)?;
            matter.print_standard_qr_code(QrTextType::Unicode, DiscoveryCapabilities::IP)?;
            save_pairing_files(&matter, DiscoveryCapabilities::IP);
            matter.open_basic_comm_window(
                MAX_COMM_WINDOW_TIMEOUT_SECS,
                &crypto,
                dm.change_notify(),
            )?;
        }

        tokio::select! {
            result = &mut transport => result,
            result = &mut mdns => result,
            result = &mut respond => result,
            result = &mut dm_job => result,
        }
    }
}

fn save_pairing_files(matter: &Matter, _disc_caps: DiscoveryCapabilities) {
    let pairing_code = matter.dev_comm().compute_pretty_pairing_code();
    // https://project-chip.github.io/connectedhomeip/qrcode.html?data=MT%3A-24J042C000GM363000

    // let payload = match standard_qr_payload(matter, disc_caps) {
    //     Ok(p) => p,
    //     Err(e) => {
    //         log::error!("Failed to get QR payload: {:?}", e);
    //         return;
    //     }
    // };
    //
    // let mut buf = vec![0u8; 2048];
    // let (qr_text, _) = match payload.as_str(&mut buf) {
    //     Ok(r) => r,
    //     Err(e) => {
    //         log::error!("Failed to encode QR text: {:?}", e);
    //         return;
    //     }
    // };
    // let qr_text_owned = qr_text.to_string();

    // let content = format!("Pairing Code: {}\nQR Code URL: https://project-chip.github.io/connectedhomeip/qrcode.html?data={}\n", pairing_code, qr_text_owned);
    let content = format!("Pairing Code: {}", pairing_code);
    if let Err(e) = std::fs::write("pairing_code.txt", &content) {
        log::error!("Failed to save pairing_code.txt: {}", e);
    } else {
        log::info!("Pairing code saved to pairing_code.txt");
    }

    // let mut qr_buf = vec![0u8; 8192];
    // let mid = qr_buf.len() / 2;
    // let (tmp_buf, out_buf) = qr_buf.split_at_mut(mid);
    // let qr = match Qr::compute(&qr_text_owned, tmp_buf, out_buf) {
    //     Ok(q) => q,
    //     Err(e) => {
    //         log::error!("Failed to compute QR code: {:?}", e);
    //         return;
    //     }
    // };
    //
    // let size = qr.size();
    // let scale = 10u32;
    // let border = 4u32;
    // let img_size = (size + 2 * border) * scale;
    // let mut img = image::GrayImage::new(img_size, img_size);
    //
    // for py in 0..img_size {
    //     for px in 0..img_size {
    //         let mx = (px / scale) as i32 - border as i32;
    //         let my = (py / scale) as i32 - border as i32;
    //         let is_dark = mx >= 0
    //             && my >= 0
    //             && mx < size as i32
    //             && my < size as i32
    //             && qr.get_module(mx, my);
    //         img.put_pixel(px, py, image::Luma([if is_dark { 0u8 } else { 255u8 }]));
    //     }
    // }

    // if let Err(e) = img.save("pairing_qr.png") {
    //     log::error!("Failed to save pairing_qr.png: {}", e);
    // } else {
    //     log::info!("QR code image saved to pairing_qr.png");
    // }
}

fn second_light_enabled() -> bool {
    matches!(
        std::env::var("SECOND_LIGHT_ENABLED").as_deref(),
        Ok("true") | Ok("TRUE") | Ok("True")
    )
}

/// Static node metadata for the full bridge topology.
const NODE_DETAILS_TWO_LIGHTS: Node<'static> = Node {
    endpoints: &[
        Endpoint {
            id: endpoints::ROOT_ENDPOINT_ID,
            device_types: devices!(rs_matter::dm::devices::DEV_TYPE_ROOT_NODE),
            clusters: clusters!(geth; OtaRequestorHandler::CLUSTER),
        },
        Endpoint {
            id: 1,
            device_types: devices!(DEV_TYPE_AGGREGATOR),
            clusters: clusters!(desc::DescHandler::CLUSTER),
        },
        Endpoint {
            // light 1
            id: 2,
            device_types: devices!(DEV_TYPE_ON_OFF_LIGHT, DEV_TYPE_BRIDGED_NODE),
            clusters: clusters!(
                desc::DescHandler::CLUSTER,
                groups::GroupsHandler::CLUSTER,
                BridgedHandler::CLUSTER,
                LightOnOffLogic::CLUSTER
            ),
        },
        Endpoint {
            // light 2
            id: 3,
            device_types: devices!(DEV_TYPE_ON_OFF_LIGHT, DEV_TYPE_BRIDGED_NODE),
            clusters: clusters!(
                desc::DescHandler::CLUSTER,
                groups::GroupsHandler::CLUSTER,
                BridgedHandler::CLUSTER,
                LightOnOffLogic::CLUSTER
            ),
        },
    ],
};

const NODE_SINGLE_LIGHT_ONLY: Node<'static> = Node {
    endpoints: &[
        Endpoint {
            id: endpoints::ROOT_ENDPOINT_ID,
            device_types: devices!(rs_matter::dm::devices::DEV_TYPE_ROOT_NODE),
            clusters: clusters!(geth; OtaRequestorHandler::CLUSTER),
        },
        Endpoint {
            id: 1,
            device_types: devices!(DEV_TYPE_AGGREGATOR),
            clusters: clusters!(desc::DescHandler::CLUSTER),
        },
        Endpoint {
            // light 1 only
            id: 2,
            device_types: devices!(DEV_TYPE_ON_OFF_LIGHT, DEV_TYPE_BRIDGED_NODE),
            clusters: clusters!(
                desc::DescHandler::CLUSTER,
                groups::GroupsHandler::CLUSTER,
                BridgedHandler::CLUSTER,
                LightOnOffLogic::CLUSTER
            ),
        },
    ],
};

/// Builds the data model handler chain for the full bridge topology.
fn dm_handler_single_light<'a>(
    mut rand: impl RngCore + Copy,
    on_off_handlers: [&'a LightOnOffHandler; 1],
) -> impl AsyncMetadata + AsyncHandler + 'a {
    (
        NODE_SINGLE_LIGHT_ONLY,
        endpoints::with_eth_sys(
            &false,
            &(),
            &SysNetifs,
            rand,
            EmptyHandler
                .chain(
                    EpClMatcher::new(Some(1), Some(desc::DescHandler::CLUSTER.id)),
                    Async(desc::DescHandler::new_aggregator(Dataver::new_rand(&mut rand)).adapt()),
                )
                .chain(
                    EpClMatcher::new(
                        Some(endpoints::ROOT_ENDPOINT_ID),
                        Some(OtaRequestorHandler::CLUSTER.id),
                    ),
                    Async(OtaRequestorHandler::new(Dataver::new_rand(&mut rand))),
                )
                .chain(
                    EpClMatcher::new(Some(2), Some(desc::DescHandler::CLUSTER.id)),
                    Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
                )
                .chain(
                    EpClMatcher::new(Some(2), Some(groups::GroupsHandler::CLUSTER.id)),
                    Async(groups::GroupsHandler::new(Dataver::new_rand(&mut rand)).adapt()),
                )
                .chain(
                    EpClMatcher::new(Some(2), Some(LightOnOffLogic::CLUSTER.id)),
                    on_off::HandlerAsyncAdaptor(on_off_handlers[0]),
                )
                .chain(
                    EpClMatcher::new(Some(2), Some(BridgedHandler::CLUSTER.id)),
                    Async(BridgedHandler::new(Dataver::new_rand(&mut rand)).adapt()),
                ),
        ),
    )
}

fn dm_second_light_enabled<'a>(
    mut rand: impl RngCore + Copy,
    on_off_handlers: [&'a LightOnOffHandler; 2],
) -> impl AsyncMetadata + AsyncHandler + 'a {
    (
        NODE_DETAILS_TWO_LIGHTS,
        endpoints::with_eth_sys(
            &false,
            &(),
            &SysNetifs,
            rand,
            EmptyHandler
                .chain(
                    EpClMatcher::new(Some(1), Some(desc::DescHandler::CLUSTER.id)),
                    Async(desc::DescHandler::new_aggregator(Dataver::new_rand(&mut rand)).adapt()),
                )
                .chain(
                    EpClMatcher::new(
                        Some(endpoints::ROOT_ENDPOINT_ID),
                        Some(OtaRequestorHandler::CLUSTER.id),
                    ),
                    Async(OtaRequestorHandler::new(Dataver::new_rand(&mut rand))),
                )
                .chain(
                    EpClMatcher::new(Some(2), Some(desc::DescHandler::CLUSTER.id)),
                    Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
                )
                .chain(
                    EpClMatcher::new(Some(2), Some(groups::GroupsHandler::CLUSTER.id)),
                    Async(groups::GroupsHandler::new(Dataver::new_rand(&mut rand)).adapt()),
                )
                .chain(
                    EpClMatcher::new(Some(2), Some(LightOnOffLogic::CLUSTER.id)),
                    on_off::HandlerAsyncAdaptor(on_off_handlers[0]),
                )
                .chain(
                    EpClMatcher::new(Some(2), Some(BridgedHandler::CLUSTER.id)),
                    Async(BridgedHandler::new(Dataver::new_rand(&mut rand)).adapt()),
                )
                .chain(
                    EpClMatcher::new(Some(3), Some(desc::DescHandler::CLUSTER.id)),
                    Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
                )
                .chain(
                    EpClMatcher::new(Some(3), Some(groups::GroupsHandler::CLUSTER.id)),
                    Async(groups::GroupsHandler::new(Dataver::new_rand(&mut rand)).adapt()),
                )
                .chain(
                    EpClMatcher::new(Some(3), Some(LightOnOffLogic::CLUSTER.id)),
                    on_off::HandlerAsyncAdaptor(on_off_handlers[1]),
                )
                .chain(
                    EpClMatcher::new(Some(3), Some(BridgedHandler::CLUSTER.id)),
                    Async(BridgedHandler::new(Dataver::new_rand(&mut rand)).adapt()),
                ),
        ),
    )
}

#[derive(Clone, Debug)]
pub struct BridgedHandler {
    dataver: Dataver,
}

impl BridgedHandler {
    /// Creates a bridged device information handler.
    pub const fn new(dataver: Dataver) -> Self {
        Self { dataver }
    }

    /// Adapts this handler to the generated cluster adaptor type.
    pub const fn adapt(self) -> bridged_device_basic_information::HandlerAdaptor<Self> {
        bridged_device_basic_information::HandlerAdaptor(self)
    }

    /// Returns the configured display name for a bridged endpoint.
    pub(crate) fn default_light_name(endpoint_id: u16) -> &'static str {
        match endpoint_id {
            2 => "Light 1",
            3 => "Light 2",
            _ => DEFAULT_LIGHT_NAME,
        }
    }

    /// Returns the stable unique identifier for a bridged endpoint.
    fn default_light_unique_id(endpoint_id: u16) -> &'static str {
        match endpoint_id {
            2 => "light-1",
            3 => "light-2",
            _ => "pnp-bridge-light",
        }
    }
}

impl bridged_device_basic_information::ClusterHandler for BridgedHandler {
    /// Cluster metadata for Bridged Device Basic Information.
    const CLUSTER: Cluster<'static> = bridged_device_basic_information::FULL_CLUSTER
        .with_features(0)
        .with_attrs(with!(
            required;
            bridged_device_basic_information::AttributeId::ProductName
                | bridged_device_basic_information::AttributeId::NodeLabel
                | bridged_device_basic_information::AttributeId::UniqueID
        ))
        .with_cmds(with!());

    /// Returns the current data version.
    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    /// Marks the data version as changed.
    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    /// Returns the product name for the bridged endpoint.
    fn product_name<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        builder.set(Self::default_light_name(ctx.endpt()))
    }

    /// Returns the node label for the bridged endpoint.
    fn node_label<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        builder.set(Self::default_light_name(ctx.endpt()))
    }

    /// Reports the bridged endpoint as reachable.
    fn reachable(&self, _ctx: impl ReadContext) -> Result<bool, Error> {
        Ok(true)
    }

    /// Returns the unique identifier for the bridged endpoint.
    fn unique_id<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        builder.set(Self::default_light_unique_id(ctx.endpt()))
    }

    /// Accepts keep-active requests without additional handling.
    fn handle_keep_active(
        &self,
        _ctx: impl InvokeContext,
        _request: KeepActiveRequest<'_>,
    ) -> Result<(), Error> {
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct OtaRequestorHandler {
    dataver: Dataver,
}

impl OtaRequestorHandler {
    /// Cluster metadata for the minimal OTA Requestor implementation.
    const CLUSTER: Cluster<'static> = Cluster::new(
        OTA_REQUESTOR_CLUSTER_ID,
        1,
        0,
        rs_matter::attributes!(Attribute::new(
            OTA_REQUESTOR_DEFAULT_PROVIDERS_ATTR_ID,
            Access::RWVA,
            Quality::A,
        )),
        &[],
        &[],
        with!(all),
        with!(),
        with!(),
    );

    /// Creates the OTA Requestor handler.
    const fn new(dataver: Dataver) -> Self {
        Self { dataver }
    }
}

impl Handler for OtaRequestorHandler {
    /// Serves OTA Requestor attribute reads.
    fn read(&self, ctx: impl ReadContext, reply: impl ReadReply) -> Result<(), Error> {
        let attr = ctx.attr();

        if attr.is_system() {
            if let Some(writer) = reply.with_dataver(self.dataver.get())? {
                return attr.cluster()?.read(attr, writer);
            }

            return Ok(());
        }

        match attr.attr_id {
            OTA_REQUESTOR_DEFAULT_PROVIDERS_ATTR_ID => {
                let Some(writer) = reply.with_dataver(self.dataver.get())? else {
                    return Ok(());
                };

                let empty: &[u8] = &[];
                writer.set(empty)
            }
            _ => Err(ErrorCode::AttributeNotFound.into()),
        }
    }

    /// Accepts writes to the default OTA provider list.
    fn write(&self, ctx: impl WriteContext) -> Result<(), Error> {
        match ctx.attr().attr_id {
            OTA_REQUESTOR_DEFAULT_PROVIDERS_ATTR_ID => {
                self.dataver.changed();
                Ok(())
            }
            _ => Err(ErrorCode::AttributeNotFound.into()),
        }
    }
}

impl NonBlockingHandler for OtaRequestorHandler {}
