use std::net::SocketAddr;
use std::net::{IpAddr, UdpSocket};
use std::thread::{self, JoinHandle};

use anyhow::Result;
use core_affinity::CoreId;
use crossbeam_channel::Receiver;
use tracing::info;

use crate::cores;
use crate::protocol::feed::PriceUpdate;

#[derive(Debug, Clone, Copy, Default)]
pub struct UdpSendOutcome {
    pub sent: u64,
    pub send_errors: u64,
}

pub fn spawn(
    core_id: CoreId,
    bind: SocketAddr,
    target: SocketAddr,
    rx: Receiver<PriceUpdate>,
) -> Result<JoinHandle<UdpSendOutcome>> {
    let udp_socket = UdpSocket::bind(bind)?;
    if let IpAddr::V4(_) = target.ip() {
        udp_socket.set_multicast_ttl_v4(1)?;
        udp_socket.set_multicast_loop_v4(true)?;
    }
    udp_socket.connect(target)?;
    let is_mcast = match target.ip() {
        IpAddr::V4(a) => a.is_multicast(),
        IpAddr::V6(a) => a.is_multicast(),
    };
    info!(
        bind = ?udp_socket.local_addr()?,
        target = ?target,
        multicast = is_mcast,
        "udp feed socket ready",
    );

    let handle = thread::Builder::new()
        .name("udp-send".into())
        .spawn(move || {
            cores::pin_and_verify(core_id);
            run(udp_socket, rx)
        })
        .expect("spawn udp-send thread");

    Ok(handle)
}

fn run(socket: UdpSocket, rx: Receiver<PriceUpdate>) -> UdpSendOutcome {
    let mut sent = 0u64;
    let mut send_errors = 0u64;
    while let Ok(update) = rx.recv() {
        match socket.send(update.as_bytes()) {
            Ok(_) => sent += 1,
            Err(_) => send_errors += 1,
        }
    }
    UdpSendOutcome { sent, send_errors }
}
