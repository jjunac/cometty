# Terminal comparison

Measured 2026-10-27T19:51:42Z on Apple M3 Pro, macOS 26.7.1, AC Power; cometty revision `40cac47`. Raw data: `target/compare/20261027T195140Z`.

Display 120 Hz (max 120 fps), backing scale 2; font Menlo 14; window {'x': 80, 'y': 80, 'w': 1000, 'h': 700}; runs 1.

Host-specific measurements; see docs/performance.md for what each number does and does not include. Medians; ranges are min-max across runs.

| Terminal | Version |
|---|---|
| cometty | 0.1.0 (55) rev 40cac47 |
| terminal | 2.15 (470.2) |
| iterm2 | 3.7.6 (3.7.6) |
| ghostty | 1.3.1 (15212) |
| kitty | 0.49.2 (0.49.2) |

## Grid size

Throughput, latency and memory depend on cols×rows; differences come from each terminal's cell metrics, padding and chrome at the same window size.

| Terminal | cols×rows |
|---|---|
| cometty | 118x37 |
| terminal | 123x38 |
| iterm2 | 123x39 |
| ghostty | 117x40 |
| kitty | 117x40 |

## Startup

Launch command → first on-screen window / → agent process start.

| Terminal | window ms | child ms | window range |
|---|---|---|---|
| cometty | 389 | 462 | 363-792 |
| terminal | 377 | 472 | 376-398 |
| iterm2 | 924 | 913 | 896-2279 |
| ghostty | 419 | 486 | 418-719 |
| kitty | 599 | 699 | 554-1286 |

## Idle (one window, cursor blink off)

| Terminal | CPU % | CPU range | wakeups/s | energy mW |
|---|---|---|---|---|
| cometty | 0.18 | 0.18-0.18 | 36 | 0.07 |
| terminal | 0.05 | 0.05-0.05 | 0 | 0.08 |
| iterm2 | 1.57 | 1.57-1.57 | 4 | 2.26 |
| ghostty | 1.50 | 1.50-1.50 | 150 | 0.74 |
| kitty | 0.15 | 0.15-0.15 | 25 | 0.06 |

## Throughput (DA1-synchronized; parse, not presentation)

MiB/s, median of all samples (higher is better); terminal CPU seconds per MiB in parentheses.

| Terminal | ascii | sgr | unicode | scroll_region | redraw |
|---|---|---|---|---|---|
| cometty | 6.7 (0.156) | 30.2 (0.038) | 1.8 (0.569) | 9.8 (0.108) | 22.7 (0.043) |
| terminal | 38.4 (0.030) | 26.3 (0.052) | 48.9 (0.029) | 40.7 (0.028) | 45.0 (0.026) |
| iterm2 | 83.4 (0.024) | 16.2 (0.150) | 4.0 (0.416) | 6.0 (0.214) | 66.6 (0.036) |
| ghostty | 88.9 (0.015) | 74.8 (0.017) | 22.9 (0.048) | 85.4 (0.015) | 84.4 (0.016) |
| kitty | 155.0 (0.007) | 79.7 (0.014) | 117.7 (0.009) | 179.6 (0.006) | 164.8 (0.007) |

## Memory (phys_footprint, MiB, all terminal-owned processes)

Includes GPU/IOSurface memory attributed to the process. `footprint/` has the per-category breakdown for every checkpoint.

| Terminal | fresh | ascii_10k | sgr_10k | tabs_10_sgr | closed_to_1 | peak |
|---|---|---|---|---|---|---|
| cometty | 53.6 | 58.2 | 84.0 | 313.1 | 84.6 | 474.7 |
| terminal | 43.6 | 43.9 | 67.6 | 645.5 | 140.5 | 804.2 |
| iterm2 | 223.7 | 238.1 | 233.0 | 304.1 | 337.6 | 480.8 |
| ghostty | 157.6 | 180.6 | 187.9 | 778.1 | 523.6 | 802.8 |
| kitty | 136.9 | 173.5 | 174.8 | 509.8 | 176.3 | 617.5 |

## Key → pixel latency (ms)

Post of a synthetic key event → display time of the first captured frame showing the change. Includes capture quantization at the display rate (common to all). `loaded`: another tab floods ASCII output meanwhile. `input` is key post → bytes read by the child.

| Terminal | mode | p50 | p95 | p99 | max | input p50 | misses | n |
|---|---|---|---|---|---|---|---|---|
| cometty | idle | 14.9 | 31.3 | 38.0 | 40.4 | 2.57 | 0 | 200 |
| cometty | loaded | 19.3 | 26.4 | 29.4 | 31.6 | 3.54 | 0 | 200 |
| terminal | idle | 20.4 | 28.5 | 31.4 | 33.1 | 2.48 | 0 | 200 |
| terminal | loaded | 18.6 | 58.1 | 61.5 | 113.9 | 1.22 | 0 | 200 |
| iterm2 | idle | 26.1 | 41.1 | 45.6 | 47.3 | 3.27 | 0 | 200 |
| iterm2 | loaded | 77.1 | 136.2 | 140.2 | 143.7 | 32.62 | 0 | 200 |
| ghostty | idle | 21.9 | 30.0 | 31.8 | 34.3 | 2.89 | 0 | 200 |
| ghostty | loaded | 22.8 | 28.3 | 31.6 | 37.1 | 1.21 | 0 | 200 |
| kitty | idle | 13.3 | 20.9 | 23.2 | 25.1 | 2.81 | 0 | 200 |
| kitty | loaded | 12.2 | 18.0 | 23.3 | 24.8 | 1.72 | 0 | 200 |
