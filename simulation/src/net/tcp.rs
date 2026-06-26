use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::Result;
use core_affinity::CoreId;
use crossbeam_channel::Sender;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::tcp::{OwnedReadHalf, OwnedWriteHalf},
    sync::{mpsc, oneshot, watch},
    task::{AbortHandle, JoinSet},
};
use tracing::{info, warn};

use crate::{
    cores,
    protocol::submission::{DeserializeError, RouteSubmission, SubmissionResponse, MAX_SUBMISSION_SIZE},
};

const SUBMISSION_READ_BUF: usize = 4096;
const RESPONSE_CHANNEL_CAP: usize = 1024;

// The reader's accumulator must be large enough to hold one full submission
const _: () = assert!(SUBMISSION_READ_BUF >= MAX_SUBMISSION_SIZE);

#[derive(Debug)]
pub struct ContestantSubmission {
    pub contestant_id: u64,
    pub submission: RouteSubmission,
    /// Fired exactly once when the runloop finishes evaluating this
    /// submission. If the receiver was already dropped (writer backpressure or
    /// connection closed), the send returns `Err` and the runloop counts it.
    pub response_tx: oneshot::Sender<SubmissionResponse>,
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

                    let (read_half, write_half) = stream.into_split();
                    let (resp_tx, resp_rx) = mpsc::channel::<oneshot::Receiver<SubmissionResponse>>(RESPONSE_CHANNEL_CAP,);

                    tokio::spawn(write_stream(write_half, resp_rx));

                    let handle = tasks.spawn(read_stream(
                        read_half,
                        id,
                        sub_tx.clone(),
                        resp_tx,
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

async fn read_handshake(stream: &mut tokio::net::TcpStream, registered_ids: &Arc<Mutex<HashSet<u64>>>) -> Result<u64> {
    let mut buf = [0u8; 8];
    stream.read_exact(&mut buf).await?;
    let id = u64::from_le_bytes(buf);
    let known = registered_ids.lock().expect("can't acquire registered_ids mutex").contains(&id);
    if !known {
        anyhow::bail!("contestant id {id} not registered");
    }
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
async fn read_stream(
    mut read_half: OwnedReadHalf,
    contestant_id: u64,
    sub_tx: Sender<ContestantSubmission>,
    resp_tx: mpsc::Sender<oneshot::Receiver<SubmissionResponse>>,
    streams_disconnected: Arc<AtomicU64>,
    bytes_read: Arc<AtomicU64>,
    submissions_forwarded: Arc<AtomicU64>,
    submissions_dropped_bad: Arc<AtomicU64>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut acc = [0u8; SUBMISSION_READ_BUF];
    let mut len = 0usize;
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => return,
            res = read_half.read(&mut acc[len..]) => match res {
                Ok(0) => {
                    streams_disconnected.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Ok(n) => {
                    bytes_read.fetch_add(n as u64, Ordering::Relaxed);
                    len += n;

                    let mut cursor = 0;
                    loop {
                        match RouteSubmission::deserialize(&acc[cursor..len]) {
                            Ok((submission, consumed)) => {
                                let (response_tx, response_rx) = oneshot::channel();
                                // If the writer queue is full, drop the receiver
                                // immediately. The runloop's send will then fail
                                // and bump its own dropped-response counter.
                                let _ = resp_tx.try_send(response_rx);
                                let msg = ContestantSubmission {
                                    contestant_id,
                                    submission,
                                    response_tx,
                                };
                                if sub_tx.send(msg).is_err() {
                                    return;
                                }
                                submissions_forwarded.fetch_add(1, Ordering::Relaxed);
                                cursor += consumed;
                            }
                            Err(DeserializeError::Incomplete) => break,
                            Err(e) => {
                                warn!(contestant_id, error = %e, "bad submission framing; dropping stream");
                                submissions_dropped_bad.fetch_add(1, Ordering::Relaxed);
                                return;
                            }
                        }
                    }

                    if cursor > 0 {
                        acc.copy_within(cursor..len, 0);
                        len -= cursor;
                    }
                }
                Err(_) => {
                    streams_disconnected.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        }
    }
}

async fn write_stream(mut write_half: OwnedWriteHalf, mut resp_rx: mpsc::Receiver<oneshot::Receiver<SubmissionResponse>>) {
    while let Some(rx) = resp_rx.recv().await {
        if let Ok(resp) = rx.await {
            if write_half.write_all(resp.as_bytes()).await.is_err() {
                return;
            }
        }
    }
}
