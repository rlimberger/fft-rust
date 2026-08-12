let meta = #{
    name: "wave11-residuals",
    description: "Wave 11: review M1.5 residuals, implement UI chrome and docs, verify",
    phases: [
        #{ title: "Review", detail: "read-only audit of uncommitted engine and UI residual" },
        #{ title: "Implement", detail: "non-colliding residual tracks" },
        #{ title: "Verify", detail: "adversarial skeptics on each track" },
    ],
    when_to_use: "Continue IMPLEMENTATION-PLAN after M1.5: SimLive UI, provenance, checkpoint cadence, docs",
};

let findings_schema = #{
    "type": "object",
    "required": ["blockers", "nits", "summary"],
    "properties": #{
        "blockers": #{ "type": "array", "maxItems": 12, "items": #{ "type": "string" } },
        "nits": #{ "type": "array", "maxItems": 12, "items": #{ "type": "string" } },
        "summary": #{ "type": "string" },
    },
};

let done_schema = #{
    "type": "object",
    "required": ["ok", "files_touched", "tests", "notes"],
    "properties": #{
        "ok": #{ "type": "boolean" },
        "files_touched": #{ "type": "array", "maxItems": 20, "items": #{ "type": "string" } },
        "tests": #{ "type": "string" },
        "notes": #{ "type": "string" },
    },
};

let verdict_schema = #{
    "type": "object",
    "required": ["real", "reason", "evidence"],
    "properties": #{
        "real": #{ "type": "boolean" },
        "reason": #{ "type": "string" },
        "evidence": #{ "type": "string" },
    },
};

phase("Review");

let p_engine = "";
p_engine += "Review UNCOMMITTED fft-engine residual in /home/rlimberger/Projects/fft-rust. ";
p_engine += "Use git diff and read_file/grep only. Never mutate git or the tree. ";
p_engine += "Scope: crates/fft-engine/src/live_log.rs (60s wall-clock checkpoint absolute cadence + tests); ";
p_engine += "forward.rs and snapshot.rs (LiveTransportPhase); sim_live.rs; runtime.rs; ";
p_engine += "bin/m15_gate/report.rs and evidence_tests.rs (provenance hard-fail + replay sha256); ";
p_engine += "Cargo.toml sha2; docs/ENGINE.md live_phase. ";
p_engine += "Doctrine: fail loudly; budgets in time; 500-line rule; ENGINE.md section 5 is law. ";
p_engine += "Report blockers and nits. Empty blockers only after reading diffs. summary = one paragraph.";

let p_ui = "";
p_ui += "Review UNCOMMITTED fft-ui SimLive CLI/spawn residual in /home/rlimberger/Projects/fft-rust. ";
p_ui += "Use git diff and read_file/grep only. Never mutate git. ";
p_ui += "Scope: crates/fft-ui/src/main.rs, shell.rs, shell_replay.rs, Cargo.toml. ";
p_ui += "Check: --sim-live/--head/--live-out exclusive with --replay; head snap to exact event ts before SetSource; ";
p_ui += "no I/O before first paint (on_next_frame); 500-line rule; loud exit on invalid head. ";
p_ui += "Report blockers and nits after reading diffs.";

let p_gaps = "";
p_gaps += "Inventory Wave 11 residual GAPS still missing from the uncommitted tree in /home/rlimberger/Projects/fft-rust. ";
p_gaps += "Read HANDOFF.md Wave 10 residuals, IMPLEMENTATION-PLAN M5/M7, git status/diff. Tools only, no mutations. ";
p_gaps += "Residuals: (1) 60s wall checkpoint cadence tests (2) m15 provenance hard-fail ";
p_gaps += "(3) prior-session-in-checkpoint vs live-out ruling (4) UI SimLive CLI + GoLive key + LIVE indicator. ";
p_gaps += "For each: DONE, PARTIAL with remaining work, or MISSING. blockers = merge-blocking incompleteness.";

let reviews = parallel([
    #{
        label: "review:engine",
        capability_mode: "read-only",
        model: "ocx-gpt-5-6-sol",
        output_schema: findings_schema,
        prompt: p_engine,
    },
    #{
        label: "review:ui-cli",
        capability_mode: "read-only",
        model: "ocx-cursor-grok-4-5-fast",
        output_schema: findings_schema,
        prompt: p_ui,
    },
    #{
        label: "review:gaps",
        capability_mode: "read-only",
        model: "ocx-xai-grok-4-5",
        output_schema: findings_schema,
        prompt: p_gaps,
    },
]);

let review_text = "";
for r in reviews {
    if r != () && r.success && r.output.summary != () {
        review_text += r.output.summary;
        review_text += " | ";
        if r.output.blockers != () {
            for b in r.output.blockers {
                review_text += "BLOCK:";
                review_text += b;
                review_text += "; ";
            }
        }
    }
}
log("Review complete: " + review_text);

phase("Implement");

let p_golive = "";
p_golive += "IMPLEMENT in /home/rlimberger/Projects/fft-rust shared tree. NO git commit/stash/clean/checkout/push. ";
p_golive += "GOAL: Wire SimLive transport chrome - GoLive key + LIVE indicator. ";
p_golive += "Already in tree: RenderSnapshot.live_phase LiveTransportPhase; CLI --sim-live and spawn_sim_live_engine; ";
p_golive += "transport.go_live_placeholder still says go-live M6 and sends no EngineCmd::GoLive. ";
p_golive += "REQUIREMENTS: (1) Add TransportCommand::GoLive; map in shell_input::dispatch_transport to EngineCmd::GoLive. ";
p_golive += "(2) KeyCtx gains live_phase; key l emits GoLive when phase is not Inactive; Inactive gets status hint needs sim-live and no command. ";
p_golive += "(3) HeaderArgs gains live_phase; paint CATCHING-UP / LIVE / SCRUBBED / GO-LIVE; omit when Inactive. Wire shell.rs both header sites. ";
p_golive += "(4) Update transport_tests; keep files under 500 lines; match style. ";
p_golive += "(5) Run cargo test -p fft-ui --lib and fix failures you introduce. ";
p_golive += "Return ok, files_touched, tests, notes.";

