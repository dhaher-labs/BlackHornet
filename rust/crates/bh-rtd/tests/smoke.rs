//! Phase 0 smoke tests — hermetic (no network, no Infinity runtime).
//!
//! The `#[cfg(feature = "infinity")]` tests additionally prove the pinned
//! `infinity-agent-core` 0.2.0 dependency links and the verified trait seams
//! construct real objects; they are excluded from
//! `cargo test --no-default-features`.

use bh_rtd::host::{AgentHost, AgentId, AgentSpec, ColonyEvent, ColonyEventKind, ColdHost, HostError};
use bh_rtd::{RuntimeError, RuntimeFlavor};

#[test]
fn runtime_flavor_defaults_to_python() {
    assert_eq!(RuntimeFlavor::default(), RuntimeFlavor::Python);
}

#[test]
fn runtime_flavor_parses_documented_values_case_insensitively() {
    assert_eq!(RuntimeFlavor::parse("python").unwrap(), RuntimeFlavor::Python);
    assert_eq!(RuntimeFlavor::parse("PYTHON").unwrap(), RuntimeFlavor::Python);
    assert_eq!(RuntimeFlavor::parse(" rust \n").unwrap(), RuntimeFlavor::Rust);
}

#[test]
fn runtime_flavor_fails_closed_on_unknown_value() {
    let err = RuntimeFlavor::parse("simulated").unwrap_err();
    assert_eq!(
        err,
        RuntimeError::UnknownRuntime("simulated".to_string()),
        "bad BH_RUNTIME values must error, not silently fall back (no fake-success)"
    );
    assert!(err.to_string().contains("BH_RUNTIME"));
}

#[test]
fn cold_host_fails_loud_instead_of_simulating() {
    let mut host = ColdHost;
    let err = host
        .spawn_agent(AgentSpec {
            name: "worker-1".into(),
            role: "worker".into(),
            instructions: "noop".into(),
        })
        .unwrap_err();
    assert!(matches!(err, HostError::Unsupported(_)));

    let err2 = host
        .send_event(
            &AgentId("worker-1".into()),
            ColonyEvent {
                kind: ColonyEventKind::TaskAssigned,
                payload_json: "{}".into(),
            },
        )
        .unwrap_err();
    assert!(matches!(err2, HostError::UnknownAgent(_)));

    // Nothing was ever started, so shutdown truthfully succeeds.
    assert!(host.shutdown().is_ok());
}

#[test]
fn host_error_display_is_actionable() {
    assert_eq!(
        HostError::Unsupported("not wired").to_string(),
        "unsupported operation: not wired"
    );
}

#[cfg(feature = "infinity")]
mod infinity_link {
    use bh_rtd::infinity_adapter::{AdapterError, BhMemorySeam, StateStore};

    #[tokio::test]
    async fn in_memory_seam_registers_colony_threads() {
        let seam = BhMemorySeam::in_memory();
        assert!(!seam.ensure_colony_thread("queen").await.unwrap());
        // Second call reports the thread record now exists (idempotent).
        assert!(seam.ensure_colony_thread("queen").await.unwrap());
    }

    #[tokio::test]
    async fn state_store_roundtrips_processed_message_ids() {
        use bh_rtd::infinity_adapter::ThreadId;
        let seam = BhMemorySeam::in_memory();
        seam.mark_events_processed("radar-1", vec!["m-1".into()]).await.unwrap();
        let seen = seam
            .state_store()
            .get_processed_ids(&ThreadId::from("radar-1".to_owned()))
            .await
            .unwrap();
        assert!(seen.contains("m-1"));
    }

    #[test]
    fn adapter_error_is_a_std_error() {
        let e = AdapterError::Store("boom".into());
        assert!(format!("{e}").contains("boom"));
        let _: &dyn std::error::Error = &e;
    }
}
