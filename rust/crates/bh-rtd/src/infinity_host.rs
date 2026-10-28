//! [`InfinityHost`] — the Phase 2 [`AgentHost`] implementation embedding
//! `infinity-agent-core` 0.2.0 (PLAN.md §4 "runtime wedge").
//!
//! Design notes (all constraints verified against the published 0.2.0
//! sources, not assumed):
//!
//! * **Non-`Send` containment.** `infinity-agent-core`'s local system is
//!   built on `Rc<SystemInner>` and drives its tasks with
//!   `tokio::task::spawn_local`, so every system must live on one OS thread
//!   inside a `LocalSet` on a current-thread runtime (this is PLAN.md §4's
//!   own risk note: "non-`Send` local tasks (LocalSet, single-threaded per
//!   system)"). One [`InfinityHost`] therefore owns one dedicated "runtime
//!   actor" OS thread; the public methods are synchronous and round-trip
//!   commands to that thread over a `tokio` mpsc channel. **The trait
//!   methods must not be called from inside an async runtime** (they block
//!   on the reply); the daemon's dispatch threads are plain `std` threads,
//!   which is the intended caller.
//! * **Bounded pool.** `spawn_thread` ≈ the colony's sub-agent pool
//!   (PLAN.md §4 Phase 2 mapping): the host refuses to exceed
//!   [`InfinityHostConfig::max_threads`] with [`HostError::Capacity`] —
//!   fail-loud, never queued-and-forgotten.
//! * **Clean lifecycle.** `shutdown` drops the command channel; the actor
//!   drains it, then calls the Infinity system's own
//!   `LaunchingSystem::shutdown()` (drivers flush their in-flight turns)
//!   before the OS thread is joined. Post-shutdown calls fail with
//!   [`HostError::Shutdown`].
//! * **No unsafe, no hidden fallbacks.** Startup failures (unknown model,
//!   provider catalog errors) abort `start` synchronously with
//!   [`HostError::Startup`]; events to unknown/detached agents fail with
//!   [`HostError::UnknownAgent`].
//! * **Tools.** Extra tools (e.g. the BH tools bridge from the sibling
//!   `bh-mcp-bridge` crate, assembled by the daemon binary) are registered
//!   system-wide on the Infinity tool surface and are offered to every
//!   member-agent completion round.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use infinity_agent_core::message::InputMessage;
use infinity_agent_core::stores::{InMemoryConversationStore, InMemoryStateStore};
use infinity_agent_core::tools::Tool;
use infinity_agent_core::system::local::{ChannelSender, LaunchingSystem, ThreadHandle};
use infinity_agent_core::system::{AgentEvent, AgentSystemBuilder, NoRapHttp, StaticModel};
use infinity_agent_core::traits::InputSender;
use infinity_agent_core::ThreadId;
use infinity_provider_protocol::ModelProvider;
use tokio::sync::{mpsc, oneshot, watch};

use crate::host::{
    AgentHost, AgentHandle, AgentId, AgentSpec, ColonyEvent, ColonyEventKind, HostError,
};

/// Configuration for [`InfinityHost::start`].
pub struct InfinityHostConfig {
    /// The model provider backing every completion round (Phase 1's
    /// `JeumpaProvider`, or a test double). Kept behind the Infinity trait
    /// so the host stays provider-agnostic.
    pub provider: Arc<dyn ModelProvider>,
    /// Model id resolved from the provider catalog for every thread
    /// (must exist in `provider.list_models()`).
    pub model_id: String,
    /// Upper bound of the sub-agent pool (see [`HostError::Capacity`]).
    pub max_threads: usize,
    /// Extra tools registered system-wide (offered to every completion
    /// round). The daemon binary assembles these from `bh-mcp-bridge`'s
    /// [`Tool`](infinity_agent_core::tools::Tool) implementation when a
    /// tools socket is configured; tests leave this empty.
    pub tools: Vec<DaemonToolBox>,
}

