//! Emit `FFT_GPUI_REV` from the workspace `Cargo.lock` so `--gate-out` evidence can name
//! the pinned gpui git revision without guessing at runtime.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect(
        "fft-ui build.rs: CARGO_MANIFEST_DIR is unset — cargo must set this for build scripts",
    ));
    let lock_path = find_cargo_lock(&manifest_dir);
    println!("cargo:rerun-if-changed={}", lock_path.display());

    let lock = fs::read_to_string(&lock_path).unwrap_or_else(|err| {
        panic!(
            "fft-ui build.rs: cannot read {}: {err}",
            lock_path.display()
        )
    });
    let rev = gpui_rev_from_lock(&lock).unwrap_or_else(|err| {
        panic!(
            "fft-ui build.rs: cannot extract gpui git rev from {}: {err}",
            lock_path.display()
        )
    });
    println!("cargo:rustc-env=FFT_GPUI_REV={rev}");
}

fn find_cargo_lock(start: &Path) -> PathBuf {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("Cargo.lock");
        if candidate.is_file() {
            return candidate;
        }
        if !dir.pop() {
            panic!(
                "fft-ui build.rs: walked up from {} without finding Cargo.lock",
                start.display()
            );
        }
    }
}

/// Locate `[[package]]` / `name = "gpui"` and parse a 40-hex rev from its `source` line.
fn gpui_rev_from_lock(lock: &str) -> Result<String, String> {
    let mut in_package = false;
    let mut is_gpui = false;
    let mut source: Option<String> = None;

    for line in lock.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            if is_gpui {
                break;
            }
            in_package = true;
            is_gpui = false;
            source = None;
            continue;
        }
        if !in_package {
            continue;
        }
        if trimmed.is_empty() {
            if is_gpui {
                break;
            }
            in_package = false;
            continue;
        }
        if let Some(name) = trimmed.strip_prefix("name = \"") {
            let name = name
                .strip_suffix('"')
                .ok_or_else(|| format!("malformed package name line: {trimmed}"))?;
            is_gpui = name == "gpui";
        } else if let Some(rest) = trimmed.strip_prefix("source = \"") {
            let rest = rest
                .strip_suffix('"')
                .ok_or_else(|| format!("malformed package source line: {trimmed}"))?;
            source = Some(rest.to_string());
        }
    }

    if !is_gpui {
        return Err("no [[package]] with name = \"gpui\"".to_string());
    }
    let source = source.ok_or_else(|| "gpui package has no source line".to_string())?;
    extract_git_rev(&source).ok_or_else(|| format!("gpui source has no 40-hex git rev: {source}"))
}

fn extract_git_rev(source: &str) -> Option<String> {
    // Prefer `?rev=<40-hex>` (Cargo.lock git sources); fall back to `#<40-hex>`.
    if let Some(idx) = source.find("?rev=") {
        let rev = &source[idx + "?rev=".len()..];
        let rev = rev.split(['#', '&']).next().unwrap_or(rev);
        if is_git_sha(rev) {
            return Some(rev.to_string());
        }
    }
    if let Some(idx) = source.rfind('#') {
        let rev = &source[idx + 1..];
        if is_git_sha(rev) {
            return Some(rev.to_string());
        }
    }
    None
}

fn is_git_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
