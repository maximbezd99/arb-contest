use std::net::SocketAddr;

use anyhow::Context;
use axum::{extract::State, http::header, response::IntoResponse, routing::get, Router};
use bytes::Bytes;
use tokio::net::TcpListener;
use tracing::info;

use crate::protocol::market::{serialize_market, Market};

#[derive(Debug, Clone)]
pub struct HttpServerState {
    pub market_bytes: Bytes,
}

pub async fn run(bind: SocketAddr, market: Market) -> anyhow::Result<()> {
    let market_bytes = Bytes::from(serialize_market(&market));
    let state = HttpServerState { market_bytes };

    let app = Router::new()
        .route("/market", get(get_market))
        .with_state(state);

    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind http {bind}"))?;
    info!(local = %listener.local_addr()?, "http listener bound");

    axum::serve(listener, app).await?;

    Err(anyhow::anyhow!("http server stopped"))
}

async fn get_market(State(state): State<HttpServerState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        state.market_bytes,
    )
}
