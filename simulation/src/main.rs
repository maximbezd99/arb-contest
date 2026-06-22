mod config;
mod generation;
mod net;
mod protocol;

use clap::Parser;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::config::SimConfig;
use crate::generation::generate_market::generate;

const MARKET_SEED: u64 = 0;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cfg = SimConfig::parse();
    info!(?cfg, "starting simulation server");

    let gen_cfg = generation::config::load_config()?;
    gen_cfg.check_no_overflow()?;
    info!(?gen_cfg, "generation config loaded and validated");

    let market = generate(&gen_cfg, MARKET_SEED)?;
    info!(
        tokens = market.tokens.len(),
        pairs = market.pairs.len(),
        "market generated"
    );

    let http_task = tokio::spawn(net::http::run(cfg.http_bind, market));

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("shutdown signal received");
        }
        res = http_task => log_task("http", res),
    }

    Ok(())
}

fn log_task(name: &str, res: Result<anyhow::Result<()>, tokio::task::JoinError>) {
    match res {
        Ok(Ok(())) => info!(task = name, "task finished"),
        Ok(Err(e)) => error!(task = name, error = %e, "task failed"),
        Err(e) => error!(task = name, error = %e, "task panicked"),
    }
}
