//! Shared grid-size math (single source of truth for resize clamps).
//!
//! Fixes the initial-vs-resize clamp mismatch: both paths now use
//! [`MAX_DIM`] via [`compute_grid_size`].

/// Maximum grid dimension (cols/rows). Used by initial sizing,
/// resize handling, and PTY sizing so all three agree.
pub const MAX_DIM: usize = 1024;

/// Minimum grid dimension (cols/rows).
pub const MIN_DIM: usize = 1;

/// Compute `(cols, rows)` from physical size + cell metrics, clamped to
/// `MIN_DIM..=MAX_DIM`.
pub fn compute_grid_size(
    width: u32,
    height: u32,
    cell_width: f32,
    line_height: f32,
) -> (usize, usize) {
    let cols = if cell_width > 0.0 && cell_width.is_finite() {
        (width as f32 / cell_width).floor() as usize
    } else {
        MIN_DIM
    };
    let rows = if line_height > 0.0 && line_height.is_finite() {
        (height as f32 / line_height).floor() as usize
    } else {
        MIN_DIM
    };
    (cols.clamp(MIN_DIM, MAX_DIM), rows.clamp(MIN_DIM, MAX_DIM))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_to_min_on_zero_size() {
        assert_eq!(compute_grid_size(0, 0, 8.0, 16.0), (1, 1));
    }

    #[test]
    fn clamps_to_max_dim() {
        // Huge window with tiny cells would exceed the PTY limit without clamping.
        assert_eq!(compute_grid_size(100_000, 100_000, 1.0, 1.0), (1024, 1024));
    }

    #[test]
    fn typical_size_passes_through() {
        // 800px / 8.428px ≈ 94 cols, 600px / 17.5px ≈ 34 rows.
        let (cols, rows) = compute_grid_size(800, 600, 8.428, 17.5);
        assert_eq!((cols, rows), (94, 34));
    }

    #[test]
    fn degenerate_metrics_fall_back_to_min() {
        assert_eq!(compute_grid_size(800, 600, 0.0, 0.0), (1, 1));
        assert_eq!(compute_grid_size(800, 600, f32::NAN, f32::INFINITY), (1, 1));
    }
}