/// A boxed runtime tool as accepted by [`InfinityHostConfig::tools`].
///
/// This is exactly the element type Infinity's own builder API takes for a
/// local system (`Box<dyn Tool<ChannelSender>>`); the alias exists so
/// callers (daemon binary, tests) never have to name the trait internals.
pub type DaemonToolBox = Box<dyn Tool<ChannelSender>>;

impl InfinityHostConfig {
    /// Validate basic invariants up front (fail-loud before any thread
    /// exists).
    pub fn validate(&self) -> Result<(), HostError> {
        if self.max_threads == 0 {
            return Err(HostError::Startup(
                "InfinityHostConfig::max_threads must be >= 1".into(),
            ));
        }
        if self.model_id.trim().is_empty() {
            return Err(HostError::Startup(
                "InfinityHostConfig::model_id must be non-empty".into(),
            ));
        }
        Ok(())
    }
}

/// One member agent's observable state (what `list_threads` reports).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadStatInfo {
    /// Colony-visible name this thread was spawned under.
    pub agent_id: String,
    /// Infinity thread id (UUID string; opaque per the RAP wire contract).
    pub thread_id: String,
    /// Colony role recorded at spawn time.
    pub role: String,
    /// How many Infinity `AgentEvent`s this thread has emitted.
    pub events_seen: u64,
    /// Name of the most recent event (`UserInput`, `TextChunk`,
    /// `CompletionFinished`, `Info`, ...).
    pub last_event: String,
}

/// Successful outcome of one actor command.
#[derive(Debug)]
enum CommandOutcome {
    /// `Spawn`: the Infinity thread id of the new member agent.
    Spawned(String),
    /// `SendEvent` / `Detach`: the Infinity thread id that was addressed.
    ThreadId(String),
    /// `Stats`: snapshot of all live member agents.
    Stats(Vec<ThreadStatInfo>),
}

type CommandReply = Result<CommandOutcome, HostError>;

/// Commands sent from the public (blocking) methods to the runtime actor.
///
/// The host itself never touches Infinity types off the actor thread; reply
/// payloads are plain `Send` data.
enum Command {
    Spawn {
        spec: AgentSpec,
        reply: oneshot::Sender<CommandReply>,
    },
    SendEvent {
        agent: AgentId,
        event: ColonyEvent,
        reply: oneshot::Sender<CommandReply>,
    },
    Detach {
        agent: AgentId,
        reply: oneshot::Sender<CommandReply>,
    },
    Stats {
        reply: oneshot::Sender<CommandReply>,
    },
}

/// The Phase 2 host: real Infinity threads behind the Phase 0 [`AgentHost`]
/// contract.
pub struct InfinityHost {
    cmd_tx: Option<mpsc::UnboundedSender<Command>>,
    actor: Option<std::thread::JoinHandle<()>>,
}

impl core::fmt::Debug for InfinityHost {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InfinityHost")
            .field("alive", &self.cmd_tx.is_some())
            .finish_non_exhaustive()
    }
}

impl InfinityHost {
    /// Start the runtime actor thread and build the Infinity system on it.
    ///
    /// Blocks until the actor reports readiness. Any startup failure
    /// (provider catalog lookup, unknown model id, thread spawn) is
    /// returned here — `start` never yields a half-alive host.
    pub fn start(config: InfinityHostConfig) -> Result<Self, HostError> {
        config.validate()?;
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
        let (ready_tx, ready_rx) = oneshot::channel::<Result<(), String>>();

        let actor = std::thread::Builder::new()
            .name("bh-rtd-actor".into())
            .spawn(move || actor_thread(config, cmd_rx, ready_tx))
            .map_err(|e| HostError::Startup(format!("failed to spawn bh-rtd actor thread: {e}")))?;

        match ready_rx
            .blocking_recv()
            .unwrap_or_else(|_| Err("actor thread died before reporting readiness".into()))
        {
            Ok(()) => Ok(Self {
                cmd_tx: Some(cmd_tx),
                actor: Some(actor),
            }),
            Err(reason) => {
                // The actor already exited; just reap it.
                let _ = actor.join();
                Err(HostError::Startup(reason))
            }
        }
    }

