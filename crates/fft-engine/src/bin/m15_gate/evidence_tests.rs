//! Unit coverage for FAIL-evidence assembly (gate-honesty).

#[cfg(test)]
mod tests {
    use crate::report::{Budgets, PartialChecks, RuntimeFail, runtime_fail_evidence};

    #[test]
    fn runtime_fail_writes_fail_verdict_and_dimension() {
        let evidence = runtime_fail_evidence(RuntimeFail {
            replay: "/tmp/replay.fftlog",
            head_ts: 42,
            live_out: "/tmp/live.fftlog",
            budgets: Budgets {
                apply_budget_ns: 1,
                gate_secs: 2,
                join_timeout_s: 600,
            },
            dimension: "engine thread panic",
            diagnostic: "engine thread panicked: boom",
            partial: PartialChecks::default(),
        });
        assert_eq!(evidence.verdict, "FAIL");
        assert_eq!(evidence.failures, vec!["engine thread panic".to_string()]);
        assert!(
            evidence
                .notes
                .as_deref()
                .is_some_and(|n| n.contains("engine thread panicked: boom"))
        );
        assert!(!evidence.source.ok);
        assert!(!evidence.append.ok);
        assert!(!evidence.identity.ok);
    }
}
