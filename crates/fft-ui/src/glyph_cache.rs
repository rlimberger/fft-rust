//! Persistent shaped-line cache for custom pane elements.

use std::collections::{HashMap, hash_map::Entry};

use gpui::{Hsla, Pixels, ShapedLine, SharedString, TextRun, Window};

/// Number of frames between stale-entry sweeps.
pub const SWEEP_INTERVAL_FRAMES: u16 = 1024;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct GlyphKey {
    text: SharedString,
    color_bits: [u32; 4],
    font_size_bits: u32,
}

impl GlyphKey {
    fn new(text: SharedString, color: Hsla, font_size: Pixels) -> Self {
        Self {
            text,
            color_bits: [
                color.h.to_bits(),
                color.s.to_bits(),
                color.l.to_bits(),
                color.a.to_bits(),
            ],
            font_size_bits: font_size.as_f32().to_bits(),
        }
    }
}

#[derive(Debug)]
struct CachedLine {
    line: ShapedLine,
    last_used_generation: u64,
}

/// Cache shared by the shell and borrowed by pane prepaint code.
///
/// Call [`Self::begin_frame`] once before a frame's lookups. On the first sweep,
/// entries used since cache creation are retained. Each later sweep retains only
/// entries used after the preceding sweep. Hit and miss counters are cumulative
/// and are not reset by sweeping.
#[derive(Debug, Default)]
pub struct GlyphCache {
    entries: HashMap<GlyphKey, CachedLine>,
    generation: u64,
    frames_since_sweep: u16,
    hits: u64,
    misses: u64,
}

impl GlyphCache {
    /// Marks the start of a frame and performs the 1024-frame generation sweep.
    pub fn begin_frame(&mut self) {
        self.frames_since_sweep += 1;
        if self.frames_since_sweep != SWEEP_INTERVAL_FRAMES {
            return;
        }

        self.frames_since_sweep = 0;
        self.entries
            .retain(|_, entry| entry.last_used_generation == self.generation);
        self.generation = self
            .generation
            .checked_add(1)
            .expect("fft: glyph-cache generation overflow");
    }

    /// Returns a shaped line, shaping it only when its exact visual key misses.
    pub fn get_or_shape(
        &mut self,
        window: &mut Window,
        text: impl Into<SharedString>,
        color: Hsla,
        font_size: Pixels,
    ) -> ShapedLine {
        let text = text.into();
        let key = GlyphKey::new(text.clone(), color, font_size);
        self.lookup_or_insert_with(key, || {
            let style = window.text_style();
            let run = TextRun {
                len: text.len(),
                font: style.font(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            window
                .text_system()
                .shape_line(text, font_size, &[run], None)
        })
    }

    /// Number of cached visual keys.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache currently has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Cumulative exact-key hits since construction.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Cumulative exact-key misses since construction.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    fn lookup_or_insert_with(
        &mut self,
        key: GlyphKey,
        shape: impl FnOnce() -> ShapedLine,
    ) -> ShapedLine {
        match self.entries.entry(key) {
            Entry::Occupied(mut occupied) => {
                self.hits += 1;
                occupied.get_mut().last_used_generation = self.generation;
                occupied.get().line.clone()
            }
            Entry::Vacant(vacant) => {
                self.misses += 1;
                let line = shape();
                vacant.insert(CachedLine {
                    line: line.clone(),
                    last_used_generation: self.generation,
                });
                line
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gpui::{Hsla, ShapedLine, px};

    use super::{GlyphCache, GlyphKey, SWEEP_INTERVAL_FRAMES};

    fn key(text: &str) -> GlyphKey {
        GlyphKey::new(text.into(), Hsla::default(), px(12.0))
    }

    fn visual_key(text: &str, color: [f32; 4], size: f32) -> GlyphKey {
        GlyphKey::new(
            text.into(),
            Hsla {
                h: color[0],
                s: color[1],
                l: color[2],
                a: color[3],
            },
            px(size),
        )
    }

    fn advance_to_sweep(cache: &mut GlyphCache) {
        for _ in 0..SWEEP_INTERVAL_FRAMES {
            cache.begin_frame();
        }
    }

    #[test]
    fn exact_key_hit_does_not_shape_again() {
        let shapes = Cell::new(0);
        let mut cache = GlyphCache::default();

        for _ in 0..100 {
            cache.lookup_or_insert_with(key("42"), || {
                shapes.set(shapes.get() + 1);
                ShapedLine::default()
            });
        }

        assert_eq!(shapes.get(), 1);
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 99);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn key_uses_text_all_color_component_bits_and_font_size_bits() {
        let base = visual_key("x", [0.1, 0.2, 0.3, 0.4], 12.0);
        let variants = [
            visual_key("y", [0.1, 0.2, 0.3, 0.4], 12.0),
            visual_key("x", [0.5, 0.2, 0.3, 0.4], 12.0),
            visual_key("x", [0.1, 0.5, 0.3, 0.4], 12.0),
            visual_key("x", [0.1, 0.2, 0.5, 0.4], 12.0),
            visual_key("x", [0.1, 0.2, 0.3, 0.5], 12.0),
            visual_key("x", [0.1, 0.2, 0.3, 0.4], 13.0),
        ];

        for variant in variants {
            assert_ne!(base, variant);
        }

        let positive_zero = GlyphKey::new("x".into(), Hsla::default(), px(0.0));
        let negative_zero = GlyphKey::new("x".into(), Hsla::default(), px(-0.0));
        assert_ne!(positive_zero, negative_zero);
    }

    #[test]
    fn first_sweep_retains_entries_used_since_creation() {
        let mut cache = GlyphCache::default();
        cache.lookup_or_insert_with(key("old"), ShapedLine::default);

        advance_to_sweep(&mut cache);
        assert_eq!(cache.len(), 1);

        advance_to_sweep(&mut cache);
        assert!(cache.is_empty());
    }

    #[test]
    fn later_sweep_retains_only_entries_used_since_preceding_sweep() {
        let mut cache = GlyphCache::default();
        cache.lookup_or_insert_with(key("hot"), ShapedLine::default);
        advance_to_sweep(&mut cache);

        cache.lookup_or_insert_with(key("hot"), || unreachable!());
        advance_to_sweep(&mut cache);
        assert_eq!(cache.len(), 1);

        advance_to_sweep(&mut cache);
        assert!(cache.is_empty());
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.misses(), 1);
    }

    #[test]
    fn shaped_line_can_be_cloned_and_cross_thread_boundaries() {
        fn assert_clone_send_sync<T: Clone + Send + Sync>() {}

        assert_clone_send_sync::<ShapedLine>();
    }
}
