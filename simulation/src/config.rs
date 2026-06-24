use std::net::SocketAddr;

use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "simulation", about = "Arbitrage Arena simulation server")]
pub struct SimConfig {
    /// Address the HTTP listener binds to.
    #[arg(long, env = "SIM_HTTP_BIND", default_value = "0.0.0.0:9003")]
    pub http_bind: SocketAddr,
    /// Local address the UDP feed sender binds to.
    #[arg(long, env = "SIM_UDP_BIND", default_value = "0.0.0.0:0")]
    pub udp_bind: SocketAddr,
    /// Destination address the UDP feed sender.
    #[arg(long, env = "SIM_UDP_TARGET", default_value = "239.42.0.1:9001")]
    pub udp_target: SocketAddr,
    /// Address the TCP submission listener binds to.
    #[arg(long, env = "SIM_TCP_BIND", default_value = "0.0.0.0:9002")]
    pub tcp_submission_bind: SocketAddr,
    /// Number of contestants expected to call `/ready` before the runloop starts.
    #[arg(long, env = "SIM_EXPECTED_CONTESTANTS", default_value = "1")]
    pub expected_contestants: usize,
    /// Master seed.
    #[arg(long, env = "SEED", default_value = "0")]
    pub seed: u64,
}
