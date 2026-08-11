//! UI prefs persistence — `$XDG_CONFIG_HOME/fft/prefs.toml` (fallback `~/.config/fft`).
//!
//! Hand-rolled `key = value` lines (no toml crate). Invalid or missing keys fall back
//! to defaults with a single loud stderr WARNING per key; never panic on load or save.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::mp_layout::{ZOOM_MAX, ZOOM_MIN};
use crate::pane_state::PaneState;
use crate::transport::{SPEED_LADDER, TransportState};

/// Default splitter ratio (matches [`crate::pane_state::SplitterState`]).
pub const DEFAULT_SPLITTER_RATIO: f32 = 0.48;
/// Default MP zoom (1.0 = design-time strip widths).
pub const DEFAULT_MP_ZOOM: f32 = 1.0;
/// Valid pane tick scales (PRD §5).
const VALID_SCALES: &[u8] = &[1, 2, 4];
const SPLITTER_MIN: f32 = 0.1;
const SPLITTER_MAX: f32 = 0.9;

/// Persisted UI state (v1).
#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub mp_scale: u8,
    pub dom_scale: u8,
    pub splitter_ratio: f32,
    pub mp_zoom: f32,
    pub transport_speed_index: usize,
}

/// Quit-hook handles: main holds these across `app.run` and writes prefs after return.
/// Avoids reaching into pane/transport internals from the binary.
#[derive(Clone)]
pub struct ShellPrefsHandles {
    pub panes: Rc<RefCell<PaneState>>,
    pub transport: Rc<RefCell<TransportState>>,
}

impl ShellPrefsHandles {
    pub fn new(panes: Rc<RefCell<PaneState>>, transport: Rc<RefCell<TransportState>>) -> Self {
        Self { panes, transport }
    }

    /// Collect the prefs snapshot from live pane + transport state.
    pub fn snapshot(&self) -> Prefs {
        let pane = self.panes.borrow().prefs_snapshot();
        let transport = self.transport.borrow().prefs_snapshot();
        Prefs {
            mp_scale: pane.mp_scale,
            dom_scale: pane.dom_scale,
            splitter_ratio: pane.splitter_ratio,
            mp_zoom: pane.mp_zoom,
            transport_speed_index: transport.speed_index,
        }
    }
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            mp_scale: 1,
            dom_scale: 1,
            splitter_ratio: DEFAULT_SPLITTER_RATIO,
            mp_zoom: DEFAULT_MP_ZOOM,
            transport_speed_index: crate::transport::DEFAULT_SPEED_INDEX,
        }
    }
}