let p_docs = "";
p_docs += "DOCUMENT in /home/rlimberger/Projects/fft-rust. NO git mutations. ";
p_docs += "Update README.md and docs/OPERATOR.md for SimLive CLI already in uncommitted tree: ";
p_docs += "--sim-live, --head, --live-out; exclusive with --replay; head wall-clock snap; live-out must differ; ";
p_docs += "transport armed; l = GoLive; m15-gate pointer; example with Wed checkpoint head 2026-07-29T13:50:00Z. ";
p_docs += "Read crates/fft-ui/src/main.rs usage first. Do not invent flags. Short measurable style. ";
p_docs += "Return ok, files_touched, tests, notes.";

let p_snap = "";
p_snap += "IMPLEMENT tests only in /home/rlimberger/Projects/fft-rust. NO git mutations. ";
p_snap += "Add unit tests for shell_replay::snap_sim_live_head without exceeding 500 lines. ";
p_snap += "Prefer crates/fft-ui/src/shell_replay_tests.rs via cfg test mod, or extract fallible helper for testability. ";
p_snap += "Assert snap to last event <= head; exact event returns that ts; StartupSource meta_path and starts_engine. ";
p_snap += "CLI path must stay loud on failure. Run cargo test -p fft-ui --lib shell_replay. ";
p_snap += "Do not edit main.rs/shell.rs/transport beyond test module wiring. Return ok, files_touched, tests, notes.";

let impls = parallel([
    #{
        label: "impl:ui-golive-live",
        capability_mode: "read-write",
        model: "ocx-gpt-5-6-sol",
        output_schema: done_schema,
        prompt: p_golive,
    },
    #{
        label: "impl:docs-simlive",
        capability_mode: "read-write",
        model: "ocx-cursor-grok-4-5-fast",
        output_schema: done_schema,
        prompt: p_docs,
    },
    #{
        label: "impl:shell-replay-tests",
        capability_mode: "read-write",
        model: "ocx-xai-grok-4-5",
        output_schema: done_schema,
        prompt: p_snap,
    },
]);

let impl_ok = true;
let impl_summary = "";
for r in impls {
    if r == () || !r.success {
        impl_ok = false;
        impl_summary += "impl-slot-failed; ";
    } else if r.output.ok == false {
        impl_ok = false;
        impl_summary += r.output.notes;
        impl_summary += "; ";
    } else {
        impl_summary += "ok:";
        impl_summary += r.output.notes;
        impl_summary += "; ";
    }
}
log("Implement: " + impl_summary);

phase("Verify");

let p_v_ui = "";
p_v_ui += "Adversarially verify UI GoLive + LIVE chrome in /home/rlimberger/Projects/fft-rust. ";
p_v_ui += "Read transport.rs, shell_input.rs, header.rs, shell.rs, transport_tests.rs. ";
p_v_ui += "real=true only if: l emits EngineCmd::GoLive on sim-live phase; Inactive does not claim live; ";
p_v_ui += "header shows LIVE/CATCHING-UP from live_phase; tests cover the change. Cite file:line. Never mutate.";

let p_v_docs = "";
p_v_docs += "Adversarially verify README.md and docs/OPERATOR.md sim-live docs against crates/fft-ui/src/main.rs flags. ";
p_v_docs += "real=true only if docs match real CLI, mutual exclusion, and live-out distinctness. Cite evidence. Never mutate.";

let p_v_eng = "";
p_v_eng += "Adversarially verify uncommitted engine residual is shippable in /home/rlimberger/Projects/fft-rust. ";
p_v_eng += "live_log absolute 60s cadence tests match ENGINE.md 5.4; m15 provenance hard-fails verdict; LiveTransportPhase published. ";
p_v_eng += "real=true only with evidence all three correct. Never mutate.";

let verdicts = parallel([
    #{
        label: "verify:ui-golive",
        capability_mode: "read-only",
        model: "ocx-cursor-grok-4-5-fast",
        output_schema: verdict_schema,
        prompt: p_v_ui,
    },
    #{
        label: "verify:docs",
        capability_mode: "read-only",
        model: "ocx-xai-grok-4-5",
        output_schema: verdict_schema,
        prompt: p_v_docs,
    },
    #{
        label: "verify:engine-residual",
        capability_mode: "read-only",
        model: "ocx-gpt-5-6-sol",
        output_schema: verdict_schema,
        prompt: p_v_eng,
    },
]);

let confirmed = 0;
let total = 0;
let report = "Wave 11 residual workflow report\n\n## Reviews\n" + review_text + "\n\n## Implement\n" + impl_summary + "\n\n## Verify\n";
for v in verdicts {
    total += 1;
    if v != () && v.success && v.output.real == true {
        confirmed += 1;
        report += "PASS: ";
        report += v.output.evidence;
        report += "\n";
    } else if v != () && v.success {
        report += "FAIL/UNVERIFIED: ";
        report += v.output.reason;
        report += "\n";
    } else {
        report += "SLOT FAILED\n";
    }
}
report += "\nConfirmed " + confirmed.to_string() + "/" + total.to_string();
let path = write_scratch_file("wave11-report.md", report);
complete(#{
    path: path,
    confirmed: confirmed,
    total: total,
    impl_ok: impl_ok,
    summary: report,
});
