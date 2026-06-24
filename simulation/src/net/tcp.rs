use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Result;
use core_affinity::CoreId;
use crossbeam_channel::Sender;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;
use tokio::task::{AbortHandle, JoinSet};
use tracing::{info, warn};

use crate::cores;
use crate::protocol::submission::RouteSubmission;

const SUBMISSION_READ_BUF: usize = 4096;

#[derive(Debug)]
pub struct ContestantSubmission {
    pub contestant_id: u64,
    pub submission: RouteSubmission,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TcpReceiveOutcome {
    pub streams_accepted: u64,
    pub streams_rejected: u64,
    pub streams_replaced: u64,
    pub streams_disconnected: u64,
    pub bytes_read: u64,
    pub submissions_forwarded: u64,
    pub submissions_dropped_bad: u64,
}

pub fn spawn(
    core_id: CoreId,
    bind: SocketAddr,
    sub_tx: Sender<ContestantSubmission>,
    registered_ids: Arc<Mutex<HashSet<u64>>>,
    shutdown: Arc<AtomicBool>,
) -> Result<JoinHandle<TcpReceiveOutcome>> {
    let handle = thread::Builder::new()
        .name("tcp-receive".into())
        .spawn(move || {
            cores::pin_and_verify(core_id);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .expect("build tokio current_thread runtime for tcp-receive");
            rt.block_on(run(bind, sub_tx, registered_ids, shutdown))
        })
        .expect("spawn tcp-receive thread");

    Ok(handle)
}

async fn run(
    bind: SocketAddr,
    sub_tx: Sender<ContestantSubmission>,
    registered_ids: Arc<Mutex<HashSet<u64>>>,
    shutdown: Arc<AtomicBool>,
) -> TcpReceiveOutcome {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .expect("can't bind submissions tcp listener");

    let streams_accepted = Arc::new(AtomicU64::new(0));
    let streams_rejected = Arc::new(AtomicU64::new(0));
    let streams_replaced = Arc::new(AtomicU64::new(0));
    let streams_disconnected = Arc::new(AtomicU64::new(0));
    let bytes_read = Arc::new(AtomicU64::new(0));
    let submissions_forwarded = Arc::new(AtomicU64::new(0));
    let submissions_dropped_bad = Arc::new(AtomicU64::new(0));

    let (shutdown_tx, _) = watch::channel(false);
    {
        let shutdown_flag = shutdown.clone();
        let shutdown_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            while !shutdown_flag.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let _ = shutdown_tx.send(true);
        });
    }

    let mut tasks = JoinSet::new();
    let mut active: HashMap<u64, AbortHandle> = HashMap::new();
    let mut shutdown_rx = shutdown_tx.subscribe();

    loop {
        tokio::select! {
            res = listener.accept() => match res {
                Ok((mut stream, peer)) => {
                    let id = match read_handshake(&mut stream, &registered_ids).await {
                        Ok(id) => id,
                        Err(e) => {
                            warn!(?peer, error = %e, "submission handshake failed; dropping");
                            streams_rejected.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    };

                    if let Some(prior) = active.remove(&id) {
                        prior.abort();
                        streams_replaced.fetch_add(1, Ordering::Relaxed);
                        info!(contestant_id = id, ?peer, "replaced existing submission stream");
                    } else {
                        info!(contestant_id = id, ?peer, "submission stream accepted");
                    }

                    streams_accepted.fetch_add(1, Ordering::Relaxed);
                    let handle = tasks.spawn(read_stream(
                        stream,
                        id,
                        sub_tx.clone(),
                        streams_disconnected.clone(),
                        bytes_read.clone(),
                        submissions_forwarded.clone(),
                        submissions_dropped_bad.clone(),
                        shutdown_tx.subscribe(),
                    ));
                    active.insert(id, handle);
                }
                Err(e) => warn!(error = %e, "submission accept failed"),
            },
            _ = shutdown_rx.changed() => break,
        };
    }

    TcpReceiveOutcome {
        streams_accepted: streams_accepted.load(Ordering::Relaxed),
        streams_rejected: streams_rejected.load(Ordering::Relaxed),
        streams_replaced: streams_replaced.load(Ordering::Relaxed),
        streams_disconnected: streams_disconnected.load(Ordering::Relaxed),
        bytes_read: bytes_read.load(Ordering::Relaxed),
        submissions_forwarded: submissions_forwarded.load(Ordering::Relaxed),
        submissions_dropped_bad: submissions_dropped_bad.load(Ordering::Relaxed),
    }
}

async fn read_handshake(
    stream: &mut tokio::net::TcpStream,
    registered_ids: &Arc<Mutex<HashSet<u64>>>,
) -> Result<u64> {
    let mut buf = [0u8; 8];
    stream.read_exact(&mut buf).await?;
    let id = u64::from_le_bytes(buf);
    let known = registered_ids
        .lock()
        .expect("can't acquire registered_ids mutex")
        .contains(&id);
    if !known {
        anyhow::bail!("contestant id {id} not registered");
    }
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
async fn read_stream(
    mut stream: tokio::net::TcpStream,
    contestant_id: u64,
    sub_tx: Sender<ContestantSubmission>,
    streams_disconnected: Arc<AtomicU64>,
    bytes_read: Arc<AtomicU64>,
    submissions_forwarded: Arc<AtomicU64>,
    submissions_dropped_bad: Arc<AtomicU64>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut buf = [0u8; SUBMISSION_READ_BUF];
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => return,
            res = stream.read(&mut buf) => match res {
                Ok(0) => {
                    streams_disconnected.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Ok(n) => {
                    bytes_read.fetch_add(n as u64, Ordering::Relaxed);
                    let submission = match RouteSubmission::deserialize(&buf[..n]) {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(
                                contestant_id,
                                error = %e,
                                bytes = n,
                                "failed to parse submission; dropping",
                            );
                            submissions_dropped_bad.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    };
                    let msg = ContestantSubmission {
                        contestant_id,
                        submission,
                    };
                    if sub_tx.send(msg).is_err() {
                        return;
                    }
                    submissions_forwarded.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    streams_disconnected.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        }
    }
}