    /// Route one command to the actor and block for its reply, failing loud
    /// after shutdown or if the actor died.
    fn command(
        &self,
        make: impl FnOnce(oneshot::Sender<CommandReply>) -> Command,
    ) -> Result<CommandOutcome, HostError> {
        let tx = self
            .cmd_tx
            .as_ref()
            .ok_or_else(|| HostError::Shutdown("host already shut down".into()))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(make(reply_tx))
            .map_err(|_| HostError::Shutdown("runtime actor channel closed".into()))?;
        reply_rx
            .blocking_recv()
            .unwrap_or_else(|_| {
                Err(HostError::Shutdown(
                    "runtime actor dropped the reply channel".into(),
                ))
            })
    }

    /// Detach from a member agent (daemon `shutdown_thread`): the host stops
    /// tracking it and stops consuming its events; the Infinity thread idles
    /// out and its history remains in the conversation store (hibernation
    /// semantics — PLAN.md §4: "hibernation for scheduled roles").
    pub fn detach_agent(&mut self, agent: &AgentId) -> Result<String, HostError> {
        let agent = agent.clone();
        match self.command(|reply| Command::Detach { agent, reply })? {
            CommandOutcome::ThreadId(thread_id) => Ok(thread_id),
            other => unreachable!("Detach reply invariant violated: {other:?}"),
        }
    }

    /// Snapshot of all live member agents (daemon `list_threads`).
    pub fn thread_stats(&self) -> Result<Vec<ThreadStatInfo>, HostError> {
        match self.command(|reply| Command::Stats { reply })? {
            CommandOutcome::Stats(stats) => Ok(stats),
            other => unreachable!("Stats reply invariant violated: {other:?}"),
        }
    }

    /// Spawn a member agent and return both its colony-visible id (the
    /// accepted name) and the Infinity thread id backing it (what
    /// `list_threads` reports as `thread_id`).
    pub fn spawn_thread_full(&mut self, spec: AgentSpec) -> Result<(AgentId, String), HostError> {
        if spec.name.trim().is_empty() {
            return Err(HostError::Unsupported("agent name must be non-empty"));
        }
        let name = AgentId(spec.name.clone());
        match self.command(|reply| Command::Spawn { spec, reply })? {
            CommandOutcome::Spawned(thread_id) => Ok((name, thread_id)),
            other => unreachable!("Spawn reply invariant violated: {other:?}"),
        }
    }
}

impl AgentHost for InfinityHost {
    fn spawn_agent(&mut self, spec: AgentSpec) -> Result<AgentHandle, HostError> {
        // Phase 0 semantics: the handle id is the colony-visible name (the
        // host-assigned identity later events address); the Infinity thread
        // id is reported alongside via `spawn_thread_full`.
        self.spawn_thread_full(spec)
            .map(|(id, _thread_id)| AgentHandle { id })
    }

    fn send_event(&mut self, agent: &AgentId, event: ColonyEvent) -> Result<(), HostError> {
        let agent = agent.clone();
        match self.command(|reply| Command::SendEvent { agent, event, reply })? {
            CommandOutcome::ThreadId(_) => Ok(()),
            other => unreachable!("SendEvent reply invariant violated: {other:?}"),
        }
    }

    fn shutdown(mut self) -> Result<(), HostError> {
        // Dropping the sender closes the command channel; the actor drains
        // it, winds the Infinity system down, and exits. Join reaps it.
        self.cmd_tx = None;
        match self.actor.take() {
            Some(handle) => handle.join().map_err(|_| {
                HostError::Shutdown("runtime actor panicked during shutdown".into())
            }),
            None => Ok(()), // already shut down; truthful no-op
        }
    }
}

// ── Actor side (runs entirely on the dedicated OS thread) ──

type SharedThreads = Rc<RefCell<HashMap<ThreadId, ThreadEntry>>>;

/// The concrete system type the actor builds (`new_local` pins `H =
/// NoRapHttp`).
type LocalLaunching = LaunchingSystem<InMemoryConversationStore, InMemoryStateStore, NoRapHttp>;

struct ThreadEntry {
    agent_id: String,
    role: String,
    events_seen: u64,
    last_event: String,
    detach: watch::Sender<bool>,
}