impl Prefs {
    /// Resolve the prefs path: `$XDG_CONFIG_HOME/fft/prefs.toml`, else `~/.config/fft/prefs.toml`.
    pub fn path() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            let root = PathBuf::from(xdg);
            if !root.as_os_str().is_empty() {
                return root.join("fft").join("prefs.toml");
            }
        }
        let home = std::env::var_os("HOME").unwrap_or_else(|| {
            eprintln!("fft: WARNING prefs: HOME unset; writing under ./fft/prefs.toml");
            ".".into()
        });
        PathBuf::from(home)
            .join(".config")
            .join("fft")
            .join("prefs.toml")
    }

    /// Load from the canonical path. Missing file ⇒ defaults (no warning).
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    /// Load from an explicit path. Missing file ⇒ defaults (no warning).
    pub fn load_from(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => parse_prefs(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                eprintln!(
                    "fft: WARNING prefs: cannot read {}: {err}; using defaults",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Atomic write (temp + rename). Creates the parent dir. Write failure ⇒ stderr, no panic.
    pub fn save(&self) {
        self.save_to(&Self::path());
    }

    /// Atomic write to an explicit path.
    pub fn save_to(&self, path: &Path) {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && let Err(err) = fs::create_dir_all(parent)
        {
            eprintln!(
                "fft: WARNING prefs: cannot create {}: {err}",
                parent.display()
            );
            return;
        }
        let body = serialize_prefs(self);
        let tmp = prefs_temp_path(path);
        if let Err(err) = fs::write(&tmp, body.as_bytes()) {
            eprintln!("fft: WARNING prefs: cannot write {}: {err}", tmp.display());
            return;
        }
        if let Err(err) = fs::rename(&tmp, path) {
            eprintln!(
                "fft: WARNING prefs: cannot rename {} → {}: {err}",
                tmp.display(),
                path.display()
            );
            let _ = fs::remove_file(&tmp);
        }
    }
}

/// Parse prefs text. Unknown keys are ignored with a WARNING; bad values use defaults.
pub fn parse_prefs(text: &str) -> Prefs {
    let defaults = Prefs::default();
    let mut mp_scale = defaults.mp_scale;
    let mut dom_scale = defaults.dom_scale;
    let mut splitter_ratio = defaults.splitter_ratio;
    let mut mp_zoom = defaults.mp_zoom;
    let mut transport_speed_index = defaults.transport_speed_index;

    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            eprintln!(
                "fft: WARNING prefs: unexpected table on line {}: {line}",
                lineno + 1
            );
            continue;
        }
        let Some((key, rest)) = line.split_once('=') else {
            eprintln!(
                "fft: WARNING prefs: expected key = value on line {}: {line}",
                lineno + 1
            );
            continue;
        };
        let key = key.trim();
        let value = rest.trim().trim_matches('"').trim_matches('\'');
        match key {
            "mp_scale" => mp_scale = parse_scale(key, value, defaults.mp_scale),
            "dom_scale" => dom_scale = parse_scale(key, value, defaults.dom_scale),
            "splitter_ratio" => {
                splitter_ratio = parse_f32_clamped(
                    key,
                    value,
                    SPLITTER_MIN,
                    SPLITTER_MAX,
                    defaults.splitter_ratio,
                );
            }
            "mp_zoom" => {
                mp_zoom = parse_f32_clamped(key, value, ZOOM_MIN, ZOOM_MAX, defaults.mp_zoom);
            }
            "transport_speed_index" => {
                transport_speed_index =
                    parse_speed_index(key, value, defaults.transport_speed_index);
            }
            other => {
                eprintln!(
                    "fft: WARNING prefs: unknown key `{other}` on line {}",
                    lineno + 1
                );
            }
        }
    }

    Prefs {
        mp_scale,
        dom_scale,
        splitter_ratio,
        mp_zoom,
        transport_speed_index,
    }
}

/// Serialize prefs as stable `key = value` lines (deterministic order).
pub fn serialize_prefs(prefs: &Prefs) -> String {
    format!(
        "mp_scale = {}\n\
         dom_scale = {}\n\
         splitter_ratio = {}\n\
         mp_zoom = {}\n\
         transport_speed_index = {}\n",
        prefs.mp_scale,
        prefs.dom_scale,
        format_f32(prefs.splitter_ratio),
        format_f32(prefs.mp_zoom),
        prefs.transport_speed_index,
    )
}

fn prefs_temp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

fn parse_scale(key: &str, value: &str, default: u8) -> u8 {
    match value.parse::<u8>() {
        Ok(n) if VALID_SCALES.contains(&n) => n,
        Ok(_) | Err(_) => {
            eprintln!(
                "fft: WARNING prefs: invalid `{key}` value `{value}`; using default {default}"
            );
            default
        }
    }
}

fn parse_f32_clamped(key: &str, value: &str, min: f32, max: f32, default: f32) -> f32 {
    match value.parse::<f32>() {
        Ok(n) if n.is_finite() => {
            if n < min || n > max {
                eprintln!("fft: WARNING prefs: `{key}` value {n} outside [{min}, {max}]; clamping");
                n.clamp(min, max)
            } else {
                n
            }
        }
        Ok(_) | Err(_) => {
            eprintln!(
                "fft: WARNING prefs: invalid `{key}` value `{value}`; using default {default}"
            );
            default
        }
    }
}

