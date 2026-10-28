# Terminal comparison

Measured 2026-10-28T09:29:02Z on Apple M3 Pro, macOS 26.7.1, AC Power; cometty revision `905a812`. Raw data: `target/compare/20261028T092848Z`.

Display 120 Hz (max 120 fps), backing scale 2; font Menlo 14; window {'x': 80, 'y': 80, 'w': 1000, 'h': 700}; runs 1.

Host-specific measurements; see docs/performance.md for what each number does and does not include. Medians; ranges are min-max across runs.

| Terminal | Version |
|---|---|
| cometty | 0.1.0 (57) rev 905a812 |
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
| cometty | 483 | 552 | 443-811 |
| terminal | 483 | 543 | 465-511 |
| iterm2 | 990 | 933 | 978-2258 |
| ghostty | 530 | 555 | 511-812 |
| kitty | 623 | 684 | 598-1357 |

## Idle (one window, cursor blink off)

| Terminal | CPU % | CPU range | wakeups/s | energy mW |
|---|---|---|---|---|
| cometty | 0.21 | 0.21-0.21 | 36 | 0.17 |
| terminal | 0.06 | 0.06-0.06 | 0 | 0.07 |
| iterm2 | 1.53 | 1.53-1.53 | 4 | 2.09 |
| ghostty | 1.41 | 1.41-1.41 | 146 | 0.99 |
| kitty | 0.16 | 0.16-0.16 | 21 | 0.06 |

## Throughput (DA1-synchronized; parse, not presentation)

MiB/s, median of all samples (higher is better); terminal CPU seconds per MiB in parentheses.

| Terminal | ascii | sgr | unicode | scroll_region | redraw |
|---|---|---|---|---|---|
| cometty | 39.9 (0.029) | 135.8 (0.012) | 25.8 (0.043) | 64.1 (0.019) | 81.1 (0.016) |
| terminal | 38.1 (0.030) | 25.8 (0.053) | 49.1 (0.028) | 40.5 (0.028) | 43.0 (0.027) |
| iterm2 | 81.7 (0.025) | 16.5 (0.150) | 4.0 (0.424) | 6.0 (0.215) | 67.0 (0.037) |
| ghostty | 85.2 (0.016) | 74.3 (0.018) | 22.2 (0.050) | 82.5 (0.017) | 84.6 (0.017) |
| kitty | 152.4 (0.008) | 80.3 (0.014) | 115.4 (0.010) | 183.8 (0.006) | 164.9 (0.007) |

## Memory (phys_footprint, MiB, all terminal-owned processes)

Includes GPU/IOSurface memory attributed to the process. `footprint/` has the per-category breakdown for every checkpoint.

| Terminal | fresh | ascii_10k | sgr_10k | tabs_10_sgr | closed_to_1 | peak |
|---|---|---|---|---|---|---|
| cometty | 57.0 | 59.2 | 85.0 | 317.0 | 85.9 | 477.4 |
| terminal | 44.2 | 44.4 | 69.3 | 642.7 | 76.9 | 819.7 |
| iterm2 | 227.0 | 241.1 | 237.2 | 309.2 | 289.0 | 429.7 |
| ghostty | 159.0 | 182.1 | 189.4 | 780.9 | 590.8 | 806.2 |
| kitty | 139.5 | 176.2 | 177.2 | 572.5 | 178.8 | 620.7 |

## Key → pixel latency (ms)

Post of a synthetic key event → display time of the first captured frame showing the change. Includes capture quantization at the display rate (common to all). `loaded`: another tab floods ASCII output meanwhile. `input` is key post → bytes read by the child.

| Terminal | mode | p50 | p95 | p99 | max | input p50 | misses | n |
|---|---|---|---|---|---|---|---|---|
| cometty | idle | 13.5 | 19.9 | 22.3 | 29.1 | 2.20 | 0 | 200 |
| cometty | loaded | 19.1 | 27.4 | 32.2 | 32.5 | 3.76 | 0 | 200 |
| terminal | idle | 18.7 | 24.1 | 28.6 | 30.2 | 1.97 | 0 | 200 |
| terminal | loaded | 18.6 | 57.0 | 59.1 | 63.1 | 1.46 | 0 | 200 |
| iterm2 | idle | 26.4 | 39.1 | 46.0 | 47.5 | 3.13 | 0 | 200 |
| iterm2 | loaded | 76.5 | 136.7 | 143.9 | 177.5 | 32.64 | 0 | 200 |
| ghostty | idle | 21.0 | 29.1 | 32.3 | 36.1 | 2.73 | 0 | 200 |
| ghostty | loaded | 21.2 | 27.5 | 31.8 | 33.0 | 1.20 | 0 | 200 |
| kitty | idle | 11.2 | 17.4 | 23.4 | 26.1 | 2.08 | 0 | 200 |
| kitty | loaded | 11.2 | 17.3 | 23.0 | 23.3 | 1.03 | 0 | 200 |