fn actor_thread(
    config: InfinityHostConfig,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    ready_tx: oneshot::Sender<Result<(), String>>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("tokio current-thread runtime: {e}")));
            return;
        }
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let InfinityHostConfig { provider, model_id, max_threads, tools } = config;
        let model = match StaticModel::new(provider.clone(), &model_id).await {
            Ok(model) => model,
            Err(e) => {
                let _ = ready_tx.send(Err(format!(
                    "resolving brain model {:?}: {e}",
                    model_id
                )));
                return;
            }
        };
        let launching: LocalLaunching = AgentSystemBuilder::new_local(
                InMemoryConversationStore::new(),
                InMemoryStateStore::new(),
                model,
            )
            .tools(tools)
            .start();
        let threads: SharedThreads = Rc::new(RefCell::new(HashMap::new()));
        let sender = launching.sender();
        let mut event_seq: u64 = 0;

        let _ = ready_tx.send(Ok(()));

        while let Some(cmd) = cmd_rx.recv().await {
            // Command handlers await directly on the actor's main future:
            // while they are pending, the LocalSet drives the Infinity
            // router/driver tasks they may depend on.
            match cmd {
                Command::Spawn { spec, reply } => {
                    let outcome = spawn_one(&launching, &threads, spec, max_threads).await;
                    let _ = reply.send(outcome.map(CommandOutcome::Spawned));
                }
                Command::SendEvent { agent, event, reply } => {
                    event_seq += 1;
                    let outcome =
                        deliver_event(&threads, &sender, &agent, event, event_seq).await;
                    let _ = reply.send(outcome.map(CommandOutcome::ThreadId));
                }
                Command::Detach { agent, reply } => {
                    let _ = reply.send(detach_one(&threads, &agent).map(CommandOutcome::ThreadId));
                }
                Command::Stats { reply } => {
                    let _ = reply.send(Ok(CommandOutcome::Stats(collect_stats(&threads))));
                }
            }
        }
        // Command channel closed → `AgentHost::shutdown`. Wind down cleanly:
        // every driver flushes its in-flight turn before this resolves.
        launching.shutdown().await;
    });
}

async fn spawn_one(
    launching: &LocalLaunching,
    threads: &SharedThreads,
    spec: AgentSpec,
    max_threads: usize,
) -> Result<String, HostError> {
    let agent_id = AgentId(spec.name.clone());
    {
        let borrowed = threads.borrow();
        if borrowed.values().any(|e| e.agent_id == agent_id.0) {
            return Err(HostError::DuplicateAgent(agent_id.0));
        }
        if borrowed.len() >= max_threads {
            return Err(HostError::Capacity { limit: max_threads });
        }
    }

    let mut builder = launching.thread_builder();
    if !spec.instructions.is_empty() {
        builder = builder.extra_system_prompt(spec.instructions);
    }
    // `launch` awaits the subscribe handshake; it must be awaited from the
    // actor's main future so the LocalSet can run the router task.
    let mut handle: ThreadHandle = builder.launch().await;

    let thread_id = handle.thread_id().to_owned();
    let (detach_tx, detach_rx) = watch::channel(false);
    threads.borrow_mut().insert(
        thread_id.clone(),
        ThreadEntry {
            agent_id: agent_id.0.clone(),
            role: spec.role,
            events_seen: 0,
            last_event: "none".into(),
            detach: detach_tx,
        },
    );

    // Per-thread drain task: consumes the ThreadHandle's event stream and
    // keeps the entry's stats current. Ends on system shutdown (`None`) or
    // explicit detach (watch flag / sender drop).
    let stats = Rc::clone(threads);
    let drain_key = thread_id.clone();
    tokio::task::spawn_local(async move {
        let mut detach_rx = detach_rx;
        loop {
            tokio::select! {
                ev = handle.recv() => match ev {
                    Some(event) => record_event(&stats, &drain_key, &event),
                    None => break,
                },
                changed = detach_rx.changed() => {
                    if changed.is_err() || *detach_rx.borrow() {
                        break;
                    }
                }
            }
        }
    });

    Ok(thread_id.to_string())
}

