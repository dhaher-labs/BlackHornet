//! Durable timers for the `bh-rtd` runtime wedge (PLAN.md §4 Phase 2).
//!
//! `infinity-agent-core` 0.2.0 deliberately ships only *in-process* timers
//! (`system/builder.rs`: "serverless platforms register durable-timer
//! equivalents instead"; `tools/sleep.rs`: "for serverless or otherwise
//! restart-safe deployments you need durable timers instead"). Following
//! that guidance and the R10 isolation rule ("isolate Infinity types behind
//! `bh-rtd` traits"), the daemon's durable timer is a **minimal bh-rtd-owned
//! wheel** behind the [`TimerStore`] trait — no Infinity types appear here,
//! and the persistence backend is swappable.
//!
//! Semantics (deliberately tiny, but real):
//!
//! * A timer is `(id, fire_at_epoch_ms, target agent id?, payload)` plus a
//!   [`TimerStatus`].
//! * Every mutation is persisted through the [`TimerStore`] *before* the
//!   in-memory state returns, so a daemon restart cannot lose a scheduled
//!   timer.
//! * On load, timers whose deadline already passed are not silently dropped:
//!   they become due on the first [`TimerWheel::due`] poll.
//! * Firing is level-triggered: [`TimerWheel::due`] moves a timer from
//!   [`TimerStatus::Pending`] to [`TimerStatus::Fired`] exactly once. The
//!   caller (daemon) then attempts delivery to the target thread and marks
//!   it [`TimerStatus::Delivered`]; a timer without a live target stays
//!   `Fired` in the journal instead of being dropped or re-fired.
//!
//! The default store ([`JsonTimerStore`]) writes one JSON document with a
//! temp-file + rename (atomic on POSIX) and `0600` permissions, mirroring
//! the trust model of the UDS peers (local-only, owner-readable).

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// Monotonic-ish unique timer id (`t-<epoch_ms>-<seq>`); no new dependency.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TimerId(pub String);

/// Lifecycle of a durable timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerStatus {
    /// Scheduled; will fire at `fire_at_ms`.
    Pending,
    /// Deadline passed and handed to the daemon; delivery not confirmed.
    Fired,
    /// Fired *and* delivered to a live target thread (or needed no target).
    Delivered,
    /// Cancelled before firing; kept in the journal for auditability.
    Cancelled,
}

impl TimerStatus {
    /// Wire representation used by the daemon protocol and the JSON store.
    pub fn as_str(self) -> &'static str {
        match self {
            TimerStatus::Pending => "pending",
            TimerStatus::Fired => "fired",
            TimerStatus::Delivered => "delivered",
            TimerStatus::Cancelled => "cancelled",
        }
    }

    fn parse(raw: &str) -> Option<TimerStatus> {
        match raw {
            "pending" => Some(TimerStatus::Pending),
            "fired" => Some(TimerStatus::Fired),
            "delivered" => Some(TimerStatus::Delivered),
            "cancelled" => Some(TimerStatus::Cancelled),
            _ => None,
        }
    }
}

/// One durable timer record.
#[derive(Clone, Debug, PartialEq)]
pub struct TimerRecord {
    /// Unique timer id.
    pub id: TimerId,
    /// Absolute deadline in milliseconds since the Unix epoch.
    pub fire_at_ms: u64,
    /// Colony agent id the fired payload should be delivered to, if any.
    pub agent_id: Option<String>,
    /// Opaque JSON payload delivered with the timer.
    pub payload: Value,
    /// Current lifecycle state.
    pub status: TimerStatus,
}

impl TimerRecord {
    /// Wire/JSON form (shared by the store and the daemon `list_timers`).
    pub fn to_json(&self) -> Value {
        json!({
            "timer_id": self.id.0,
            "fire_at_ms": self.fire_at_ms,
            "agent_id": self.agent_id,
            "payload": self.payload,
            "status": self.status.as_str(),
        })
    }

