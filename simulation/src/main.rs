mod config;
mod generation;
mod net;
mod protocol;

use clap::Parser;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::config::SimConfig;
use crate::generation::generate_feed::{feed_stats, generate_feed};
use crate::generation::generate_market::generate_market;

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
    gen_cfg.sanity_check()?;
    info!(?gen_cfg, "generation config loaded and validated");

    const MARKET_SEED: u64 = 0;
    const FEED_SEED: u64 = 1;

    let market = generate_market(&gen_cfg, MARKET_SEED)?;
    info!(
        tokens = market.tokens.len(),
        pairs = market.pairs.len(),
        "market generated"
    );

    let feed = generate_feed(&market, &gen_cfg, FEED_SEED)?;
    let stats = feed_stats(&feed);
    info!(
        ticks = stats.total_ticks,
        duration_ms = stats.duration_us / 1_000,
        avg_ticks_per_ms = format!("{:.2}", stats.avg_ticks_per_ms),
        peak_ticks_per_ms = stats.peak_ticks_per_ms,
        pairs_touched = stats.distinct_pairs_touched,
        total_pairs = market.pairs.len(),
        min_ticks_per_pair = stats.min_ticks_per_touched_pair,
        avg_ticks_per_pair = format!("{:.2}", stats.avg_ticks_per_touched_pair),
        max_ticks_per_pair = stats.max_ticks_per_touched_pair,
        "feed generated"
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
