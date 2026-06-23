use std::net::SocketAddr;

use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "simulation", about = "Arbitrage Arena simulation server")]
pub struct SimConfig {
    /// Address the HTTP listener binds to (serves `GET /market`).
    #[arg(long, env = "SIM_HTTP_BIND", default_value = "0.0.0.0:9003")]
    pub http_bind: SocketAddr,
}
