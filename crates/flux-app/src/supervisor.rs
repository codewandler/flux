//! Single-owner event delivery for [`crate::App`].
//!
//! Public bus subscriptions are observation-only. This actor is the sole consumer that routes
//! events into triggers, which keeps direct deliveries, long-running supervision, and public bus
//! emission from duplicating or consuming one another's cascades.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch, OnceCell};

use flux_core::{Error, Result};

use crate::app::{Engine, JourneyRun, RecordingSink};
use crate::bus::{delivery_origin, scope_delivery, DeliveryOrigin, Event, RunContext, CAPACITY};

/// Bounds an `emit` loop driven by one synchronous [`crate::App::deliver`] request.
const MAX_CASCADE: u32 = 256;

static NEXT_SUPERVISOR_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) enum DeliveryMessage {
    Event {
        event: Event,
        run: RunContext,
    },
    Deliver {
        event: Event,
        response: oneshot::Sender<Result<Vec<JourneyRun>>>,
    },
    Start {
        event: Event,
        completed: Arc<AtomicBool>,
        observed: Arc<AtomicBool>,
        run: RunContext,
        response: oneshot::Sender<Result<()>>,
    },
}

pub(crate) struct DeliverySupervisor {
    id: u64,
    handle: OnceCell<SupervisorHandle>,
}

struct SupervisorHandle {
    sender: mpsc::Sender<DeliveryMessage>,
    external_run: Arc<Mutex<Option<RunContext>>>,
    startup_sent: Arc<AtomicBool>,
    startup_observed: Arc<AtomicBool>,
    stopped: watch::Receiver<Option<Arc<str>>>,
}

struct RunLease {
    external_run: Arc<Mutex<Option<RunContext>>>,
    run: RunContext,
    errors: mpsc::Receiver<String>,
}

impl DeliverySupervisor {
    pub(crate) fn new() -> Self {
        Self {
            id: NEXT_SUPERVISOR_ID.fetch_add(1, Ordering::Relaxed),
            handle: OnceCell::new(),
        }
    }

    pub(crate) async fn deliver(
        &self,
        engine: &Arc<Engine>,
        label: String,
        payload: Value,
    ) -> Result<Vec<JourneyRun>> {
        if delivery_origin().is_some_and(|origin| origin.supervisor == self.id) {
            return Err(Error::Other(
                "App::deliver cannot re-enter its active delivery root".into(),
            ));
        }
        let handle = self.ensure_started(engine).await;
        let (response, result) = oneshot::channel();
        handle
            .sender
            .send(DeliveryMessage::Deliver {
                event: Event::new(label, payload),
                response,
            })
            .await
            .map_err(|_| stopped_error(handle, "delivery supervisor stopped"))?;
        result
            .await
            .map_err(|_| stopped_error(handle, "delivery supervisor dropped the request"))?
    }

    pub(crate) async fn run(&self, engine: &Arc<Engine>) -> Result<()> {
        if delivery_origin().is_some_and(|origin| origin.supervisor == self.id) {
            return Err(Error::Other(
                "App::run cannot re-enter its active delivery root".into(),
            ));
        }
        let handle = self.ensure_started(engine).await;
        let mut lease = RunLease::acquire(handle.external_run.clone())?;
        if !handle.startup_sent.load(Ordering::Acquire) {
            let (response, result) = oneshot::channel();
            handle
                .sender
                .send(DeliveryMessage::Start {
                    event: Event::new("startup", serde_json::json!({})),
                    completed: handle.startup_sent.clone(),
                    observed: handle.startup_observed.clone(),
                    run: lease.run.clone(),
                    response,
                })
                .await
                .map_err(|_| stopped_error(handle, "delivery supervisor stopped"))?;
            lease.run.activate();
            result
                .await
                .map_err(|_| stopped_error(handle, "delivery supervisor dropped startup"))??;
        } else {
            lease.run.activate();
        }
        tokio::select! {
            stopped = wait_for_stop(handle) => stopped,
            error = lease.errors.recv() => match error {
                Some(error) => Err(Error::Other(error)),
                None => Err(stopped_error(handle, "delivery supervisor stopped")),
            },
        }
    }

    async fn ensure_started<'a>(&'a self, engine: &Arc<Engine>) -> &'a SupervisorHandle {
        self.handle
            .get_or_init(|| async {
                let (sender, receiver) = mpsc::channel(CAPACITY);
                let external_run = Arc::new(Mutex::new(None));
                assert!(
                    engine.bus.install_delivery_router(
                        self.id,
                        sender.downgrade(),
                        external_run.clone(),
                    ),
                    "an App may install only one delivery supervisor"
                );
                let (stopped_tx, stopped) = watch::channel(None);
                let actor = tokio::spawn(supervise(self.id, Arc::downgrade(engine), receiver));
                tokio::spawn(async move {
                    let reason: Arc<str> = match actor.await {
                        Ok(reason) => reason.into(),
                        Err(error) => format!("delivery supervisor task failed: {error}").into(),
                    };
                    let _ = stopped_tx.send(Some(reason));
                });
                SupervisorHandle {
                    sender,
                    external_run,
                    startup_sent: Arc::new(AtomicBool::new(false)),
                    startup_observed: Arc::new(AtomicBool::new(false)),
                    stopped,
                }
            })
            .await
    }
}

