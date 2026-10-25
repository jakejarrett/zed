//! A developer overlay that paints frame-time statistics directly into the
//! scene, bypassing layout, text, and view invalidation entirely (to avoid
//! infinitely triggering new frames).
//!
//! The full mode reads like MangoHUD: frames drawn in the last second, the
//! time between frames and how long the last one took to draw, the worst
//! frame of the last second, and a graph of recent frame times.

use crate::{
    BorderStyle, Bounds, ContentMask, Corners, Edges, Hsla, Pixels, Quad, ScaledPixels, Scene,
    Size, point, rgba, size, transparent_black,
};
use scheduler::Instant;
use std::{collections::VecDeque, time::Duration};

#[allow(missing_docs)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum DebugFrameOverlayMode {
    #[default]
    Hidden,
    Minimal,
    Full,
}

impl DebugFrameOverlayMode {
    /// Returns the next mode in the Hidden → FrameTime → Detailed cycle.
    pub fn next(self) -> Self {
        match self {
            Self::Hidden => Self::Minimal,
            Self::Minimal => Self::Full,
            Self::Full => Self::Hidden,
        }
    }
}

/// The number of most recent draw durations retained for percentile statistics.
const MAX_SAMPLES: usize = 1000;

const GLYPH_WIDTH: usize = 5;
const GLYPH_HEIGHT: usize = 7;
/// Glyph advance and line advance, in font cells.
const CHAR_ADVANCE: f32 = (GLYPH_WIDTH + 1) as f32;
const LINE_ADVANCE: f32 = (GLYPH_HEIGHT + 2) as f32;
/// Padding between the panel edge and the text, in font cells.
const PANEL_PADDING: f32 = 2.0;
/// Margin between the panel and the window corner, in font cells.
const PANEL_MARGIN: f32 = 4.0;
/// Side of one square font cell, in logical pixels.
const CELL_SIZE: f32 = 2.0;

/// Frames graphed, newest on the right, one cell wide each.
const GRAPH_SAMPLES: usize = 120;
/// Graph height in cells, and the frame time at its top.
const GRAPH_HEIGHT: f32 = 30.0;
const GRAPH_CEILING: Duration = Duration::from_millis(50);
/// A gap longer than this is the window resting, not a slow frame: GPUI
/// draws only when something changes, so it starts the frame times afresh.
const IDLE_GAP: Duration = Duration::from_millis(250);
/// 60 and 30 frames a second, marked on the graph.
const SMOOTH: Duration = Duration::from_micros(16_667);
const CHOPPY: Duration = Duration::from_micros(33_333);

fn text_color() -> Hsla {
    rgba(0x33ff33ff).into()
}

fn panel_color() -> Hsla {
    rgba(0x000000aa).into()
}

fn guide_color() -> Hsla {
    rgba(0xffffff33).into()
}

/// A frame time's colour on the graph: green at 60 fps or better, amber to
/// 30, red below.
fn frame_time_color(frame_time: Duration) -> Hsla {
    if frame_time <= SMOOTH {
        rgba(0x33ff33ff).into()
    } else if frame_time <= CHOPPY {
        rgba(0xffcc33ff).into()
    } else {
        rgba(0xff4444ff).into()
    }
}

pub(crate) struct DebugFrameOverlay {
    mode: DebugFrameOverlayMode,
    draw_durations: VecDeque<Duration>,
    total_frame_count: u64,
    /// When each frame of the last second was drawn, for the frame rate.
    frame_instants: VecDeque<Instant>,
    /// The time from each frame to the one before it, idle gaps left out.
    frame_times: VecDeque<Duration>,
}

impl DebugFrameOverlay {
    pub(crate) fn new() -> Self {
        Self {
            mode: DebugFrameOverlayMode::default(),
            draw_durations: VecDeque::new(),
            total_frame_count: 0,
            frame_instants: VecDeque::new(),
            frame_times: VecDeque::new(),
        }
    }

    pub(crate) fn mode(&self) -> DebugFrameOverlayMode {
        self.mode
    }

    pub(crate) fn set_mode(&mut self, mode: DebugFrameOverlayMode) {
        self.mode = mode;
    }

    /// Clears the draw-duration samples, restarting the percentile window.
    /// The total frame count is left untouched.
    pub(crate) fn reset_stats(&mut self) {
        self.draw_durations.clear();
        self.frame_instants.clear();
        self.frame_times.clear();
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.mode != DebugFrameOverlayMode::Hidden
    }

    pub(crate) fn record_frame(&mut self, draw_duration: Duration) {
        self.record_frame_at(draw_duration, Instant::now());
    }

