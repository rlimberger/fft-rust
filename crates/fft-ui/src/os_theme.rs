//! Live Omarchy OS theme + font-size pickup.
//!
//! Colors and `base-size` come from the host Omarchy theme system and are
//! re-read on a dedicated poll thread (500 ms). The UI thread only loads the
//! latest-value snapshot — never blocks on I/O.
//!
//! Font *family* is resolved once at startup via `fc-match monospace` (same
//! rule as `omarchy-font-current`). Live family switching is out of scope.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::theme::Palette;

/// Canonical Omarchy state root (user-local).
const OMARCHY_STATE: &str = ".local/state/omarchy";
const OMARCHY_CONFIG: &str = ".config/omarchy";

/// Poll interval for theme / font-size mtime checks.
const POLL_MS: u64 = 500;

/// Design-time base font size: `scale = base_size / DESIGN_BASE_SIZE`.
pub const DESIGN_BASE_SIZE: f32 = 12.0;

/// Floor for a parsed `base-size` (anything smaller is clamped up).
const BASE_SIZE_FLOOR: f32 = 6.0;

/// Default when no Omarchy source yields a usable value.
const DEFAULT_BASE_SIZE: f32 = 12.0;

/// Parsed Omarchy `colors.toml` (required keys always present after `Ok`).
#[derive(Clone, Debug, PartialEq)]
pub struct OsColors {
    pub mode: String,
    pub accent: u32,
    pub selection: u32,
    pub muted: u32,
    pub background: u32,
    pub dark_background: u32,
    pub darker_background: u32,
    pub lighter_background: u32,
    pub foreground: u32,
    pub dark_foreground: u32,
    pub light_foreground: u32,
    pub bright_foreground: u32,
    pub red: u32,
    pub yellow: u32,
    pub green: u32,
    pub cyan: u32,
    pub blue: u32,
    pub magenta: u32,
    pub bright_red: u32,
    pub bright_yellow: u32,
    pub bright_green: u32,
    pub bright_cyan: u32,
    pub bright_blue: u32,
    pub bright_magenta: u32,
    /// Falls back to `bright_yellow` when the key is absent.
    pub orange: u32,
    /// Falls back to `muted` when the key is absent.
    pub brown: u32,
}

/// Latest theme + scale published by the watcher (or the startup fallback).
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeSnapshot {
    pub palette: Palette,
    /// `base_size / 12.0` — multiplies design-time metrics (ROW_H, font px, …).
    pub scale: f32,
    pub generation: u64,
}

/// Cross-thread latest-value slot. UI loads; watcher publishes. No new crates.
pub struct ThemeSlot {
    current: Mutex<Arc<ThemeSnapshot>>,
    generation: AtomicU64,
}

impl ThemeSlot {
    pub(crate) fn new(snapshot: ThemeSnapshot) -> Self {
        let generation = snapshot.generation;
        Self {
            current: Mutex::new(Arc::new(snapshot)),
            generation: AtomicU64::new(generation),
        }
    }

    /// Generation counter (Acquire). Cheap per-frame check.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Clone the latest snapshot.
    pub fn load(&self) -> Arc<ThemeSnapshot> {
        Arc::clone(&self.lock_current())
    }

    fn publish(&self, mut snapshot: ThemeSnapshot) {
        let next = self
            .generation
            .load(Ordering::Relaxed)
            .checked_add(1)
            .expect("fft: theme generation overflow");
        snapshot.generation = next;
        let arc = Arc::new(snapshot);
        *self.lock_current() = Arc::clone(&arc);
        self.generation.store(next, Ordering::Release);
    }

    /// Recover through poison: a dead watcher must not kill the UI on next theme read.
    fn lock_current(&self) -> std::sync::MutexGuard<'_, Arc<ThemeSnapshot>> {
        self.current.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "fft: WARNING theme slot mutex poisoned; recovering last snapshot (watcher died?)"
            );
            poisoned.into_inner()
        })
    }
}

