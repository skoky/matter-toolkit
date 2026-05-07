use rs_matter::Matter;
use rs_matter::{crypto::Crypto, error::Error};

use socket2::{Domain, Protocol, Socket, Type};

/// Runs the bridge mDNS responder.
pub async fn run_mdns<C: Crypto>(matter: &Matter<'_>, crypto: C) -> Result<(), Error> {
    run_builtin_mdns(matter, crypto).await
}

/// Runs the built-in rs-matter mDNS responder on the selected network interface.
async fn run_builtin_mdns<C: Crypto>(matter: &Matter<'_>, crypto: C) -> Result<(), Error> {
    use std::net::UdpSocket;

    use log::{debug, error, info, warn};

    use rs_matter::transport::network::{Ipv4Addr, Ipv6Addr};

    /// Selects the best non-loopback interface for mDNS.
    fn initialize_network() -> Result<(Option<Ipv4Addr>, Option<Ipv6Addr>, Option<u32>), Error> {
        use rs_matter::error::ErrorCode;

        let all = if_addrs::get_if_addrs().map_err(|_| ErrorCode::StdIoError)?;
        debug!("available network interfaces: {:?}", all);

        #[derive(Clone, Debug, Default)]
        struct Candidate {
            name: String,
            ipv4: Option<std::net::Ipv4Addr>,
            ipv6: Option<std::net::Ipv6Addr>,
            index: Option<u32>,
        }

        fn score(candidate: &Candidate) -> i32 {
            let mut score = 0;

            if let Some(ipv4) = candidate.ipv4 {
                score += 10;
                if ipv4.is_private() {
                    score += 20;
                }
                if !ipv4.is_link_local() {
                    score += 5;
                }
            }

            if let Some(ipv6) = candidate.ipv6 {
                score += 10;
                if ipv6.is_unicast_link_local() {
                    score += 20;
                } else if !ipv6.is_unspecified() {
                    score += 5;
                }
            }

            score
        }

        let mut candidates = std::collections::BTreeMap::<String, Candidate>::new();

        for ia in all.iter().filter(|ia| !ia.is_loopback()) {
            let entry = candidates
                .entry(ia.name.clone())
                .or_insert_with(|| Candidate {
                    name: ia.name.clone(),
                    index: ia.index,
                    ..Candidate::default()
                });

            if entry.index.is_none() {
                entry.index = ia.index;
            }

            match ia.addr {
                if_addrs::IfAddr::V4(ref v4) if !v4.ip.is_unspecified() => {
                    if entry.ipv4.is_none() {
                        entry.ipv4 = Some(v4.ip);
                    }
                }
                if_addrs::IfAddr::V6(ref v6) if !v6.ip.is_unspecified() => {
                    let prefer_new = match entry.ipv6 {
                        None => true,
                        Some(current) => {
                            !current.is_unicast_link_local() && v6.ip.is_unicast_link_local()
                        }
                    };

                    if prefer_new {
                        entry.ipv6 = Some(v6.ip);
                    }
                }
                _ => {}
            }
        }

        let candidate = candidates
            .into_values()
            .filter(|candidate| candidate.ipv4.is_some() || candidate.ipv6.is_some())
            .max_by_key(score)
            .ok_or_else(|| {
                error!("cannot find network interface suitable for mDNS broadcasting");
                ErrorCode::StdIoError
            })?;

        if candidate.ipv6.is_none() {
            warn!(
                "selected interface {} has no usable IPv6 address; mDNS will use IPv4 only",
                candidate.name
            );
        }

        if candidate.ipv4.is_none() {
            warn!(
                "selected interface {} has no usable IPv4 address; mDNS will use IPv6 only",
                candidate.name
            );
        }

        info!(
            "using network interface {} with ipv4={:?} ipv6={:?} index={:?} for mDNS",
            candidate.name, candidate.ipv4, candidate.ipv6, candidate.index
        );

        Ok((
            candidate.ipv4.map(|ip| ip.octets().into()),
            candidate.ipv6.map(|ip| ip.octets().into()),
            candidate.index,
        ))
    }

    let (ipv4_addr, ipv6_addr, interface) = initialize_network()?;

    use rs_matter::transport::network::mdns::builtin::{BuiltinMdnsResponder, Host};
    use rs_matter::transport::network::mdns::{
        MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_SOCKET_DEFAULT_BIND_ADDR,
    };

    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_only_v6(false)?;
    socket.bind(&MDNS_SOCKET_DEFAULT_BIND_ADDR.into())?;
    let socket = async_io::Async::<UdpSocket>::new_nonblocking(socket.into())?;

    if let Some(interface) = interface {
        if ipv6_addr.is_some() {
            socket
                .get_ref()
                .join_multicast_v6(&MDNS_IPV6_BROADCAST_ADDR, interface)?;
        }
    }

    if let Some(ipv4_addr) = ipv4_addr {
        socket
            .get_ref()
            .join_multicast_v4(&MDNS_IPV4_BROADCAST_ADDR, &ipv4_addr)?;
    }

    BuiltinMdnsResponder::new(matter, crypto)
        .run(
            &socket,
            &socket,
            &Host {
                id: 0,
                hostname: "bridge-test",
                ip: ipv4_addr.unwrap_or(Ipv4Addr::UNSPECIFIED),
                ipv6: ipv6_addr.unwrap_or(Ipv6Addr::UNSPECIFIED),
            },
            ipv4_addr,
            interface.filter(|_| ipv6_addr.is_some()),
        )
        .await
}