    fn record_frame_at(&mut self, draw_duration: Duration, now: Instant) {
        self.total_frame_count += 1;
        if self.draw_durations.len() >= MAX_SAMPLES {
            self.draw_durations.pop_front();
        }
        self.draw_durations.push_back(draw_duration);

        if let Some(previous) = self.frame_instants.back().copied() {
            let frame_time = now.saturating_duration_since(previous);
            if frame_time <= IDLE_GAP {
                if self.frame_times.len() >= GRAPH_SAMPLES {
                    self.frame_times.pop_front();
                }
                self.frame_times.push_back(frame_time);
            }
        }
        self.frame_instants.push_back(now);
        while self
            .frame_instants
            .front()
            .is_some_and(|first| now.saturating_duration_since(*first) > Duration::from_secs(1))
        {
            self.frame_instants.pop_front();
        }
    }

    /// Frames drawn in the second up to the newest.
    fn frames_per_second(&self) -> usize {
        self.frame_instants.len()
    }

    /// The longest frame time among the last second's frames.
    fn worst_recent_frame_time(&self) -> Option<Duration> {
        let recent = self.frame_instants.len().saturating_sub(1);
        self.frame_times.iter().rev().take(recent).max().copied()
    }

    pub(crate) fn paint(&self, scene: &mut Scene, viewport_size: Size<Pixels>, scale_factor: f32) {
        if !self.is_enabled() {
            return;
        }

        let lines = self.lines();
        let max_line_chars = lines.iter().map(|line| line.len()).max().unwrap_or(0);
        // Ensure at least one physical pixel per cell so the text stays legible
        // at fractional downscale factors.
        let cell = (CELL_SIZE * scale_factor).max(1.0);

        let graphed = self.mode == DebugFrameOverlayMode::Full;
        let text_width = max_line_chars as f32 * CHAR_ADVANCE;
        let inner_width = if graphed {
            text_width.max(GRAPH_SAMPLES as f32)
        } else {
            text_width
        };
        let graph_height = if graphed { GRAPH_HEIGHT + LINE_ADVANCE / 2.0 } else { 0.0 };
        let panel_width = cell * (inner_width + 2.0 * PANEL_PADDING);
        let panel_height =
            cell * (lines.len() as f32 * LINE_ADVANCE + graph_height + 2.0 * PANEL_PADDING);
        let viewport = viewport_size.scale(scale_factor);
        let panel_left = viewport.width.0 - panel_width - cell * PANEL_MARGIN;
        let panel_top = cell * PANEL_MARGIN;

        let content_mask = ContentMask {
            bounds: Bounds {
                origin: point(ScaledPixels(0.), ScaledPixels(0.)),
                size: viewport,
            },
        };

        scene.insert_primitive(solid_quad(
            scaled_bounds(panel_left, panel_top, panel_width, panel_height),
            &content_mask,
            panel_color(),
        ));

        if graphed {
            let graph_left = panel_left + cell * PANEL_PADDING;
            let graph_bottom = panel_top
                + cell * (PANEL_PADDING + lines.len() as f32 * LINE_ADVANCE + graph_height);
            let height_of = |frame_time: Duration| {
                cell * GRAPH_HEIGHT
                    * (frame_time.as_secs_f32() / GRAPH_CEILING.as_secs_f32()).min(1.0)
            };
            for guide in [SMOOTH, CHOPPY] {
                scene.insert_primitive(solid_quad(
                    scaled_bounds(
                        graph_left,
                        graph_bottom - height_of(guide),
                        cell * GRAPH_SAMPLES as f32,
                        (cell / 2.0).max(1.0),
                    ),
                    &content_mask,
                    guide_color(),
                ));
            }
            // Newest on the right, the graph filling from there.
            let first_column = GRAPH_SAMPLES - self.frame_times.len();
            for (index, frame_time) in self.frame_times.iter().enumerate() {
                let height = height_of(*frame_time).max(1.0);
                scene.insert_primitive(solid_quad(
                    scaled_bounds(
                        graph_left + cell * (first_column + index) as f32,
                        graph_bottom - height,
                        cell,
                        height,
                    ),
                    &content_mask,
                    frame_time_color(*frame_time),
                ));
            }
        }

        let text_color = text_color();
        for (line_index, line) in lines.iter().enumerate() {
            let line_top = panel_top + cell * (PANEL_PADDING + line_index as f32 * LINE_ADVANCE);
            for (char_index, character) in line.chars().enumerate() {
                let Some(rows) = glyph(character) else {
                    continue;
                };
                let glyph_left =
                    panel_left + cell * (PANEL_PADDING + char_index as f32 * CHAR_ADVANCE);
                for (row_index, row) in rows.iter().enumerate() {
                    let row_top = line_top + cell * row_index as f32;
                    // Merge horizontal runs of lit cells into single quads.
                    let mut column = 0;
                    while column < GLYPH_WIDTH {
                        if row & (1 << (GLYPH_WIDTH - 1 - column)) == 0 {
                            column += 1;
                            continue;
                        }
                        let run_start = column;
                        while column < GLYPH_WIDTH && row & (1 << (GLYPH_WIDTH - 1 - column)) != 0 {
                            column += 1;
                        }
                        scene.insert_primitive(solid_quad(
                            scaled_bounds(
                                glyph_left + cell * run_start as f32,
                                row_top,
                                cell * (column - run_start) as f32,
                                cell,
                            ),
                            &content_mask,
                            text_color,
                        ));
                    }
                }
            }
        }
    }