fn parse_speed_index(key: &str, value: &str, default: usize) -> usize {
    let max = SPEED_LADDER.len().saturating_sub(1);
    match value.parse::<usize>() {
        Ok(n) if n <= max => n,
        Ok(n) => {
            eprintln!("fft: WARNING prefs: `{key}` value {n} exceeds ladder max {max}; clamping");
            max
        }
        Err(_) => {
            eprintln!(
                "fft: WARNING prefs: invalid `{key}` value `{value}`; using default {default}"
            );
            default
        }
    }
}

/// Compact f32 text (trim trailing zeros after the decimal).
fn format_f32(n: f32) -> String {
    let s = format!("{n}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn round_trip_serialize_parse() {
        let prefs = Prefs {
            mp_scale: 2,
            dom_scale: 4,
            splitter_ratio: 0.55,
            mp_zoom: 1.5,
            transport_speed_index: 5,
        };
        let text = serialize_prefs(&prefs);
        let parsed = parse_prefs(&text);
        assert_eq!(parsed, prefs);
    }

    #[test]
    fn defaults_when_empty() {
        assert_eq!(parse_prefs(""), Prefs::default());
        assert_eq!(parse_prefs("# only comments\n"), Prefs::default());
    }

    #[test]
    fn invalid_scale_falls_back() {
        let p = parse_prefs("mp_scale = 3\ndom_scale = no\n");
        assert_eq!(p.mp_scale, 1);
        assert_eq!(p.dom_scale, 1);
    }

    #[test]
    fn splitter_and_zoom_clamp() {
        let p = parse_prefs("splitter_ratio = 0.01\nmp_zoom = 9.0\n");
        assert!((p.splitter_ratio - SPLITTER_MIN).abs() < 1e-6);
        assert!((p.mp_zoom - ZOOM_MAX).abs() < 1e-6);
        let p = parse_prefs("splitter_ratio = 0.99\nmp_zoom = 0.1\n");
        assert!((p.splitter_ratio - SPLITTER_MAX).abs() < 1e-6);
        assert!((p.mp_zoom - ZOOM_MIN).abs() < 1e-6);
    }

    #[test]
    fn speed_index_clamps_to_ladder() {
        let p = parse_prefs("transport_speed_index = 999\n");
        assert_eq!(p.transport_speed_index, SPEED_LADDER.len() - 1);
        let p = parse_prefs("transport_speed_index = 0\n");
        assert_eq!(p.transport_speed_index, 0);
        let p = parse_prefs("transport_speed_index = -1\n");
        assert_eq!(
            p.transport_speed_index,
            Prefs::default().transport_speed_index
        );
    }

    #[test]
    fn missing_file_is_defaults() {
        let path = PathBuf::from("/tmp/fft-prefs-definitely-missing-xyz.toml");
        assert_eq!(Prefs::load_from(&path), Prefs::default());
    }

    #[test]
    fn filesystem_round_trip_atomic() {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("fft-prefs-test-{nanos}-{n}"));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("prefs.toml");
        let prefs = Prefs {
            mp_scale: 4,
            dom_scale: 2,
            splitter_ratio: 0.33,
            mp_zoom: 2.0,
            transport_speed_index: 3,
        };
        prefs.save_to(&path);
        assert!(path.is_file());
        assert!(!prefs_temp_path(&path).exists(), "temp file cleaned up");
        let loaded = Prefs::load_from(&path);
        assert_eq!(loaded, prefs);
        // Nested create: parent missing.
        let nested = dir.join("nested").join("prefs.toml");
        prefs.save_to(&nested);
        assert_eq!(Prefs::load_from(&nested), prefs);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_file_fills_defaults() {
        let p = parse_prefs("mp_scale = 4\n");
        assert_eq!(p.mp_scale, 4);
        assert_eq!(p.dom_scale, 1);
        assert!((p.splitter_ratio - DEFAULT_SPLITTER_RATIO).abs() < 1e-6);
        assert!((p.mp_zoom - DEFAULT_MP_ZOOM).abs() < 1e-6);
        assert_eq!(
            p.transport_speed_index,
            crate::transport::DEFAULT_SPEED_INDEX
        );
    }
}
