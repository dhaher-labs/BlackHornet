//! Phase 2 tests for [`InfinityHost`] — the real `infinity-agent-core`
//! runtime wedge behind the Phase 0 [`AgentHost`] contract (PLAN.md §4).
//!
//! Exercises the **real** Infinity local system (real threads, real
//! `LocalSet` actor, real completion rounds) with a scripted in-process
//! model provider — no Python, no network. Gated behind the `infinity`
//! feature.
//!
//! The host's public methods are synchronous by contract and must be called
//! from plain `std` threads (never inside an async runtime), which is
//! exactly what these `#[test]`s do.

#![cfg(feature = "infinity")]

use std::time::{Duration, Instant};

use async_trait::async_trait;
use bh_rtd::host::{AgentHost, AgentId, AgentSpec, ColonyEvent, ColonyEventKind, HostError};
use bh_rtd::infinity_host::{InfinityHost, InfinityHostConfig, SLEEP_UNTIL_EVENT_OR_INPUT};
use infinity_provider_protocol::{
    CompletionError, CompletionRequest, FinalResponse, ModelEntry, ModelProvider, ModelStream,
    StreamChunk, ToolCall,
};
use serde_json::json;

/// Scripted provider: a fixed catalog plus a canned two-chunk completion
/// (`Text` + `Final`) for every round — the minimal honest brain double.
struct EchoProvider {
    catalog: Vec<ModelEntry>,
}

impl EchoProvider {
    fn with_models(ids: &[&str]) -> Self {
        Self {
            catalog: ids
                .iter()
                .map(|id| ModelEntry {
                    model_id: (*id).to_owned(),
                    display_name: format!("test/{id}"),
                    context_window: 8192,
                    max_output_tokens: None,
                    supports_image_input: false,
                })
                .collect(),
        }
    }
}

#[async_trait]
impl ModelProvider for EchoProvider {
    async fn list_models(
        &self,
    ) -> Result<Vec<ModelEntry>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.catalog.clone())
    }

    async fn invoke_model(
        &self,
        _model_id: &str,
        _request: CompletionRequest,
    ) -> Result<ModelStream, CompletionError> {
        let chunks = vec![
            Ok(StreamChunk::Text("echo-reply".into())),
            Ok(StreamChunk::Final(FinalResponse { usage: None })),
        ];
        Ok(Box::pin(futures_util::stream::iter(chunks)))
    }
}

fn host_config(model: &str, max_threads: usize) -> InfinityHostConfig {
    InfinityHostConfig {
        provider: std::sync::Arc::new(EchoProvider::with_models(&["echo-model"])),
        model_id: model.to_owned(),
        max_threads,
        tools: Vec::new(),
    }
}

fn spec(name: &str, role: &str, instructions: &str) -> AgentSpec {
    AgentSpec {
        name: name.to_owned(),
        role: role.to_owned(),
        instructions: instructions.to_owned(),
    }
}

fn event(kind: ColonyEventKind, payload: serde_json::Value) -> ColonyEvent {
    ColonyEvent {
        kind,
        payload_json: payload.to_string(),
    }
}

/// Poll `f` until it returns `Some` (fail with the last observation after
/// `timeout`). The actor drives Infinity tasks on its own thread, so
/// observable stats are eventually consistent.
fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "condition not reached within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn spawn_send_complete_detach_lifecycle_is_clean() {
    let mut host = InfinityHost::start(host_config("echo-model", 4)).expect("host starts");

    // Spawn (trait path): the handle id is the colony name.
    let handle = host
        .spawn_agent(spec("worker-1", "worker", "You are a colony worker."))
        .expect("spawn");
    assert_eq!(handle.id, AgentId("worker-1".into()));

    // Spawn (full path): also reports the backing Infinity thread id.
    let (agent, thread_id) = host
        .spawn_thread_full(spec("worker-2", "analyst", ""))
        .expect("spawn 2");
    assert_eq!(agent, AgentId("worker-2".into()));
    assert!(!thread_id.is_empty(), "Infinity thread id must be reported");
    assert_ne!(thread_id, "worker-2", "thread id is the runtime UUID, not the name");

    // list_threads snapshot (Phase 5 watchdog facts ride along).
    let stats = host.thread_stats().expect("stats");
    assert_eq!(stats.len(), 2);
    let w1 = stats.iter().find(|s| s.agent_id == "worker-1").expect("w1");
    assert_eq!(w1.role, "worker");
    assert_eq!(w1.events_seen, 0);
    // A fresh thread is not parked and its heartbeat age is floored at
    // spawn time (small, never a bogus huge value).
    assert!(!w1.parked, "fresh thread must not be parked");
    assert!(
        w1.last_event_age_ms < 15_000,
        "heartbeat age must be small right after spawn, got {}",
        w1.last_event_age_ms
    );

    // send_event reaches the thread: UserInput, then the real completion
    // round against the provider (TextChunk + CompletionFinished).
    host.send_event(
        &AgentId("worker-1".into()),
        event(ColonyEventKind::TaskAssigned, json!({"task": "do a thing"})),
    )
    .expect("send");
    let seen = wait_for(Duration::from_secs(15), || {
        host.thread_stats()
            .ok()?
            .into_iter()
            .find(|s| s.agent_id == "worker-1")
            .filter(|s| s.events_seen >= 3)
    });
    assert!(seen.last_event.contains("CompletionFinished"));
    assert_eq!(seen.role, "worker");
    // Activity refreshes the heartbeat: age stays small after the round.
    assert!(
        seen.last_event_age_ms < 15_000,
        "heartbeat age must be refreshed by events, got {}",
        seen.last_event_age_ms
    );
    assert!(!seen.parked, "a thread mid-work is not parked");

    // Detach (shutdown_thread semantics): stats drop the agent, later
    // events fail loud with UnknownAgent.
    let detached_thread = host
        .detach_agent(&AgentId("worker-1".into()))
        .expect("detach");
    assert!(!detached_thread.is_empty());
    assert_ne!(detached_thread, thread_id, "each agent has its own runtime thread");
    assert_eq!(host.thread_stats().expect("stats").len(), 1);
    let err = host
        .send_event(&AgentId("worker-1".into()), event(ColonyEventKind::Custom, json!({})))
        .unwrap_err();
    assert!(matches!(err, HostError::UnknownAgent(_)), "got: {err:?}");

    // Clean shutdown: the actor drains and the Infinity system winds down.
    host.shutdown().expect("shutdown");
}

