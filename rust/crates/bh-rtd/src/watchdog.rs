//! The Phase 5 **watchdog/heartbeat** (PLAN.md §4 Phase 5, milestone M1).
//!
//! The colony's member agents run as Infinity threads inside the daemon's
//! [`crate::infinity_host::InfinityHost`]. The watchdog answers one question
//! honestly for every tracked thread: *is it alive, parked, or stalled?*
//!
//! ## Where the heartbeat comes from
//!
//! A thread "beats" whenever it emits an [`AgentEvent`](infinity_agent_core::
//! system::AgentEvent) — text chunks, tool calls, completion rounds are all
//! signs of a turn in flight. The host records the instant of the last event
//! per thread (`ThreadStatInfo::last_event_age_ms`); the watchdog classifies
//! from that age. There is no separate heartbeat protocol and none is
//! faked: an agent that emits nothing is indistinguishable from an agent
//! that stopped, so the stall threshold does the distinguishing.
//!
//! ## `sleep_until_event_or_input` (the PLAN §4 phrase, honored literally)
//!
//! `infinity-agent-core` 0.2.0 ships a built-in **passive** tool
//! `sleep_until_event_or_input` (`SleepUntilEventOrInputTool`): invoking it
//! ends the agent's turn and the thread stays parked until new input
//! arrives — the runtime's own idiom for "hibernate until woken". A parked
//! thread emits no events by design, so a naive stall detector would flag
//! every hibernating agent. The watchdog therefore treats "the last event
//! was the `sleep_until_event_or_input` tool call" as the dedicated
//! [`WatchState::Parked`] state and **exempts it from stall detection**
//! (PLAN.md §4 Phase 5: watchdog/heartbeat via `sleep_until_event_or_input`
//! and subscriptions). The flag is a recorded fact, never a guess: it tracks
//! the *most recent tool call* and is therefore sticky across non-tool
//! events (the `CompletionFinished` that ends the turn does not un-park a
//! thread whose last tool call was the sleep tool), while any other tool
//! call clears it. Verified against the published 0.2.0 sources: the sleep
//! tool executes as a no-op (`execute` returns `Ok(())`), the driver's
//! idle/respawn cycle (`system::local::driver`) then keeps the thread
//! resident, and `is_passive()` governs interruptibility of pending calls —
//! not parking. The host observes the tool call itself, which is the only
//! signal the runtime actually exposes.
//!
//! ## Evaluation model: pull, not poll
//!
//! Classification is a **pure function** evaluated whenever the daemon
//! serves `health` / `list_threads` — no background sweeper thread, no
//! watchdog state that can itself go stale between polls. This mirrors the
//! `sleep_until_event_or_input` semantics at the host level: sleep until
//! the next event *or* the deadline, and only the observer decides what the
//! deadline means. Event subscriptions (Infinity `StateStore`'s active
//! subscription table) remain a Phase 5 follow-up: this milestone observes
//! liveness; it does not yet wake parked agents on subscribed events — that
//! gap is documented in `rust/README.md`, not hidden.
//!
//! Edge semantics (all covered by tests):
//! * age exactly equal to the threshold is still [`WatchState::Active`]
//!   (strictly greater than the threshold stalls);
//! * [`WatchState::Parked`] takes precedence over [`WatchState::Stalled`]
//!   no matter how long the thread has been silent — parking is by design;
//! * detached threads leave the host registry entirely, so the watchdog
//!   never reports them (shutdown is not a stall).

/// Default stall threshold: a tracked thread that emitted nothing for
/// 30 seconds and is not parked is reported [`WatchState::Stalled`].
pub const DEFAULT_STALL_MS: u64 = 30_000;

/// Liveness classification of one member agent (Phase 5 M1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WatchState {
    /// Events are flowing (or the thread is younger than the stall
    /// threshold): nothing to report.
    Active,
    /// The thread's last action was invoking the passive
    /// `sleep_until_event_or_input` tool — it waits for input by design and
    /// is exempt from stall detection.
    Parked,
    /// Not parked, and no event observed for longer than the stall
    /// threshold: the thread looks stuck mid-turn and is named in
    /// [`WatchdogReport::stalled_agents`].
    Stalled,
}

impl WatchState {
    /// The wire form (`list_threads`' `watch_state` field).
    pub fn as_str(&self) -> &'static str {
        match self {
            WatchState::Active => "active",
            WatchState::Parked => "parked",
            WatchState::Stalled => "stalled",
        }
    }
}

/// Classify one thread's liveness.
///
/// * `last_event_age_ms` — ms since the thread's last event (spawn time
///   counts as the floor: a thread that never emitted is aged from spawn).
/// * `parked` — the host observed the thread's last event to be the
///   `sleep_until_event_or_input` tool call.
/// * `stall_threshold_ms` — the configured stall threshold.
///
/// Boundary: `age == threshold` is still [`WatchState::Active`]; only
/// `age > threshold` stalls. Parking wins over stalling regardless of age.
pub fn classify(last_event_age_ms: u64, parked: bool, stall_threshold_ms: u64) -> WatchState {
    if parked {
        WatchState::Parked
    } else if last_event_age_ms > stall_threshold_ms {
        WatchState::Stalled
    } else {
        WatchState::Active
    }
}

