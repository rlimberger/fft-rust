//! Unit coverage for FAIL-evidence assembly (gate-honesty).

#[cfg(test)]
mod tests {
    use crate::report::{Budgets, PartialChecks, RuntimeFail, runtime_fail_evidence};
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[test]
    fn runtime_fail_writes_fail_verdict_and_dimension() {
        let replay = tempfile_path("m15-evidence-replay");
        std::fs::File::create(&replay)
            .expect("create replay fixture")
            .write_all(b"fixture-bytes")
            .expect("write replay fixture");
        let replay_s = replay.to_string_lossy().into_owned();

        let evidence = runtime_fail_evidence(RuntimeFail {
            replay: &replay_s,
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
        assert!(evidence.failures.contains(&"engine thread panic".into()));
        assert!(
            evidence
                .notes
                .as_deref()
                .is_some_and(|n| n.contains("engine thread panicked: boom"))
        );
        assert!(!evidence.source.ok);
        assert!(!evidence.append.ok);
        assert!(!evidence.identity.ok);
        assert_eq!(evidence.replay_bytes, 13);
        assert_eq!(evidence.replay_sha256, sha256_hex(b"fixture-bytes"));
        assert_ne!(evidence.git_sha, "unknown");
        assert!(evidence.git_dirty.is_some());
        assert!(!evidence.failures.iter().any(|f| f == "provenance"));
        let _ = std::fs::remove_file(&replay);
    }

    #[test]
    fn missing_replay_fixture_fails_provenance_dimension() {
        let missing = format!(
            "/tmp/m15-missing-replay-{}-{}.fftlog",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let evidence = runtime_fail_evidence(RuntimeFail {
            replay: &missing,
            head_ts: 1,
            live_out: "/tmp/live.fftlog",
            budgets: Budgets {
                apply_budget_ns: 1,
                gate_secs: 2,
                join_timeout_s: 600,
            },
            dimension: "source/head validation",
            diagnostic: "missing replay",
            partial: PartialChecks::default(),
        });
        assert_eq!(evidence.verdict, "FAIL");
        assert!(evidence.failures.contains(&"provenance".into()));
        assert_eq!(evidence.replay_bytes, 0);
        assert_eq!(evidence.replay_sha256, "unknown");
    }

    fn tempfile_path(prefix: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut child = Command::new("sha256sum")
            .arg("/dev/stdin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn sha256sum");
        child
            .stdin
            .take()
            .expect("sha256sum stdin")
            .write_all(bytes)
            .expect("write sha256sum stdin");
        let output = child.wait_with_output().expect("wait sha256sum");
        assert!(output.status.success(), "sha256sum failed");
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .expect("sha256 hex")
            .to_string()
    }
}