    fn lines(&self) -> Vec<String> {
        let current = self.draw_durations.back().copied();
        match self.mode {
            DebugFrameOverlayMode::Hidden => Vec::new(),
            DebugFrameOverlayMode::Minimal => vec![format!(
                "FPS {:>3} {}",
                self.frames_per_second(),
                format_ms(self.frame_times.back().copied())
            )],
            DebugFrameOverlayMode::Full => {
                let mut sorted: Vec<Duration> = self.draw_durations.iter().copied().collect();
                sorted.sort_unstable();
                let percentile = |numerator: usize| {
                    (!sorted.is_empty()).then(|| sorted[(sorted.len() - 1) * numerator / 100])
                };
                // Past five digits the count would break the column
                // alignment, so it saturates instead.
                let frame_count = if self.total_frame_count > 99_999 {
                    "LOTS".to_string()
                } else {
                    self.total_frame_count.to_string()
                };
                // Labels are padded to a uniform width so the fixed-width
                // readouts start in the same column on every line.
                vec![
                    format!("FPS     {:>5}", self.frames_per_second()),
                    format!("FRAME  {}", format_ms(self.frame_times.back().copied())),
                    format!("WORST  {}", format_ms(self.worst_recent_frame_time())),
                    format!("CPU    {}", format_ms(current)),
                    format!("CPU 1% {}", format_ms(percentile(99))),
                    format!("CPU MX {}", format_ms(sorted.last().copied())),
                    format!("FRAMES {frame_count:>5}"),
                ]
            }
        }
    }
}

/// Formats as `abc.d MS`, right-aligned in room for three integer digits and
/// one decimal (padded with spaces, not zeroes), so stacked readouts align.
fn format_ms(duration: Option<Duration>) -> String {
    match duration {
        Some(duration) => {
            let ms = duration.as_secs_f32() * 1000.0;
            format!("{ms:>5.1} MS")
        }
        None => "   -- MS".into(),
    }
}

fn scaled_bounds(left: f32, top: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
    Bounds {
        origin: point(ScaledPixels(left), ScaledPixels(top)),
        size: size(ScaledPixels(width), ScaledPixels(height)),
    }
}

fn solid_quad(
    bounds: Bounds<ScaledPixels>,
    content_mask: &ContentMask<ScaledPixels>,
    color: Hsla,
) -> Quad {
    Quad {
        order: 0,
        border_style: BorderStyle::Solid,
        bounds,
        content_mask: *content_mask,
        background: color.into(),
        border_color: transparent_black(),
        corner_radii: Corners::default(),
        border_widths: Edges::default(),
    }
}