/// Aggregate watchdog verdict for one daemon snapshot (what `health`
/// reports under `watchdog`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchdogReport {
    /// The threshold the classification used (reported so consumers can
    /// interpret the counts without reading the daemon's env).
    pub stall_threshold_ms: u64,
    /// Threads classified [`WatchState::Active`].
    pub active: usize,
    /// Threads classified [`WatchState::Parked`].
    pub parked: usize,
    /// Threads classified [`WatchState::Stalled`].
    pub stalled: usize,
    /// Colony names of the stalled threads, sorted (stable, testable).
    pub stalled_agents: Vec<String>,
}

impl Default for WatchdogReport {
    fn default() -> Self {
        WatchdogReport {
            stall_threshold_ms: DEFAULT_STALL_MS,
            active: 0,
            parked: 0,
            stalled: 0,
            stalled_agents: Vec::new(),
        }
    }
}

impl WatchdogReport {
    /// The wire form (the `health` result's `watchdog` object).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "stall_threshold_ms": self.stall_threshold_ms,
            "active": self.active,
            "parked": self.parked,
            "stalled": self.stalled,
            "stalled_agents": self.stalled_agents,
        })
    }
}

/// Survey one snapshot of thread facts: `(agent_id, last_event_age_ms,
/// parked)` per tracked thread. Pure — no clock, no locks — so the daemon
/// evaluates it per request from the host's [`crate::infinity_host::
/// ThreadStatInfo`] snapshot.
pub fn survey<'a, I>(facts: I, stall_threshold_ms: u64) -> WatchdogReport
where
    I: IntoIterator<Item = (&'a str, u64, bool)>,
{
    let mut report = WatchdogReport {
        stall_threshold_ms,
        ..WatchdogReport::default()
    };
    for (agent_id, age_ms, parked) in facts {
        match classify(age_ms, parked, stall_threshold_ms) {
            WatchState::Active => report.active += 1,
            WatchState::Parked => report.parked += 1,
            WatchState::Stalled => {
                report.stalled += 1;
                report.stalled_agents.push(agent_id.to_owned());
            }
        }
    }
    report.stalled_agents.sort();
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_thread_is_active() {
        assert_eq!(classify(0, false, 30_000), WatchState::Active);
    }

    #[test]
    fn age_at_threshold_is_active_strictly_greater_stalls() {
        assert_eq!(classify(30_000, false, 30_000), WatchState::Active);
        assert_eq!(classify(30_001, false, 30_000), WatchState::Stalled);
    }

    #[test]
    fn parked_exempts_from_stall_regardless_of_age() {
        assert_eq!(classify(0, true, 30_000), WatchState::Parked);
        assert_eq!(classify(30_001, true, 30_000), WatchState::Parked);
        assert_eq!(classify(u64::MAX, true, 30_000), WatchState::Parked);
    }

    #[test]
    fn zero_threshold_stalls_everything_unparked() {
        assert_eq!(classify(0, false, 0), WatchState::Active);
        assert_eq!(classify(1, false, 0), WatchState::Stalled);
    }

    #[test]
    fn watch_state_wire_names() {
        assert_eq!(WatchState::Active.as_str(), "active");
        assert_eq!(WatchState::Parked.as_str(), "parked");
        assert_eq!(WatchState::Stalled.as_str(), "stalled");
    }

    #[test]
    fn survey_counts_and_sorts_stalled_agents() {
        let report = survey(
            vec![
                ("worker-b", 40_000, false), // stalled
                ("parked-1", 90_000, true),  // parked (exempt)
                ("worker-a", 40_000, false), // stalled
                ("active-1", 5_000, false),  // active
            ],
            30_000,
        );
        assert_eq!(report.stall_threshold_ms, 30_000);
        assert_eq!(report.active, 1);
        assert_eq!(report.parked, 1);
        assert_eq!(report.stalled, 2);
        assert_eq!(report.stalled_agents, vec!["worker-a", "worker-b"]);
    }

    #[test]
    fn survey_empty_snapshot_is_all_zeroes() {
        let report: WatchdogReport = survey(Vec::new(), 30_000);
        assert_eq!(report, WatchdogReport::default());
    }

    #[test]
    fn report_json_shape() {
        let report = survey(vec![("s1", 99_000, false), ("p1", 1, true)], 30_000);
        let json = report.to_json();
        assert_eq!(json["stall_threshold_ms"], 30_000);
        assert_eq!(json["active"], 0);
        assert_eq!(json["parked"], 1);
        assert_eq!(json["stalled"], 1);
        assert_eq!(json["stalled_agents"], serde_json::json!(["s1"]));
    }
}
