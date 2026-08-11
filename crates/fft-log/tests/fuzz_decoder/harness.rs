//! Full read-surface exercise + catch_unwind runner for the LOG-FUZZ harness.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use fft_log::{KIND_CHECKPOINT, LogReader};

use crate::common::temp_path;

/// Decoder panic / hang / unbounded alloc on arbitrary bytes — M7 zero-defect gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Sev1,
    /// Data-integrity or contract ambiguity short of panic.
    Sev2,
}

#[derive(Debug)]
pub struct Finding {
    pub recipe: String,
    pub detail: String,
    pub severity: Severity,
}

/// Exercise every public decode path on `path`. Returns `Ok(())` on typed success or
/// typed `LogError`; panics (caught by the harness) are the only findings.
pub fn exercise_full_surface(path: &Path) -> Result<(), String> {
    match LogReader::open(path) {
        Err(e) => {
            // Typed error is the contract. Display + Debug must not panic either.
            let _ = format!("{e}");
            let _ = format!("{e:?}");
            Ok(())
        }
        Ok((reader, report)) => {
            let _ = format!("{report:?}");
            let _ = format!("{reader:?}");
            let _ = reader.meta();
            let _ = reader.version();
            let _ = reader.schema_tag();
            let _ = reader.opened_live();
            let _ = reader.is_live();
            let n = reader.frame_count();
            let _ = reader.index();

            for i in 0..n {
                match reader.frame_header(i) {
                    Ok(fh) => {
                        let _ = format!("{fh:?}");
                        if fh.kind == KIND_CHECKPOINT {
                            match reader.read_checkpoint(i) {
                                Ok(sections) => {
                                    let _ = sections.len();
                                }
                                Err(e) => {
                                    let _ = format!("{e}");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = format!("{e}");
                    }
                }
            }

            // Full event iteration (skips checkpoints; first error fuses).
            let mut count = 0usize;
            for item in reader.events(0..n) {
                match item {
                    Ok(_ev) => count += 1,
                    Err(e) => {
                        let _ = format!("{e}");
                        break;
                    }
                }
                // Hard ceiling against unbounded decode (malformed count / payload).
                if count > 10_000_000 {
                    return Err(format!(
                        "event iteration exceeded 10M events (possible unbounded decode); \
                         frames={n} index_source={:?}",
                        report.index_source
                    ));
                }
            }

            // refresh() on a just-opened path (no concurrent writer) must also be safe.
            // We cannot mutably refresh through the immutable path above without re-open;
            // re-open + refresh covers that surface.
            drop(reader);
            if let Ok((mut r2, _)) = LogReader::open(path) {
                match r2.refresh() {
                    Ok(rr) => {
                        let _ = format!("{rr:?}");
                    }
                    Err(e) => {
                        let _ = format!("{e}");
                    }
                }
            }
            Ok(())
        }
    }
}

/// Write `bytes` to a temp path, run the full surface inside `catch_unwind`, return a
/// finding description if the contract is violated.
pub fn run_one(bytes: &[u8], recipe: &str) -> Option<Finding> {
    let tmp = temp_path("fuzz-case");
    if let Err(e) = std::fs::write(tmp.path(), bytes) {
        return Some(Finding {
            recipe: recipe.to_string(),
            detail: format!("test harness failed to write temp file: {e}"),
            severity: Severity::Sev2,
        });
    }

    let path = tmp.path().to_path_buf();
    let result = catch_unwind(AssertUnwindSafe(|| exercise_full_surface(&path)));
    match result {
        Ok(Ok(())) => None,
        Ok(Err(msg)) => Some(Finding {
            recipe: recipe.to_string(),
            detail: msg,
            severity: Severity::Sev1,
        }),
        Err(payload) => {
            let panic_msg = panic_payload_to_string(payload);
            Some(Finding {
                recipe: recipe.to_string(),
                detail: format!("PANIC: {panic_msg}"),
                severity: Severity::Sev1,
            })
        }
    }
}

fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".into()
    }
}
