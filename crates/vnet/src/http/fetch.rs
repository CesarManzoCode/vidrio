//! Network thread pool: requests run on up to `MAX_NETWORK_THREADS` threads
//! and report progress over a channel, so the main thread never blocks on I/O.

use super::client::{Client, Event};
use super::{HttpError, Request};
use std::cell::Cell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

/// Bound of the pool (contract §3.2).
pub const MAX_NETWORK_THREADS: usize = 8;

pub type FetchId = u64;

/// One event of one request, tagged with the id returned by [`Fetcher::start`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchMessage {
    pub id: FetchId,
    pub event: Event,
}

struct Job {
    id: FetchId,
    request: Request,
    reply: Sender<FetchMessage>,
}

pub struct Fetcher {
    jobs: Option<Sender<Job>>,
    next_id: AtomicU64,
    cancelled: Arc<Mutex<HashSet<FetchId>>>,
}

impl Fetcher {
    /// Start `threads` workers (clamped to 1..=8) sharing `client`.
    pub fn new(client: Arc<Client>, threads: usize) -> Fetcher {
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let cancelled = Arc::new(Mutex::new(HashSet::new()));
        for i in 0..threads.clamp(1, MAX_NETWORK_THREADS) {
            let rx = Arc::clone(&rx);
            let client = Arc::clone(&client);
            let cancelled = Arc::clone(&cancelled);
            thread::Builder::new()
                .name(format!("vnet-{i}"))
                .spawn(move || worker(&client, &rx, &cancelled))
                .expect("spawn network thread");
        }
        Fetcher {
            jobs: Some(tx),
            next_id: AtomicU64::new(1),
            cancelled,
        }
    }

    /// Queue `request`; its events go to `reply` in order, ending with
    /// `Done` or `Error`.
    pub fn start(&self, request: Request, reply: Sender<FetchMessage>) -> FetchId {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let job = Job { id, request, reply };
        if let Some(jobs) = &self.jobs
            && let Err(mpsc::SendError(job)) = jobs.send(job)
        {
            let _ = job.reply.send(FetchMessage {
                id,
                event: Event::Error(HttpError::Cancelled),
            });
        }
        id
    }

    /// Stop a request; no further events are sent for it after the worker
    /// notices (between reads).
    pub fn cancel(&self, id: FetchId) {
        self.cancelled.lock().unwrap().insert(id);
    }
}

impl Drop for Fetcher {
    fn drop(&mut self) {
        // Workers finish their current request and exit; they are not joined
        // so dropping never blocks on the network.
        self.jobs.take();
    }
}

fn worker(client: &Client, rx: &Mutex<Receiver<Job>>, cancelled: &Mutex<HashSet<FetchId>>) {
    loop {
        let job = {
            let rx = rx.lock().unwrap();
            rx.recv()
        };
        let Ok(job) = job else {
            return;
        };
        let id = job.id;
        let gone = Cell::new(false);
        let is_cancelled = || gone.get() || cancelled.lock().unwrap().contains(&id);
        let mut sink = |event: Event| {
            if gone.get() {
                return;
            }
            if job.reply.send(FetchMessage { id, event }).is_err() {
                gone.set(true);
            }
        };
        let result = if is_cancelled() {
            Err(HttpError::Cancelled)
        } else {
            client.send(&job.request, &is_cancelled, &mut sink)
        };
        let was_cancelled = cancelled.lock().unwrap().remove(&id);
        if let Err(e) = result
            && !was_cancelled
            && !gone.get()
        {
            let _ = job.reply.send(FetchMessage {
                id,
                event: Event::Error(e),
            });
        }
    }
}
