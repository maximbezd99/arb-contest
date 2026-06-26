use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
};

use anyhow::{Context, Result};
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use core_affinity::CoreId;
use rand::{RngCore, SeedableRng};
use rand_pcg::Pcg64Mcg;
use tokio::net::TcpListener;
use tracing::info;

use crate::cores;

#[derive(Clone)]
pub struct HttpServerState {
    market_bytes: Bytes,
    market_json_bytes: Bytes,
    /// Seeded RNG used to mint contestant IDs at `/register` time.
    contestant_id_rng: Arc<Mutex<Pcg64Mcg>>,
    /// IDs handed out by `/register`.
    registered_ids: Arc<Mutex<HashSet<u64>>>,
    /// IDs that have called `/ready`.
    ready_ids: Arc<Mutex<HashSet<u64>>>,
    /// After it's true - all http requests are ignored.
    configuration_complete: Arc<AtomicBool>,
}

impl HttpServerState {
    pub fn new(
        market_bytes: Bytes,
        market_json_bytes: Bytes,
        seed: u64,
        registered_ids: Arc<Mutex<HashSet<u64>>>,
        ready_ids: Arc<Mutex<HashSet<u64>>>,
        configuration_complete: Arc<AtomicBool>,
    ) -> Self {
        Self {
            market_bytes,
            market_json_bytes,
            contestant_id_rng: Arc::new(Mutex::new(Pcg64Mcg::seed_from_u64(seed))),
            registered_ids,
            ready_ids,
            configuration_complete,
        }
    }
}

pub fn spawn(core_id: CoreId, bind: SocketAddr, state: HttpServerState) -> thread::JoinHandle<Result<()>> {
    thread::Builder::new()
        .name("http-server".into())
        .spawn(move || {
            cores::pin_and_verify(core_id);

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .expect("build tokio current_thread runtime for http server");

            rt.block_on(server(bind, state))
        })
        .expect("spawn http server thread")
}

async fn server(bind: SocketAddr, state: HttpServerState) -> Result<()> {
    let app = Router::new()
        .route("/health", get(get_health))
        .route("/market", get(get_market))
        .route("/market/json", get(get_market_json))
        .route("/register", post(post_register))
        .route("/:id/ready", post(post_ready))
        .with_state(state);

    let listener = TcpListener::bind(bind).await.with_context(|| format!("bind http {bind}"))?;
    info!(local = %listener.local_addr()?, "http listener bound");

    axum::serve(listener, app).await?;

    Ok(())
}

async fn get_health() -> impl IntoResponse {
    (StatusCode::OK, "ok").into_response()
}

async fn get_market(State(state): State<HttpServerState>) -> impl IntoResponse {
    if state.configuration_complete.load(Ordering::Relaxed) {
        return (StatusCode::SERVICE_UNAVAILABLE).into_response();
    }

    let bytes = state.market_bytes.clone();
    ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response()
}

async fn get_market_json(State(state): State<HttpServerState>) -> impl IntoResponse {
    if state.configuration_complete.load(Ordering::Relaxed) {
        return (StatusCode::SERVICE_UNAVAILABLE).into_response();
    }

    let bytes = state.market_json_bytes.clone();
    ([(header::CONTENT_TYPE, "application/json")], bytes).into_response()
}

async fn post_register(State(state): State<HttpServerState>) -> impl IntoResponse {
    if state.configuration_complete.load(Ordering::Relaxed) {
        return (StatusCode::SERVICE_UNAVAILABLE).into_response();
    }

    let id = state
        .contestant_id_rng
        .lock()
        .expect("can't acquire contestant_id_rng mutex")
        .next_u64();

    state.registered_ids.lock().expect("can't acquire registered_ids mutex").insert(id);

    info!(contestant_id = id, "registered new contestant");

    let response = (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        Bytes::copy_from_slice(&id.to_le_bytes()),
    );
    response.into_response()
}

async fn post_ready(Path(id): Path<u64>, State(state): State<HttpServerState>) -> impl IntoResponse {
    if state.configuration_complete.load(Ordering::Relaxed) {
        return (StatusCode::SERVICE_UNAVAILABLE).into_response();
    }

    if !state
        .registered_ids
        .lock()
        .expect("can't acquire registered_ids mutex")
        .contains(&id)
    {
        return (StatusCode::FORBIDDEN, "id not registered").into_response();
    }

    state.ready_ids.lock().expect("can't acquire ready_ids mutex").insert(id);

    info!(contestant_id = id, "contestant ready");

    StatusCode::OK.into_response()
}