/// Parse Omarchy `colors.toml` text. Missing required keys or malformed hex ⇒ `Err`.
pub fn parse_colors_toml(text: &str) -> Result<OsColors, String> {
    let mut map: Vec<(&str, &str)> = Vec::new();
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // No tables in this schema — reject them loudly.
        if line.starts_with('[') {
            return Err(format!(
                "colors.toml: unexpected table header on line {}: {line}",
                lineno + 1
            ));
        }
        let Some((key, rest)) = line.split_once('=') else {
            return Err(format!(
                "colors.toml: expected key = value on line {}: {line}",
                lineno + 1
            ));
        };
        let key = key.trim();
        let value = strip_quoted(rest.trim())
            .map_err(|e| format!("colors.toml: key `{key}` on line {}: {e}", lineno + 1))?;
        map.push((key, value));
    }

    let req = |key: &str| -> Result<u32, String> {
        let raw = find_key(&map, key)
            .ok_or_else(|| format!("colors.toml: missing required key `{key}`"))?;
        parse_hex(raw).map_err(|e| format!("colors.toml: key `{key}`: {e}"))
    };
    let req_str = |key: &str| -> Result<String, String> {
        find_key(&map, key)
            .map(|s| s.to_string())
            .ok_or_else(|| format!("colors.toml: missing required key `{key}`"))
    };

    let muted = req("muted")?;
    let bright_yellow = req("bright_yellow")?;
    let orange = match find_key(&map, "orange") {
        Some(raw) => parse_hex(raw).map_err(|e| format!("colors.toml: key `orange`: {e}"))?,
        None => bright_yellow,
    };
    let brown = match find_key(&map, "brown") {
        Some(raw) => parse_hex(raw).map_err(|e| format!("colors.toml: key `brown`: {e}"))?,
        None => muted,
    };

    Ok(OsColors {
        mode: req_str("mode")?,
        accent: req("accent")?,
        selection: req("selection")?,
        muted,
        background: req("background")?,
        dark_background: req("dark_background")?,
        darker_background: req("darker_background")?,
        lighter_background: req("lighter_background")?,
        foreground: req("foreground")?,
        dark_foreground: req("dark_foreground")?,
        light_foreground: req("light_foreground")?,
        bright_foreground: req("bright_foreground")?,
        red: req("red")?,
        yellow: req("yellow")?,
        green: req("green")?,
        cyan: req("cyan")?,
        blue: req("blue")?,
        magenta: req("magenta")?,
        bright_red: req("bright_red")?,
        bright_yellow,
        bright_green: req("bright_green")?,
        bright_cyan: req("bright_cyan")?,
        bright_blue: req("bright_blue")?,
        bright_magenta: req("bright_magenta")?,
        orange,
        brown,
    })
}

fn find_key<'a>(map: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    map.iter().rev().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn strip_quoted(value: &str) -> Result<&str, String> {
    let value = value.trim();
    if let Some(inner) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return Ok(inner);
    }
    if let Some(inner) = value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return Ok(inner);
    }
    // Bare tokens (mode = dark) are accepted for robustness.
    if value.is_empty() {
        return Err("empty value".into());
    }
    if value.contains(char::is_whitespace) {
        return Err(format!("unquoted value with whitespace: {value}"));
    }
    Ok(value)
}

/// Parse `"#rrggbb"` or `rrggbb` into a 0xRRGGBB `u32`.
pub fn parse_hex(raw: &str) -> Result<u32, String> {
    let s = raw.trim();
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 {
        return Err(format!("expected 6 hex digits, got `{raw}`"));
    }
    u32::from_str_radix(s, 16).map_err(|_| format!("invalid hex `{raw}`"))
}

/// Extract `base-size` from a shell.toml body. Only the key inside a `[font]`
/// section is considered. Returns `None` when absent; `Err` when present but
/// malformed.
pub fn parse_base_size_from_shell_toml(text: &str) -> Result<Option<f32>, String> {
    let mut in_font = false;
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_font = line.eq_ignore_ascii_case("[font]");
            continue;
        }
        if !in_font {
            continue;
        }
        let Some((key, rest)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "base-size" {
            continue;
        }
        let value = rest.trim().trim_matches('"').trim_matches('\'');
        let n: f32 = value.parse().map_err(|_| {
            format!(
                "shell.toml: invalid base-size on line {}: `{value}`",
                lineno + 1
            )
        })?;
        if !n.is_finite() || n <= 0.0 {
            return Err(format!(
                "shell.toml: base-size must be a positive finite number, got {n}"
            ));
        }
        return Ok(Some(n.max(BASE_SIZE_FLOOR)));
    }
    Ok(None)
}

/// Omarchy `[font] base-size` precedence: user shell.toml → themed shell.toml → 12.
///
/// Parse failures in an *existing* file are loud stderr warnings; the next source
/// is tried. Missing files are silent skips.
pub fn load_base_size() -> f32 {
    load_base_size_from_paths(&user_shell_toml_path(), &themed_shell_toml_path())
}