    /// Parse a record from its JSON form. Fails loud on malformed input —
    /// a corrupt timer journal is an operator-visible problem, not something
    /// to skip quietly.
    pub fn from_json(value: &Value) -> Result<TimerRecord, String> {
        let obj = value
            .as_object()
            .ok_or_else(|| "timer record is not a JSON object".to_string())?;
        let id = obj
            .get("timer_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "timer record missing string 'timer_id'".to_string())?;
        let fire_at_ms = obj
            .get("fire_at_ms")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("timer {id:?} missing u64 'fire_at_ms'"))?;
        let agent_id = obj
            .get("agent_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let payload = obj.get("payload").cloned().unwrap_or(Value::Null);
        let status = obj
            .get("status")
            .and_then(Value::as_str)
            .and_then(TimerStatus::parse)
            .ok_or_else(|| format!("timer {id:?} has unknown 'status'"))?;
        Ok(TimerRecord {
            id: TimerId(id.to_owned()),
            fire_at_ms,
            agent_id,
            payload,
            status,
        })
    }
}

/// Errors from the timer wheel / store. Fail-loud: callers surface these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimerError {
    /// The store could not be read or written.
    Store(String),
    /// A request referenced a timer id that does not exist.
    UnknownTimer(String),
    /// The store contained a record that failed validation.
    Corrupt(String),
}

impl core::fmt::Display for TimerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TimerError::Store(m) => write!(f, "timer store error: {m}"),
            TimerError::UnknownTimer(id) => write!(f, "unknown timer id {id:?}"),
            TimerError::Corrupt(m) => write!(f, "timer store corrupt: {m}"),
        }
    }
}

impl std::error::Error for TimerError {}

/// Persistence backend for the timer wheel (R10 isolation trait).
///
/// Implementations must persist the *whole* timer list on every `save`
/// call; the wheel treats `save` as the durability barrier.
pub trait TimerStore: Send + Sync {
    /// Load all records (empty list = fresh state).
    fn load(&self) -> Result<Vec<TimerRecord>, TimerError>;
    /// Atomically replace the persisted state with `timers`.
    fn save(&self, timers: &[TimerRecord]) -> Result<(), TimerError>;
}

/// JSON-file [`TimerStore`] (temp file + rename, `0600` permissions).
#[derive(Clone, Debug)]
pub struct JsonTimerStore {
    path: PathBuf,
}

impl JsonTimerStore {
    /// Store backed by the file at `path` (created on first save).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// File path backing this store.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl TimerStore for JsonTimerStore {
    fn load(&self) -> Result<Vec<TimerRecord>, TimerError> {
        let raw = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(TimerError::Store(e.to_string())),
        };
        let text = String::from_utf8(raw)
            .map_err(|e| TimerError::Corrupt(format!("{} is not UTF-8: {e}", self.path.display())))?;
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let doc: Value = serde_json::from_str(&text)
            .map_err(|e| TimerError::Corrupt(format!("{} is not valid JSON: {e}", self.path.display())))?;
        let timers = doc
            .get("timers")
            .and_then(Value::as_array)
            .ok_or_else(|| TimerError::Corrupt("journal missing 'timers' array".into()))?;
        timers
            .iter()
            .map(TimerRecord::from_json)
            .map(|r| r.map_err(TimerError::Corrupt))
            .collect()
    }

    fn save(&self, timers: &[TimerRecord]) -> Result<(), TimerError> {
        let doc = json!({
            "version": 1,
            "timers": timers.iter().map(TimerRecord::to_json).collect::<Vec<_>>(),
        });
        let mut text = serde_json::to_string_pretty(&doc)
            .map_err(|e| TimerError::Store(e.to_string()))?;
        text.push('\n');

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| TimerError::Store(format!("mkdir {}: {e}", parent.display())))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        {
            // 0600: same trust model as the UDS peers — local, owner-only.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|e| TimerError::Store(format!("open {}: {e}", tmp.display())))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
            file.write_all(text.as_bytes())
                .map_err(|e| TimerError::Store(format!("write {}: {e}", tmp.display())))?;
            file.sync_all()
                .map_err(|e| TimerError::Store(format!("sync {}: {e}", tmp.display())))?;
        }
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| TimerError::Store(format!("rename -> {}: {e}", self.path.display())))
    }
}

/// Wall clock used by the wheel (injectable for tests).
pub trait WallClock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

/// System wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl WallClock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// The durable timer wheel: in-memory state + store-backed persistence.
pub struct TimerWheel<C: WallClock = SystemClock> {
    inner: Mutex<WheelInner<C>>,
}

struct WheelInner<C: WallClock> {
    timers: Vec<TimerRecord>,
    store: Box<dyn TimerStore>,
    clock: C,
    seq: u64,
}

