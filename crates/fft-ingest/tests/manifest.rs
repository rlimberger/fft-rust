//! Verify SHA-256 of large DBN artifacts listed in `fixtures/MANIFEST.sha256`
//! (docs/FIXTURES.md). `#[ignore]` so a bare `cargo test` stays green without `data/`.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn data_dir() -> PathBuf {
    std::env::var_os("FFT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("data"))
}

fn manifest_path() -> PathBuf {
    repo_root().join("fixtures/MANIFEST.sha256")
}

fn hex_digest(path: &Path) -> String {
    let mut file = File::open(path).unwrap_or_else(|err| {
        panic!(
            "cannot open {}: {err}\nAcquire Databento batch GLBX-20260803-4WJS899FNL into {} \
             (see docs/FIXTURES.md).",
            path.display(),
            data_dir().display()
        );
    });
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf).unwrap_or_else(|err| {
            panic!("read {}: {err}", path.display());
        });
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
#[ignore = "requires large data/ DBN week; run with --ignored when data is present"]
fn large_mbo_files_match_manifest() {
    let manifest = manifest_path();
    let file = File::open(&manifest).unwrap_or_else(|err| {
        panic!("missing {}: {err}", manifest.display());
    });
    let data = data_dir();
    let mut checked = 0u32;
    for (lineno, line) in BufReader::new(file).lines().enumerate() {
        let line = line.expect("read manifest line");
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (hash, rel) = line.split_once("  ").unwrap_or_else(|| {
            panic!(
                "{}:{}: expected `<sha256>  <relative-path>` (two spaces), got {line:?}",
                manifest.display(),
                lineno + 1
            );
        });
        assert_eq!(hash.len(), 64, "line {}: bad sha256 length", lineno + 1);
        assert!(
            rel.ends_with(".mbo.dbn.zst"),
            "line {}: manifest is for .mbo.dbn.zst only, got {rel}",
            lineno + 1
        );
        let path = data.join(rel);
        assert!(
            path.is_file(),
            "missing {}; acquire Databento batch GLBX-20260803-4WJS899FNL into {} \
             (see docs/FIXTURES.md)",
            path.display(),
            data.display()
        );
        let got = hex_digest(&path);
        assert_eq!(
            got,
            hash,
            "SHA-256 mismatch for {}\n  manifest: {hash}\n  on disk:  {got}\n\
             Re-acquire the batch or update fixtures/MANIFEST.sha256 after a verified replace.",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 5,
        "manifest listed only {checked} mbo files; expected the sample week"
    );
}
