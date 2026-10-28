//! Phase 2 tests for the durable timer wheel ([`bh_rtd::timers`]).
//!
//! Hermetic: the clock is a frozen injectable double (`WallClock`), the
//! store is the real `JsonTimerStore` on a temp dir. Covers the three
//! durability guarantees the daemon relies on:
//!
//! 1. every mutation persists *before* the call returns,
//! 2. a restarted wheel reloads pending timers from the journal,
//! 3. a corrupt journal fails loud instead of silently starting empty.

#![cfg(feature = "bridge")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bh_rtd::timers::{
    JsonTimerStore, TimerError, TimerId, TimerRecord, TimerStatus, TimerStore, TimerWheel,
    WallClock,
};
use serde_json::{json, Value};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bh-rtd-timers-test-{}-{}-{}",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Frozen manual clock (advance explicitly — fully deterministic).
#[derive(Clone)]
struct FrozenClock(Arc<AtomicU64>);

impl FrozenClock {
    fn new(start_ms: u64) -> Self {
        Self(Arc::new(AtomicU64::new(start_ms)))
    }
    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

impl WallClock for FrozenClock {
    fn now_ms(&self) -> u64 {
        self.now()
    }
}

fn record_json(path: &std::path::Path) -> Value {
    let raw = std::fs::read_to_string(path).expect("journal file exists");
    serde_json::from_str(&raw).expect("journal is valid JSON")
}

// ── schedule / due / cancel / deliver lifecycle ──

#[test]
fn schedule_persists_before_returning() {
    let dir = temp_dir("persist");
    let path = dir.join("timers.json");
    let clock = FrozenClock::new(1_000);
    let wheel = TimerWheel::with_clock(Box::new(JsonTimerStore::new(&path)), clock.clone())
        .expect("fresh wheel");

    let id = wheel
        .schedule(500, Some("w1".into()), json!({"n": 1}))
        .expect("schedule");
    // Journal already on disk the moment `schedule` returned.
    let doc = record_json(&path);
    let timers = doc.get("timers").and_then(Value::as_array).expect("array");
    assert_eq!(timers.len(), 1);
    assert_eq!(timers[0].get("timer_id").and_then(Value::as_str), Some(id.0.as_str()));
    assert_eq!(timers[0].get("status").and_then(Value::as_str), Some("pending"));
    assert_eq!(timers[0].get("fire_at_ms").and_then(Value::as_u64), Some(1_500));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn due_fires_level_triggered_exactly_once() {
    let clock = FrozenClock::new(2_000);
    let wheel = TimerWheel::with_clock(
        Box::new(JsonTimerStore::new(temp_dir("due").join("timers.json"))),
        clock.clone(),
    )
    .expect("fresh wheel");
    wheel.schedule(100, None, json!("x")).expect("schedule");

    clock.advance(99);
    assert!(wheel.due().expect("due poll").is_empty(), "not yet due");

    clock.advance(1);
    let fired = wheel.due().expect("due poll");
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].status, TimerStatus::Fired);

    // Level-triggered: no re-fires on later polls.
    clock.advance(10_000);
    assert!(wheel.due().expect("second poll").is_empty());
    assert_eq!(wheel.pending_count().expect("count"), 0);
}

#[test]
fn cancel_is_idempotent_for_fired_timers() {
    let clock = FrozenClock::new(5_000);
    let wheel = TimerWheel::with_clock(
        Box::new(JsonTimerStore::new(temp_dir("cancel").join("timers.json"))),
        clock.clone(),
    )
    .expect("fresh wheel");
    let id = wheel.schedule(50, None, Value::Null).expect("schedule");

    assert!(wheel.cancel(&id).expect("cancel pending"));
    assert!(
        !wheel.cancel(&id).expect("second cancel"),
        "already cancelled is reported truthfully"
    );
    assert_eq!(wheel.pending_count().expect("count"), 0);

    // Unknown ids are a hard error, not a fake success.
    assert!(matches!(
        wheel.cancel(&TimerId("nope".into())),
        Err(TimerError::UnknownTimer(_))
    ));
}

#[test]
fn mark_delivered_journals_delivery() {
    let dir = temp_dir("delivered");
    let path = dir.join("timers.json");
    let clock = FrozenClock::new(10_000);
    let wheel = TimerWheel::with_clock(Box::new(JsonTimerStore::new(&path)), clock.clone())
        .expect("fresh wheel");
    let id = wheel.schedule(0, Some("w1".into()), Value::Null).expect("schedule");
    let fired = wheel.due().expect("due");
    assert_eq!(fired.len(), 1);

    wheel.mark_delivered(&id).expect("mark delivered");
    let doc = record_json(&path);
    let timers = doc.get("timers").and_then(Value::as_array).unwrap();
    assert_eq!(timers[0].get("status").and_then(Value::as_str), Some("delivered"));
    assert_eq!(timers[0].get("agent_id").and_then(Value::as_str), Some("w1"));

    let _ = std::fs::remove_dir_all(&dir);
}

