# Terminal comparison

Measured 2026-10-29T23:05:48Z on Apple M3 Pro, macOS 26.7.1, AC Power; cometty revision `a6db46e`. Raw data: `target/compare/20261029T230539Z`.

Display 120 Hz (max 120 fps), backing scale 2; font Menlo 14; window {'x': 80, 'y': 80, 'w': 1000, 'h': 700}; runs 1.

Host-specific measurements; see docs/performance.md for what each number does and does not include. Medians; ranges are min-max across runs.

| Terminal | Version |
|---|---|
| cometty | 0.1.1 (67) rev a6db46e |
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
| cometty | 435 | 490 | 391-1000 |
| terminal | 407 | 468 | 393-593 |
| iterm2 | 928 | 864 | 911-1282 |
| ghostty | 411 | 452 | 391-680 |
| kitty | 624 | 686 | 554-1308 |

## Idle (one window, cursor blink off)

| Terminal | CPU % | CPU range | wakeups/s | energy mW |
|---|---|---|---|---|
| cometty | 0.02 | 0.02-0.02 | 0 | 0.01 |
| terminal | 0.05 | 0.05-0.05 | 0 | 0.10 |
| iterm2 | 1.38 | 1.38-1.38 | 5 | 1.61 |
| ghostty | 1.60 | 1.60-1.60 | 140 | 0.68 |
| kitty | 0.16 | 0.16-0.16 | 20 | 0.05 |

## Throughput (DA1-synchronized; parse, not presentation)

MiB/s, median of all samples (higher is better); terminal CPU seconds per MiB in parentheses.

| Terminal | ascii | sgr | unicode | scroll_region | redraw |
|---|---|---|---|---|---|
| cometty | 41.8 (0.028) | 134.2 (0.012) | 27.3 (0.041) | 65.8 (0.019) | 88.5 (0.015) |
| terminal | 38.7 (0.029) | 26.8 (0.052) | 49.2 (0.029) | 40.8 (0.028) | 43.3 (0.027) |
| iterm2 | 91.3 (0.021) | 15.6 (0.129) | 4.1 (0.386) | 6.3 (0.194) | 81.7 (0.027) |
| ghostty | 89.1 (0.015) | 74.5 (0.017) | 22.9 (0.048) | 86.4 (0.015) | 85.5 (0.015) |
| kitty | 154.1 (0.007) | 83.2 (0.014) | 117.9 (0.009) | 183.9 (0.006) | 166.8 (0.007) |

## Memory (phys_footprint, MiB, all terminal-owned processes)

Includes GPU/IOSurface memory attributed to the process. `footprint/` has the per-category breakdown for every checkpoint.

| Terminal | fresh | ascii_10k | sgr_10k | tabs_10_sgr | closed_to_1 | peak |
|---|---|---|---|---|---|---|
| cometty | 60.0 | 62.7 | 92.1 | 331.0 | 330.6 | 494.3 |
| terminal | 52.4 | 52.9 | 87.8 | 708.0 | 195.1 | 856.9 |
| iterm2 | 255.6 | 281.9 | 319.3 | 430.7 | 484.2 | 535.7 |
| ghostty | 165.8 | 188.1 | 195.6 | 791.8 | 520.6 | 819.5 |
| kitty | 151.8 | 188.5 | 191.4 | 532.3 | 209.9 | 639.9 |

## Key → pixel latency (ms)

Post of a synthetic key event → display time of the first captured frame showing the change. Includes capture quantization at the display rate (common to all). `loaded`: another tab floods ASCII output meanwhile. `input` is key post → bytes read by the child.

| Terminal | mode | p50 | p95 | p99 | max | input p50 | misses | n |
|---|---|---|---|---|---|---|---|---|
| cometty | idle | 13.9 | 25.7 | 32.1 | 39.1 | 2.46 | 0 | 200 |
| cometty | loaded | 20.9 | 26.9 | 29.6 | 32.9 | 3.86 | 0 | 200 |
| terminal | idle | 20.9 | 27.8 | 28.4 | 29.1 | 2.79 | 0 | 200 |
| terminal | loaded | 19.1 | 58.6 | 62.4 | 63.7 | 1.23 | 0 | 200 |
| iterm2 | idle | 28.6 | 43.2 | 46.5 | 49.6 | 4.49 | 0 | 200 |
| iterm2 | loaded | 76.7 | 136.2 | 168.6 | 169.8 | 34.17 | 0 | 200 |
| ghostty | idle | 23.0 | 29.9 | 33.3 | 36.6 | 3.09 | 0 | 200 |
| ghostty | loaded | 22.5 | 28.8 | 30.1 | 32.4 | 1.09 | 0 | 200 |
| kitty | idle | 13.9 | 21.6 | 25.4 | 26.8 | 2.77 | 0 | 200 |
| kitty | loaded | 12.5 | 18.7 | 20.2 | 22.5 | 1.58 | 0 | 200 |