pub(crate) fn load_base_size_from_paths(user: &Path, themed: &Path) -> f32 {
    for (label, path) in [("user", user), ("themed", themed)] {
        match fs::read_to_string(path) {
            Ok(text) => match parse_base_size_from_shell_toml(&text) {
                Ok(Some(n)) => return n,
                Ok(None) => { /* key absent — try next */ }
                Err(err) => {
                    eprintln!(
                        "fft: WARNING {label} shell.toml base-size parse failed ({}): {err}",
                        path.display()
                    );
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                eprintln!(
                    "fft: WARNING cannot read {label} shell.toml ({}): {err}",
                    path.display()
                );
            }
        }
    }
    DEFAULT_BASE_SIZE
}

/// Resolve the monospace family the same way `omarchy-font-current` does:
/// `fc-match monospace -f '%{family}'`, first comma-separated entry.
///
/// Missing `fc-match` or empty output → loud warning + `"monospace"` fallback.
pub fn resolve_font_family() -> String {
    match Command::new("fc-match")
        .args(["monospace", "-f", "%{family}"])
        .output()
    {
        Ok(out) if out.status.success() => {
            let raw = String::from_utf8_lossy(&out.stdout);
            let family = raw.split(',').next().unwrap_or("").trim().to_string();
            if family.is_empty() {
                eprintln!(
                    "fft: WARNING fc-match returned empty family; falling back to \"monospace\""
                );
                "monospace".into()
            } else {
                family
            }
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            eprintln!(
                "fft: WARNING fc-match failed (status {}); falling back to \"monospace\": {stderr}",
                out.status
            );
            "monospace".into()
        }
        Err(err) => {
            eprintln!("fft: WARNING fc-match not available ({err}); falling back to \"monospace\"");
            "monospace".into()
        }
    }
}

/// Spawn the 500 ms theme poller and return the latest-value slot.
///
/// Startup: missing Omarchy state dir → WARNING + `Palette::mocha()` at scale 1.0
/// (CI / non-Omarchy hosts). The watcher keeps polling so a later-appearing state
/// dir is picked up.
pub fn spawn_theme_watcher() -> Arc<ThemeSlot> {
    let initial = load_theme_snapshot(0);
    let slot = Arc::new(ThemeSlot::new(initial));
    let worker = Arc::clone(&slot);
    if let Err(err) = std::thread::Builder::new()
        .name("fft-os-theme".into())
        .spawn(move || theme_poll_loop(worker))
    {
        // Loud warn + keep static fallback snapshot; UI stays alive without live theme updates.
        eprintln!(
            "fft: WARNING failed to spawn os-theme watcher ({err}); keeping static fallback theme"
        );
    }
    slot
}

fn theme_poll_loop(slot: Arc<ThemeSlot>) {
    let mut last = WatchedMtimes::default();
    // Seed mtimes so the first real change is detected; do not re-publish startup.
    last.capture();
    loop {
        std::thread::sleep(Duration::from_millis(POLL_MS));
        let mut now = WatchedMtimes::default();
        now.capture();
        if now == last {
            continue;
        }
        last = now;
        match try_load_live_snapshot(slot.generation()) {
            Ok(snap) => slot.publish(snap),
            Err(err) => {
                eprintln!("fft: WARNING os theme reload failed (keeping previous snapshot): {err}");
            }
        }
    }
}

fn load_theme_snapshot(generation: u64) -> ThemeSnapshot {
    match try_load_live_snapshot(generation) {
        Ok(snap) => snap,
        Err(err) => {
            // Missing state dir (or unreadable) at startup: documented mocha fallback.
            eprintln!(
                "fft: WARNING Omarchy theme unavailable ({err}); using built-in Palette::mocha() at scale 1.0"
            );
            ThemeSnapshot {
                palette: Palette::mocha(),
                scale: 1.0,
                generation,
            }
        }
    }
}

fn try_load_live_snapshot(generation: u64) -> Result<ThemeSnapshot, String> {
    let state = omarchy_state_dir();
    if !state.is_dir() {
        return Err(format!("Omarchy state dir missing: {}", state.display()));
    }
    let colors_path = theme_colors_path();
    let text = fs::read_to_string(&colors_path)
        .map_err(|e| format!("cannot read colors.toml ({}): {e}", colors_path.display()))?;
    let os = parse_colors_toml(&text)?;
    let base = load_base_size();
    let scale = base / DESIGN_BASE_SIZE;
    Ok(ThemeSnapshot {
        palette: Palette::from_os_colors(&os),
        scale,
        generation,
    })
}

#[derive(Clone, Default, PartialEq, Eq)]
struct WatchedMtimes {
    theme_name: Option<SystemTime>,
    colors: Option<SystemTime>,
    user_shell: Option<SystemTime>,
    themed_shell: Option<SystemTime>,
}

impl WatchedMtimes {
    fn capture(&mut self) {
        self.theme_name = mtime(&theme_name_path());
        self.colors = mtime(&theme_colors_path());
        self.user_shell = mtime(&user_shell_toml_path());
        self.themed_shell = mtime(&themed_shell_toml_path());
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn omarchy_state_dir() -> PathBuf {
    home_dir().join(OMARCHY_STATE)
}

fn theme_name_path() -> PathBuf {
    omarchy_state_dir().join("current/theme.name")
}

fn theme_colors_path() -> PathBuf {
    omarchy_state_dir().join("current/theme/colors.toml")
}

fn themed_shell_toml_path() -> PathBuf {
    omarchy_state_dir().join("current/theme/shell.toml")
}

fn user_shell_toml_path() -> PathBuf {
    home_dir().join(OMARCHY_CONFIG).join("shell.toml")
}

#[cfg(test)]
#[path = "os_theme_tests.rs"]
mod tests;