// ── restart / persistence ──

#[test]
fn pending_timers_survive_a_wheel_restart() {
    let dir = temp_dir("restart");
    let path = dir.join("timers.json");
    let clock = FrozenClock::new(100);

    let wheel = TimerWheel::with_clock(Box::new(JsonTimerStore::new(&path)), clock.clone())
        .expect("wheel 1");
    let id = wheel
        .schedule(60_000, Some("w1".into()), json!({"kind": "wake"}))
        .expect("schedule");
    drop(wheel);

    // "Restart": a fresh wheel over the same store, same clock.
    let wheel2 =
        TimerWheel::with_clock(Box::new(JsonTimerStore::new(&path)), clock.clone())
            .expect("wheel 2");
    assert_eq!(wheel2.pending_count().expect("count"), 1);
    let snap = wheel2.snapshot().expect("snapshot");
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].id, id);
    assert_eq!(snap[0].status, TimerStatus::Pending);
    assert_eq!(snap[0].fire_at_ms, 60_100);

    // A fired-then-delivered timer also reloads with its terminal status.
    clock.advance(61_000);
    wheel2.due().expect("due");
    wheel2.mark_delivered(&id).expect("delivered");
    drop(wheel2);
    let wheel3 =
        TimerWheel::with_clock(Box::new(JsonTimerStore::new(&path)), clock).expect("wheel 3");
    let snap = wheel3.snapshot().expect("snapshot");
    assert_eq!(snap[0].status, TimerStatus::Delivered);
    assert!(wheel3.due().expect("no refire").is_empty(), "delivered timers never re-fire");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overdue_timers_fire_on_first_poll_after_restart() {
    let dir = temp_dir("overdue");
    let path = dir.join("timers.json");
    let clock = FrozenClock::new(1_000);

    let wheel = TimerWheel::with_clock(Box::new(JsonTimerStore::new(&path)), clock.clone())
        .expect("wheel 1");
    wheel.schedule(100, None, Value::Null).expect("schedule");
    drop(wheel);

    // Restart *after* the deadline: not dropped, not silently delivered —
    // due on the first poll.
    clock.advance(5_000);
    let wheel2 = TimerWheel::with_clock(Box::new(JsonTimerStore::new(&path)), clock).expect("wheel 2");
    assert_eq!(wheel2.pending_count().expect("count"), 1);
    let fired = wheel2.due().expect("due on first poll");
    assert_eq!(fired.len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn corrupt_journal_fails_loud() {
    let dir = temp_dir("corrupt");
    let path = dir.join("timers.json");
    std::fs::write(&path, "this is not json").expect("write corrupt journal");

    let err = TimerWheel::new(Box::new(JsonTimerStore::new(&path)))
        .err()
        .expect("corrupt journal must fail the load");
    assert!(matches!(err, TimerError::Corrupt(_)), "got: {err:?}");

    // Valid JSON but a malformed record is equally loud.
    std::fs::write(&path, r#"{"timers":[{"timer_id":"t1"}]}"#).expect("write malformed");
    let err = TimerWheel::new(Box::new(JsonTimerStore::new(&path)))
        .err()
        .expect("malformed record must fail the load");
    assert!(matches!(err, TimerError::Corrupt(_)), "got: {err:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn record_json_roundtrip_is_lossless() {
    let rec = TimerRecord {
        id: TimerId("t-1-2".into()),
        fire_at_ms: 123_456_789,
        agent_id: Some("radar-1".into()),
        payload: json!({"deep": {"n": 3}, "list": [1, 2]}),
        status: TimerStatus::Pending,
    };
    let parsed = TimerRecord::from_json(&rec.to_json()).expect("roundtrip");
    assert_eq!(parsed, rec);

    // TimerStatus wire strings are the documented ones.
    assert_eq!(TimerStatus::Pending.as_str(), "pending");
    assert_eq!(TimerStatus::Fired.as_str(), "fired");
    assert_eq!(TimerStatus::Delivered.as_str(), "delivered");
    assert_eq!(TimerStatus::Cancelled.as_str(), "cancelled");
}

#[test]
fn store_failure_is_surfaced_not_swallowed() {
    /// A store that always fails — the daemon's fatal path depends on
    /// errors propagating out of the wheel.
    struct BrokenStore;
    impl TimerStore for BrokenStore {
        fn load(&self) -> Result<Vec<TimerRecord>, TimerError> {
            Err(TimerError::Store("disk gone".into()))
        }
        fn save(&self, _timers: &[TimerRecord]) -> Result<(), TimerError> {
            Err(TimerError::Store("disk gone".into()))
        }
    }

    assert!(TimerWheel::new(Box::new(BrokenStore)).is_err(), "load failure fails loud");

    let wheel = TimerWheel::with_clock(Box::new(BrokenStore), FrozenClock::new(0))
        .err()
        .expect("with_clock also fails loud on load failure");
    assert!(matches!(wheel, TimerError::Store(_)));
}