async fn deliver_event(
    threads: &SharedThreads,
    sender: &ChannelSender,
    agent: &AgentId,
    event: ColonyEvent,
    event_seq: u64,
) -> Result<String, HostError> {
    let thread_id = {
        let borrowed = threads.borrow();
        borrowed
            .iter()
            .find(|(_, e)| e.agent_id == agent.0)
            .map(|(tid, _)| tid.clone())
            .ok_or_else(|| HostError::UnknownAgent(agent.0.clone()))?
    };
    let message = InputMessage::user_text(thread_id.clone(), render_event_text(&event));
    sender
        .send_to_input_queue(message, &format!("bh-evt-{event_seq}"))
        .await
        .map_err(|_| {
            HostError::Shutdown("infinity input channel closed (system shutting down)".into())
        })?;
    Ok(thread_id.to_string())
}

fn detach_one(threads: &SharedThreads, agent: &AgentId) -> Result<String, HostError> {
    let mut borrowed = threads.borrow_mut();
    let found = borrowed
        .iter()
        .find(|(_, e)| e.agent_id == agent.0)
        .map(|(tid, _)| tid.clone());
    match found {
        Some(tid) => {
            let entry = borrowed.remove(&tid).expect("entry just found");
            // Signal the drain task to detach (drop its subscription).
            let _ = entry.detach.send(true);
            Ok(tid.to_string())
        }
        None => Err(HostError::UnknownAgent(agent.0.clone())),
    }
}

fn collect_stats(threads: &SharedThreads) -> Vec<ThreadStatInfo> {
    let mut stats: Vec<ThreadStatInfo> = threads
        .borrow()
        .iter()
        .map(|(thread_id, e)| ThreadStatInfo {
            agent_id: e.agent_id.clone(),
            thread_id: thread_id.to_string(),
            role: e.role.clone(),
            events_seen: e.events_seen,
            last_event: e.last_event.clone(),
        })
        .collect();
    stats.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    stats
}

fn record_event(threads: &SharedThreads, thread_id: &ThreadId<str>, event: &AgentEvent) {
    if let Some(entry) = threads.borrow_mut().get_mut(thread_id) {
        entry.events_seen += 1;
        entry.last_event = event_name(event).to_owned();
    }
    // No entry => the agent was detached after the event was emitted; the
    // event is intentionally dropped (detach stops the event flow by
    // contract, and this race is documented on `detach_agent`).
}

fn event_name(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::UserInput { .. } => "UserInput",
        AgentEvent::ToolResult { .. } => "ToolResult",
        AgentEvent::SubscriptionEvent { .. } => "SubscriptionEvent",
        AgentEvent::OAuthRequired { .. } => "OAuthRequired",
        AgentEvent::CompactionApplied => "CompactionApplied",
        AgentEvent::CompletionStarted => "CompletionStarted",
        AgentEvent::TextChunk { .. } => "TextChunk",
        AgentEvent::ThinkingStarted => "ThinkingStarted",
        AgentEvent::ThinkingChunk { .. } => "ThinkingChunk",
        AgentEvent::ThinkingEnded => "ThinkingEnded",
        AgentEvent::ToolCall { .. } => "ToolCall",
        AgentEvent::CompletionFinished { .. } => "CompletionFinished",
        AgentEvent::UserChoiceRequired { .. } => "UserChoiceRequired",
        AgentEvent::UserChoiceDismissed { .. } => "UserChoiceDismissed",
        AgentEvent::Info { .. } => "Info",
    }
}

/// Colony event → Infinity user-text input (the Phase 2 seam mapping).
///
/// `Custom` events carry their payload verbatim; the coarse colony
/// categories get a bracketed tag so the thread history shows what kind of
/// colony event entered the conversation.
fn render_event_text(event: &ColonyEvent) -> String {
    let prefix = match event.kind {
        ColonyEventKind::TaskAssigned => "[task_assigned] ",
        ColonyEventKind::MessageReceived => "[message_received] ",
        ColonyEventKind::HealthTick => "[health_tick] ",
        ColonyEventKind::Custom => "",
    };
    format!("{}{}", prefix, event.payload_json)
}
