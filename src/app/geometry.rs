//! Shared grid-size math (single source of truth for resize clamps).
//!
//! Bounds come from [`crate::config::TerminalConfig`] so all resize/PTY
//! paths agree. See [`compute_grid_size`].

use crate::config::TerminalConfig;

/// Compute `(cols, rows)` from physical size + cell metrics, clamped to
/// `config.min_dim..=config.max_dim`.
pub fn compute_grid_size(
    width: u32,
    height: u32,
    cell_width: f32,
    line_height: f32,
    config: &TerminalConfig,
) -> (usize, usize) {
    let min_dim = config.min_dim.max(1);
    let cols = if cell_width > 0.0 && cell_width.is_finite() {
        (width as f32 / cell_width).floor() as usize
    } else {
        min_dim
    };
    let rows = if line_height > 0.0 && line_height.is_finite() {
        (height as f32 / line_height).floor() as usize
    } else {
        min_dim
    };
    clamp_grid_size(cols, rows, config.min_dim, config.max_dim, config.max_cells)
}

/// Shared by window geometry, Grid and PTY. Multiplication never precedes
/// validation, and even unsanitized GUI/headless configs obey hard ceilings.
pub fn clamp_grid_size(
    cols: usize,
    rows: usize,
    min: usize,
    max: usize,
    cells: usize,
) -> (usize, usize) {
    let cells = cells.clamp(1, 1024 * 1024);
    let max = max.max(min).clamp(1, u16::MAX as usize);
    let min = min.max(1).min(max).min(cells.isqrt());
    let mut cols = cols.clamp(min, max);
    let mut rows = rows.clamp(min, max);
    // Preserve the requested aspect while enforcing the total-cell budget:
    // clipping only rows would turn an oversized square window into a flat
    // 1024x9 strip. Integer trimming handles float rounding at the edge.
    if cols.saturating_mul(rows) > cells {
        let scale = (cells as f64 / (cols as f64 * rows as f64)).sqrt();
        cols = ((cols as f64 * scale).floor() as usize).clamp(min, max);
        rows = ((rows as f64 * scale).floor() as usize).clamp(min, max);
        cols = cols.min(cells / min);
        rows = rows.min(cells / cols);
        while cols.saturating_mul(rows) > cells {
            if cols > min && cols >= rows {
                cols -= 1;
            } else if rows > min {
                rows -= 1;
            } else {
                break;
            }
        }
    }
    (cols, rows)
}

/// True when the window is drawn at — or about to be drawn at — the exact
/// backing size of its display: native fullscreen, or a window sized to
/// cover the screen.
///
/// That exact size is what triggers the macOS/Metal fullscreen stripe bug,
/// so the renderer shrinks the surface by a pixel there (see
/// `renderer::shrink_fullscreen_surface`). The window/monitor sizes can
/// disagree for a frame while AppKit animates a fullscreen transition, so
/// the fullscreen flag is consulted as well instead of trusting the size
/// comparison alone.
pub fn is_fullscreen_like(window: &winit::window::Window, width: u32, height: u32) -> bool {
    if window.fullscreen().is_some() {
        return true;
    }
    window.current_monitor().is_some_and(|monitor| {
        let size = monitor.size();
        size.width == width && size.height == height
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TerminalConfig;

    fn cfg() -> TerminalConfig {
        TerminalConfig::default()
    }

    #[test]
    fn clamps_to_min_on_zero_size() {
        assert_eq!(compute_grid_size(0, 0, 8.0, 16.0, &cfg()), (1, 1));
    }

    #[test]
    fn clamps_to_max_dim() {
        // Huge window with tiny cells would exceed the PTY limit without clamping.
        assert_eq!(
            compute_grid_size(100_000, 100_000, 1.0, 1.0, &cfg()),
            (1024, 1024)
        );
    }

    #[test]
    fn typical_size_passes_through() {
        // 800px / 8.428px ≈ 94 cols, 600px / 17.5px ≈ 34 rows.
        let (cols, rows) = compute_grid_size(800, 600, 8.428, 17.5, &cfg());
        assert_eq!((cols, rows), (94, 34));
    }

    #[test]
    fn degenerate_metrics_fall_back_to_min() {
        assert_eq!(compute_grid_size(800, 600, 0.0, 0.0, &cfg()), (1, 1));
        assert_eq!(
            compute_grid_size(800, 600, f32::NAN, f32::INFINITY, &cfg()),
            (1, 1)
        );
    }

    #[test]
    fn total_cell_and_pty_limits_override_per_axis_dimensions() {
        let mut c = cfg();
        c.max_cells = 10_000;
        let (cols, rows) = compute_grid_size(100_000, 100_000, 1.0, 1.0, &c);
        assert!(cols * rows <= 10_000);
        assert_eq!((cols, rows), (100, 100));
        let mut large = cfg();
        large.max_dim = 100_000;
        large.min_dim = 1;
        large.max_cells = 1024 * 1024;
        let (cols, rows) = compute_grid_size(100_000, 15, 1.0, 1.0, &large);
        assert!(cols * rows <= 1024 * 1024);
        assert_eq!((cols, rows), (u16::MAX as usize, 15));
        // A minimum dimension larger than the total budget cannot multiply
        // past the hard cap.
        large.min_dim = 1000;
        large.max_cells = 100;
        let (cols, rows) = compute_grid_size(100_000, 100_000, 1.0, 1.0, &large);
        assert!(cols * rows <= 100 && cols >= 1 && rows >= 1);
    }
}