impl RunLease {
    fn acquire(external_run: Arc<Mutex<Option<RunContext>>>) -> Result<Self> {
        let (run, errors) = RunContext::new();
        let mut active = external_run.lock().expect("App::run route poisoned");
        if active.is_some() {
            return Err(Error::Other(
                "App::run already has an active supervisor owner".into(),
            ));
        }
        *active = Some(run.clone());
        drop(active);
        Ok(Self {
            external_run,
            run,
            errors,
        })
    }
}

impl Drop for RunLease {
    fn drop(&mut self) {
        self.run.cancel();
        let mut active = self.external_run.lock().expect("App::run route poisoned");
        if active.as_ref().is_some_and(|active| active.same(&self.run)) {
            active.take();
        }
        drop(active);
    }
}

async fn wait_for_stop(handle: &SupervisorHandle) -> Result<()> {
    let mut stopped = handle.stopped.clone();
    loop {
        if let Some(reason) = stopped.borrow().clone() {
            return Err(Error::Other(reason.to_string()));
        }
        if stopped.changed().await.is_err() {
            return Err(Error::Other("delivery supervisor stopped".into()));
        }
    }
}

fn stopped_error(handle: &SupervisorHandle, fallback: &str) -> Error {
    let message = handle
        .stopped
        .borrow()
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| fallback.to_string());
    Error::Other(message)
}

async fn supervise(
    supervisor: u64,
    engine: Weak<Engine>,
    mut receiver: mpsc::Receiver<DeliveryMessage>,
) -> String {
    while let Some(message) = receiver.recv().await {
        let Some(engine) = engine.upgrade() else {
            return "App engine dropped".into();
        };
        match message {
            DeliveryMessage::Deliver {
                event,
                mut response,
            } => {
                if response.is_closed() {
                    continue;
                }
                let work = process_root(&engine, supervisor, event, Some(MAX_CASCADE));
                tokio::pin!(work);
                let result = tokio::select! {
                    biased;
                    result = &mut work => Some(result),
                    _ = response.closed() => None,
                };
                if let Some(result) = result {
                    let _ = response.send(result);
                }
            }
            DeliveryMessage::Start {
                event,
                completed,
                observed,
                run,
                mut response,
            } => {
                if response.is_closed() {
                    run.cancel();
                    continue;
                }
                if !observed.swap(true, Ordering::AcqRel) {
                    engine.bus.observe(event.clone());
                }
                let work = process_root(&engine, supervisor, event, None);
                tokio::pin!(work);
                let result = tokio::select! {
                    biased;
                    result = &mut work => Some(result.map(|_| ())),
                    _ = response.closed() => None,
                };
                if let Some(result) = result {
                    if result.is_ok() {
                        completed.store(true, Ordering::Release);
                    } else {
                        run.cancel();
                    }
                    let _ = response.send(result);
                } else {
                    run.cancel();
                }
            }
            DeliveryMessage::Event { event, run } => {
                if run.is_cancelled() {
                    continue;
                }
                let work = process_root(&engine, supervisor, event, None);
                tokio::pin!(work);
                tokio::select! {
                    biased;
                    _ = run.cancelled() => {}
                    result = &mut work => {
                        if let Err(error) = result {
                            run.cancel();
                            run.report(format!("delivery supervisor failed: {error}"));
                        }
                    }
                }
            }
        }
    }
    "delivery supervisor queue closed".into()
}

async fn process_root(
    engine: &Engine,
    supervisor: u64,
    initial: Event,
    limit: Option<u32>,
) -> Result<Vec<JourneyRun>> {
    let cascades = Arc::new(Mutex::new(VecDeque::new()));
    let mut results = Vec::new();
    let mut handled = 0_u32;
    let mut next = Some(initial);

    while let Some(event) = next {
        if limit.is_some_and(|limit| handled >= limit) {
            break;
        }
        handled += 1;
        let mut sink = RecordingSink::default();
        let runs = scope_delivery(
            DeliveryOrigin {
                supervisor,
                cascades: cascades.clone(),
            },
            engine.run_triggers(&event.label, &event.payload, &mut sink),
        )
        .await?;
        results.extend(runs);
        next = cascades
            .lock()
            .expect("delivery cascades poisoned")
            .pop_front();
    }
    Ok(results)
}