impl TimerWheel<SystemClock> {
    /// Build a wheel over `store`, loading persisted state.
    ///
    /// A failed load is a hard error (fail-loud) — the daemon refuses to
    /// start with a corrupt journal rather than silently losing timers.
    pub fn new(store: Box<dyn TimerStore>) -> Result<Self, TimerError> {
        Self::with_clock(store, SystemClock)
    }
}

impl<C: WallClock> TimerWheel<C> {
    /// Build a wheel with an explicit clock (tests inject frozen clocks).
    pub fn with_clock(store: Box<dyn TimerStore>, clock: C) -> Result<Self, TimerError> {
        let timers = store.load()?;
        Ok(Self {
            inner: Mutex::new(WheelInner {
                timers,
                store,
                clock,
                seq: 0,
            }),
        })
    }

    /// Schedule a new timer; returns its id. Persists before returning.
    pub fn schedule(
        &self,
        delay_ms: u64,
        agent_id: Option<String>,
        payload: Value,
    ) -> Result<TimerId, TimerError> {
        let mut inner = self.inner.lock().expect("timer wheel mutex poisoned");
        inner.seq += 1;
        let id = TimerId(format!("t-{}-{}", inner.clock.now_ms(), inner.seq));
        let record = TimerRecord {
            id: id.clone(),
            fire_at_ms: inner
                .clock
                .now_ms()
                .checked_add(delay_ms)
                .ok_or_else(|| TimerError::Store("delay overflow".into()))?,
            agent_id,
            payload,
            status: TimerStatus::Pending,
        };
        inner.timers.push(record);
        inner.persist()?;
        Ok(id)
    }

    /// Cancel a pending timer (idempotent for already-fired timers: returns
    /// `Ok(false)` — the deadline passed, there is nothing left to cancel).
    pub fn cancel(&self, id: &TimerId) -> Result<bool, TimerError> {
        let mut inner = self.inner.lock().expect("timer wheel mutex poisoned");
        let Some(record) = inner.timers.iter_mut().find(|r| &r.id == id) else {
            return Err(TimerError::UnknownTimer(id.0.clone()));
        };
        if record.status != TimerStatus::Pending {
            return Ok(false);
        }
        record.status = TimerStatus::Cancelled;
        inner.persist()?;
        Ok(true)
    }

    /// Pop every pending timer whose deadline has passed, marking them
    /// [`TimerStatus::Fired`] (persisted). Due timers are never re-fired.
    pub fn due(&self) -> Result<Vec<TimerRecord>, TimerError> {
        let mut inner = self.inner.lock().expect("timer wheel mutex poisoned");
        let now = inner.clock.now_ms();
        let mut due = Vec::new();
        for record in inner.timers.iter_mut() {
            if record.status == TimerStatus::Pending && record.fire_at_ms <= now {
                record.status = TimerStatus::Fired;
                due.push(record.clone());
            }
        }
        if !due.is_empty() {
            inner.persist()?;
        }
        Ok(due)
    }

    /// Mark a fired timer as delivered (journal + persistence).
    pub fn mark_delivered(&self, id: &TimerId) -> Result<(), TimerError> {
        let mut inner = self.inner.lock().expect("timer wheel mutex poisoned");
        let Some(record) = inner.timers.iter_mut().find(|r| &r.id == id) else {
            return Err(TimerError::UnknownTimer(id.0.clone()));
        };
        if record.status == TimerStatus::Fired {
            record.status = TimerStatus::Delivered;
            inner.persist()?;
        }
        Ok(())
    }

    /// Read-only snapshot of the whole journal (daemon `list_timers`).
    pub fn snapshot(&self) -> Result<Vec<TimerRecord>, TimerError> {
        let inner = self.inner.lock().expect("timer wheel mutex poisoned");
        Ok(inner.timers.clone())
    }

    /// Number of timers still pending.
    pub fn pending_count(&self) -> Result<usize, TimerError> {
        let inner = self.inner.lock().expect("timer wheel mutex poisoned");
        Ok(inner
            .timers
            .iter()
            .filter(|r| r.status == TimerStatus::Pending)
            .count())
    }
}

impl<C: WallClock> WheelInner<C> {
    fn persist(&self) -> Result<(), TimerError> {
        self.store.save(&self.timers)
    }
}