/// Returns the 5x7 bitmap for the given character, one `u8` of column bits per
/// row with the most significant of the 5 bits leftmost. Only the characters
/// used by the overlay's readouts are defined.
fn glyph(character: char) -> Option<[u8; GLYPH_HEIGHT]> {
    Some(match character {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        '%' => [
            0b11001, 0b11001, 0b00010, 0b00100, 0b01000, 0b10011, 0b10011,
        ],
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'C' => [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character the readouts can produce must have a glyph, or it
    /// would silently render as blank space.
    #[test]
    fn all_rendered_characters_have_glyphs() {
        let mut overlay = DebugFrameOverlay::new();
        overlay.set_mode(DebugFrameOverlayMode::Full);

        let mut lines = Vec::new();
        for duration in [
            Duration::ZERO,
            Duration::from_micros(1),
            Duration::from_micros(8_333),
            Duration::from_millis(123),
            Duration::from_secs(2),
        ] {
            overlay.record_frame(duration);
            lines.extend(overlay.lines());
        }
        // Counts beyond five digits render as "LOTS".
        overlay.total_frame_count = 100_000;
        lines.extend(overlay.lines());

        // An enabled overlay with no samples yet renders placeholders.
        let mut empty = DebugFrameOverlay::new();
        empty.set_mode(DebugFrameOverlayMode::Full);
        lines.extend(empty.lines());

        for line in lines {
            for character in line.chars() {
                assert!(
                    character == ' ' || glyph(character).is_some(),
                    "no glyph for {character:?} in line {line:?}"
                );
            }
        }
    }

    /// Frames 16 ms apart, starting at `start`, each taking `draw` to draw.
    fn record_paced(overlay: &mut DebugFrameOverlay, start: Instant, frames: u32, draw: Duration) {
        for frame in 0..frames {
            overlay.record_frame_at(draw, start + Duration::from_millis(16) * frame);
        }
    }

    #[test]
    fn reads_frame_rate_and_frame_time_like_mangohud() {
        let mut overlay = DebugFrameOverlay::new();
        overlay.set_mode(DebugFrameOverlayMode::Full);
        let start = Instant::now();
        // 63 frames 16 ms apart but for one slow one of 40 ms: 1,016 ms in
        // all, so the first has left the last second.
        record_paced(&mut overlay, start, 40, Duration::from_millis(5));
        let slow = start + Duration::from_millis(16 * 39 + 40);
        overlay.record_frame_at(Duration::from_millis(30), slow);
        record_paced(&mut overlay, slow + Duration::from_millis(16), 22, Duration::from_millis(5));
        let lines = overlay.lines();
        assert_eq!(lines[0], "FPS        62");
        assert_eq!(lines[1], "FRAME   16.0 MS");
        assert_eq!(lines[2], "WORST   40.0 MS");
        assert_eq!(lines[3], "CPU      5.0 MS");
        assert_eq!(lines[5], "CPU MX  30.0 MS");
        assert_eq!(lines[6], "FRAMES    63");
    }

    #[test]
    fn an_idle_gap_is_not_a_slow_frame() {
        let mut overlay = DebugFrameOverlay::new();
        overlay.set_mode(DebugFrameOverlayMode::Full);
        let start = Instant::now();
        record_paced(&mut overlay, start, 3, Duration::from_millis(5));
        // Nothing changed for two seconds, then two frames.
        let later = start + Duration::from_secs(2);
        record_paced(&mut overlay, later, 2, Duration::from_millis(5));
        let lines = overlay.lines();
        assert_eq!(lines[0], "FPS         2");
        assert_eq!(lines[2], "WORST   16.0 MS");
        assert!(overlay.frame_times.iter().all(|time| *time <= IDLE_GAP));
    }

    #[test]
    fn the_graph_keeps_the_newest_frames() {
        let mut overlay = DebugFrameOverlay::new();
        record_paced(&mut overlay, Instant::now(), 500, Duration::from_millis(1));
        assert_eq!(overlay.frame_times.len(), GRAPH_SAMPLES);
    }

    #[test]
    fn reset_clears_durations_but_keeps_frame_count() {
        let mut overlay = DebugFrameOverlay::new();
        overlay.set_mode(DebugFrameOverlayMode::Full);
        record_paced(&mut overlay, Instant::now(), 10, Duration::from_millis(10));
        overlay.reset_stats();
        let lines = overlay.lines();
        assert_eq!(lines[0], "FPS         0");
        assert_eq!(lines[3], "CPU       -- MS");
        assert_eq!(lines[6], "FRAMES    10");
    }

    #[test]
    fn frame_count_is_right_aligned_and_saturates() {
        let mut overlay = DebugFrameOverlay::new();
        overlay.set_mode(DebugFrameOverlayMode::Full);
        overlay.record_frame(Duration::from_millis(10));
        assert_eq!(overlay.lines()[6], "FRAMES     1");
        overlay.total_frame_count = 99_999;
        assert_eq!(overlay.lines()[6], "FRAMES 99999");
        overlay.total_frame_count = 100_000;
        assert_eq!(overlay.lines()[6], "FRAMES  LOTS");
    }

    #[test]
    fn the_minimal_mode_is_one_line_of_rate_and_frame_time() {
        let mut overlay = DebugFrameOverlay::new();
        record_paced(&mut overlay, Instant::now(), 2, Duration::from_millis(3));
        overlay.set_mode(DebugFrameOverlayMode::Minimal);
        assert_eq!(overlay.lines(), vec!["FPS   2  16.0 MS".to_string()]);
    }
}
