mod config;
mod cores;
mod engine;
mod generation;
mod net;
mod protocol;

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use anyhow::{anyhow, Result};
use bytes::Bytes;
use clap::Parser;
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    iterator::Signals,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::{
    config::SimConfig,
    engine::{contestant::Contestant, runloop},
    generation::{
        generate_feed::{feed_stats, generate_feed},
        generate_market::generate_market,
    },
    net::http,
    protocol::market::serialize_market,
};

const BOOT_WAIT: Duration = Duration::from_secs(30);

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let cfg = SimConfig::parse();
    info!(?cfg, "starting simulation server");

    let cores = cores::pick_cores()?;

    let market_seed = cfg.seed;
    let feed_seed = cfg.seed.wrapping_add(1);
    let contestant_id_seed = cfg.seed.wrapping_add(2);

    let registered_ids = Arc::new(Mutex::new(HashSet::<u64>::new()));
    let ready_ids = Arc::new(Mutex::new(HashSet::<u64>::new()));

    let shutdown_flag = Arc::new(AtomicBool::new(false));
    std::thread::spawn({
        let shutdown_flag = shutdown_flag.clone();
        move || block(shutdown_flag.clone())
    });

    let gen_cfg = generation::config::load_config()?;
    gen_cfg.sanity_check()?;
    info!(?gen_cfg, "generation config loaded and validated");

    let market = generate_market(&gen_cfg, market_seed)?;
    info!(tokens = market.tokens.len(), pairs = market.pairs.len(), "market generated");

    let feed = generate_feed(&market, &gen_cfg, feed_seed)?;
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

    let (udp_tx, udp_rx) = crossbeam_channel::unbounded();
    let udp_handle = net::udp::spawn(cores.udp, cfg.udp_bind, cfg.udp_target, udp_rx)?;

    let (tcp_tx, tcp_rx) = crossbeam_channel::unbounded();
    let tcp_handle = net::tcp::spawn(
        cores.tcp,
        cfg.tcp_submission_bind,
        tcp_tx,
        registered_ids.clone(),
        shutdown_flag.clone(),
    )?;

    let market_bytes = Bytes::from(serialize_market(&market));
    let market_json_bytes = Bytes::from(serde_json::to_vec(&market).expect("serialize market to json"));

    let configuration_complete = Arc::new(AtomicBool::new(false));
    let http_state = http::HttpServerState::new(
        market_bytes,
        market_json_bytes,
        contestant_id_seed,
        registered_ids.clone(),
        ready_ids.clone(),
        configuration_complete.clone(),
    );
    let _ = http::spawn(cores.tokio, cfg.http_bind, http_state);

    info!(
        expected = cfg.expected_contestants,
        boot_wait_ms = BOOT_WAIT.as_millis() as u64,
        "simulation ready; waiting for contestants to /register, connect, and /ready",
    );

    let mut elapsed_sec = Duration::ZERO;
    loop {
        let one_sec = Duration::from_secs(1);
        std::thread::sleep(one_sec);

        if shutdown_flag.load(Ordering::Relaxed) {
            return Ok(());
        }

        let current_contestants = ready_ids.lock().expect("can't acquire ready_ids lock in main").len();

        if current_contestants > cfg.expected_contestants {
            panic!("More contestants than expected");
        } else if current_contestants == cfg.expected_contestants {
            break;
        } else if elapsed_sec > BOOT_WAIT {
            panic!("Contestants didn't connect");
        }

        elapsed_sec += one_sec;
    }

    configuration_complete.store(true, Ordering::Relaxed);

    let contestants: std::collections::HashMap<u64, Contestant> = ready_ids
        .lock()
        .expect("can't acquire ready_ids lock to seed contestants")
        .iter()
        .map(|&id| {
            (
                id,
                Contestant::new_with_decimals(id, cfg.initial_balance_usd, gen_cfg.usd_decimals as i64),
            )
        })
        .collect();

    let runloop_handle = runloop::spawn(
        cores.runloop,
        feed,
        market,
        contestants,
        udp_tx.clone(),
        tcp_rx,
        shutdown_flag.clone(),
    );

    let outcome = runloop_handle.join().map_err(|_| anyhow!("fail to join runloop thread"))?;
    info!(
        dispatched = outcome.dispatched,
        udp_dispatch_errors = outcome.udp_dispatch_errors,
        submissions_received = outcome.submissions_received,
        submissions_ok = outcome.submissions_ok,
        submissions_fail = outcome.submissions_fail,
        responses_dropped = outcome.responses_dropped,
        final_overshoot_ns = outcome.final_overshoot_ns,
        late_ticks = outcome.late_ticks,
        "engine thread finished:",
    );

    outcome.iter_stats.info();
    outcome.read_stats.info();
    outcome.wait_stats.info();
    outcome.send_stats.info();
    outcome.overshoot_stats.info();
    outcome.udp_queue_depth_stats.info();

    shutdown_flag.store(true, Ordering::Relaxed);
    let drain_started = std::time::Instant::now();
    drop(udp_tx);

    let udp_sender_outcome = udp_handle.join().map_err(|_| anyhow!("fail to join udp thread"))?;
    info!(
        sent = udp_sender_outcome.sent,
        send_errors = udp_sender_outcome.send_errors,
        drain_duration_ms = drain_started.elapsed().as_millis() as u64,
        "udp thread finished:",
    );

    let submissions_outcome = tcp_handle.join().map_err(|_| anyhow!("fail to join submissions thread"))?;

    info!(
        streams_accepted = submissions_outcome.streams_accepted,
        streams_rejected = submissions_outcome.streams_rejected,
        streams_replaced = submissions_outcome.streams_replaced,
        streams_disconnected = submissions_outcome.streams_disconnected,
        bytes_read = submissions_outcome.bytes_read,
        submissions_forwarded = submissions_outcome.submissions_forwarded,
        submissions_dropped_bad = submissions_outcome.submissions_dropped_bad,
        "submissions thread finished:",
    );

    Ok(())
}

fn block(shutdown: Arc<AtomicBool>) {
    let mut signals = Signals::new([SIGTERM, SIGINT]).expect("can't construct signals list");
    if signals.forever().next().is_some() {
        info!("Shutdown signal received");
        shutdown.store(true, Ordering::Relaxed);
    }
}
