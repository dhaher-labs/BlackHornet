//! Phase 5 M1 tests for the [`bh_rtd::watchdog`] module — the
//! watchdog/heartbeat classification over the public API surface.
//!
//! The pure logic (`classify` / `survey` / report JSON) is exercised here
//! end-to-end through the public module API; the daemon-level wiring is
//! covered by `tests/daemon.rs` (the `health` / `list_threads` additive
//! fields) and the host-level park detection by
//! `tests/infinity_host.rs::sleep_tool_call_parks_thread_for_watchdog`.

#![cfg(feature = "bridge")]

use bh_rtd::watchdog::{classify, survey, WatchState, WatchdogReport, DEFAULT_STALL_MS};

#[test]
fn default_threshold_is_thirty_seconds() {
    assert_eq!(DEFAULT_STALL_MS, 30_000);
}

#[test]
fn classification_boundaries() {
    // Fresh / young threads are active.
    assert_eq!(classify(0, false, DEFAULT_STALL_MS), WatchState::Active);
    assert_eq!(classify(1, false, DEFAULT_STALL_MS), WatchState::Active);
    assert_eq!(
        classify(DEFAULT_STALL_MS, false, DEFAULT_STALL_MS),
        WatchState::Active,
        "age exactly at the threshold is still active (strictly greater stalls)"
    );
    // Silent beyond the threshold and not parked: stalled.
    assert_eq!(
        classify(DEFAULT_STALL_MS + 1, false, DEFAULT_STALL_MS),
        WatchState::Stalled
    );
    assert_eq!(classify(u64::MAX, false, DEFAULT_STALL_MS), WatchState::Stalled);
}

#[test]
fn parked_wins_over_stalled_and_active() {
    // The sleep_until_event_or_input idiom: a parked thread is waiting for
    // input BY DESIGN, no matter how long it has been silent.
    assert_eq!(classify(0, true, DEFAULT_STALL_MS), WatchState::Parked);
    assert_eq!(
        classify(DEFAULT_STALL_MS + 1, true, DEFAULT_STALL_MS),
        WatchState::Parked
    );
    assert_eq!(classify(u64::MAX, true, DEFAULT_STALL_MS), WatchState::Parked);
}

#[test]
fn wire_names_are_stable() {
    assert_eq!(WatchState::Active.as_str(), "active");
    assert_eq!(WatchState::Parked.as_str(), "parked");
    assert_eq!(WatchState::Stalled.as_str(), "stalled");
}

#[test]
fn survey_aggregates_a_mixed_snapshot() {
    let report = survey(
        vec![
            ("alpha", 1_000, false),      // active
            ("beta", DEFAULT_STALL_MS, false), // at threshold: active
            ("gamma", DEFAULT_STALL_MS + 1, false), // stalled
            ("hibernator", u64::MAX, true), // parked (exempt)
            ("delta", u64::MAX, false),   // stalled
        ],
        DEFAULT_STALL_MS,
    );
    assert_eq!(report.stall_threshold_ms, DEFAULT_STALL_MS);
    assert_eq!(report.active, 2);
    assert_eq!(report.parked, 1);
    assert_eq!(report.stalled, 2);
    // Sorted for stable reporting.
    assert_eq!(report.stalled_agents, vec!["delta".to_owned(), "gamma".to_owned()]);
}

#[test]
fn survey_empty_snapshot_is_a_clean_report() {
    let report: WatchdogReport = survey(Vec::new(), DEFAULT_STALL_MS);
    assert_eq!(report.active, 0);
    assert_eq!(report.parked, 0);
    assert_eq!(report.stalled, 0);
    assert!(report.stalled_agents.is_empty());
}

#[test]
fn report_json_is_wire_shaped() {
    let report = survey(
        vec![("worker-9", DEFAULT_STALL_MS + 5, false), ("sleeper", 2, true)],
        DEFAULT_STALL_MS,
    );
    let json = report.to_json();
    assert_eq!(json["stall_threshold_ms"], DEFAULT_STALL_MS);
    assert_eq!(json["active"], 0);
    assert_eq!(json["parked"], 1);
    assert_eq!(json["stalled"], 1);
    assert_eq!(json["stalled_agents"], serde_json::json!(["worker-9"]));
}