/// Scripted provider for the park test. Round plan (verified against the
/// published 0.2.0 sources AND observed through the real runtime: after a
/// passive `sleep_until_event_or_input` tool call the loop stops — the
/// turn consumes exactly ONE provider round, the driver idles, and the
/// next input respawns it):
///
/// * round 0 (turn 1) — `sleep_until_event_or_input` tool call: the
///   runtime's "hibernate until input" idiom. The watchdog must classify
///   the thread as parked.
/// * (test wakes the thread with a user text)
/// * round 1 (turn 2) — plain text: the thread works, but no tool call
///   happens, so the park flag STAYS (it is a fact about the most recent
///   tool call, sticky across non-tool events).
/// * (test wakes the thread again)
/// * round 2 (turn 3) — a NON-sleep tool call (`cancel_subscription`; an
///   unknown id is fine: the tool call event itself is what clears the
///   park flag). The runtime runs its follow-up round.
/// * round 3+ — plain text, so every turn terminates.
struct ParkProvider {
    round: std::sync::atomic::AtomicUsize,
}

impl ParkProvider {
    fn new() -> Self {
        ParkProvider {
            round: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ModelProvider for ParkProvider {
    async fn list_models(
        &self,
    ) -> Result<Vec<ModelEntry>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(vec![ModelEntry {
            model_id: "park-model".to_owned(),
            display_name: "test/park-model".to_owned(),
            context_window: 8192,
            max_output_tokens: None,
            supports_image_input: false,
        }])
    }

    async fn invoke_model(
        &self,
        _model_id: &str,
        _request: CompletionRequest,
    ) -> Result<ModelStream, CompletionError> {
        let round = self.round.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let chunks = match round {
            0 => vec![
                Ok(StreamChunk::ToolCall(ToolCall::new(
                    "park-call-1",
                    SLEEP_UNTIL_EVENT_OR_INPUT,
                    json!({}),
                ))),
                Ok(StreamChunk::Final(FinalResponse { usage: None })),
            ],
            2 => vec![
                // A non-sleep tool call clears the park flag (the tool
                // result for the unknown id is a business error inside the
                // runtime — irrelevant to the watchdog, which reads the
                // tool call event).
                Ok(StreamChunk::ToolCall(ToolCall::new(
                    "cancel-call-1",
                    "cancel_subscription",
                    json!({"tool_call_id": "no-such-subscription"}),
                ))),
                Ok(StreamChunk::Final(FinalResponse { usage: None })),
            ],
            _ => vec![
                Ok(StreamChunk::Text("settling".into())),
                Ok(StreamChunk::Final(FinalResponse { usage: None })),
            ],
        };
        Ok(Box::pin(futures_util::stream::iter(chunks)))
    }
}

#[test]
fn sleep_tool_call_parks_thread_for_watchdog() {
    let config = InfinityHostConfig {
        provider: std::sync::Arc::new(ParkProvider::new()),
        model_id: "park-model".to_owned(),
        max_threads: 2,
        tools: Vec::new(),
    };
    let mut host = InfinityHost::start(config).expect("host starts");
    host.spawn_agent(spec("hibernator", "scheduled", "Sleep until woken."))
        .expect("spawn");

    host.send_event(
        &AgentId("hibernator".into()),
        event(ColonyEventKind::TaskAssigned, json!({"task": "wait"})),
    )
    .expect("send");

    // Turn 1 settles into the parked state: the thread's most recent tool
    // call was the sleep tool and the turn has ended (the driver idles;
    // the thread waits for input by design).
    let parked = wait_for(Duration::from_secs(15), || {
        host.thread_stats()
            .ok()?
            .into_iter()
            .find(|s| s.agent_id == "hibernator")
            .filter(|s| s.parked && s.last_event == "CompletionFinished")
    });
    assert!(
        parked.events_seen >= 2,
        "the sleep tool call and its turn-end must have been observed"
    );

    // Wake #1: the thread works (plain-text round) but makes NO tool call,
    // so the park flag STAYS — a fact about the most recent tool call,
    // never fabricated from unrelated events.
    host.send_event(
        &AgentId("hibernator".into()),
        event(ColonyEventKind::MessageReceived, json!({"msg": "wake up"})),
    )
    .expect("wake 1");
    let worked = wait_for(Duration::from_secs(15), || {
        host.thread_stats()
            .ok()?
            .into_iter()
            .find(|s| s.agent_id == "hibernator")
            .filter(|s| s.parked && s.last_event == "CompletionFinished" && s.events_seen > parked.events_seen)
    });
    assert!(worked.parked, "no new tool call happened: flag stays");

    // Wake #2: the model now makes a NON-sleep tool call, which clears the
    // park flag; the follow-up round settles the turn.
    host.send_event(
        &AgentId("hibernator".into()),
        event(ColonyEventKind::TaskAssigned, json!({"task": "do real work"})),
    )
    .expect("wake 2");
    let awake = wait_for(Duration::from_secs(15), || {
        host.thread_stats()
            .ok()?
            .into_iter()
            .find(|s| s.agent_id == "hibernator")
            .filter(|s| !s.parked && s.last_event == "CompletionFinished" && s.events_seen > worked.events_seen)
    });
    assert!(!awake.parked, "the non-sleep tool call cleared the flag");

    host.shutdown().expect("shutdown");
}

#[test]
fn duplicate_agent_name_fails_loud() {
    let mut host = InfinityHost::start(host_config("echo-model", 4)).expect("host starts");
    host.spawn_agent(spec("dupe", "worker", "")).expect("first");
    let err = host
        .spawn_agent(spec("dupe", "worker", ""))
        .unwrap_err();
    assert!(matches!(err, HostError::DuplicateAgent(_)), "got: {err:?}");
    assert_eq!(host.thread_stats().expect("stats").len(), 1);
    host.shutdown().expect("shutdown");
}

#[test]
fn bounded_pool_refuses_over_capacity() {
    let mut host = InfinityHost::start(host_config("echo-model", 1)).expect("host starts");
    host.spawn_agent(spec("only", "worker", "")).expect("first");
    let err = host.spawn_agent(spec("extra", "worker", "")).unwrap_err();
    assert!(matches!(err, HostError::Capacity { limit: 1 }), "got: {err:?}");
    assert_eq!(host.thread_stats().expect("stats").len(), 1);
    host.shutdown().expect("shutdown");
}

#[test]
fn send_event_to_unknown_agent_fails_loud() {
    let mut host = InfinityHost::start(host_config("echo-model", 4)).expect("host starts");
    let err = host
        .send_event(&AgentId("ghost".into()), event(ColonyEventKind::Custom, json!({})))
        .unwrap_err();
    assert!(matches!(err, HostError::UnknownAgent(id) if id == "ghost"));
    host.shutdown().expect("shutdown");
}

#[test]
fn start_fails_loud_on_bad_configuration() {
    // Empty model id: caught by config validation before any thread exists.
    let err = InfinityHost::start(host_config("  ", 4)).unwrap_err();
    assert!(matches!(err, HostError::Startup(_)), "got: {err:?}");

    // Zero-capacity pool.
    let err = InfinityHost::start(host_config("echo-model", 0)).unwrap_err();
    assert!(matches!(err, HostError::Startup(_)), "got: {err:?}");

    // Unknown model id: the provider catalog lookup fails at startup.
    let err = InfinityHost::start(host_config("missing-model", 4)).unwrap_err();
    assert!(matches!(err, HostError::Startup(_)), "got: {err:?}");
    assert!(
        err.to_string().contains("missing-model"),
        "startup error must name the offending model: {err}"
    );
}

#[test]
fn empty_agent_name_is_rejected() {
    let mut host = InfinityHost::start(host_config("echo-model", 4)).expect("host starts");
    let err = host.spawn_agent(spec("   ", "worker", "")).unwrap_err();
    assert!(matches!(err, HostError::Unsupported(_)), "got: {err:?}");
    host.shutdown().expect("shutdown");
}
