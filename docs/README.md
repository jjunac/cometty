# Documentation

The root [README](../README.md) is for people using cometty. This directory
contains the configuration reference and the technical documentation for
maintaining it.

## Using cometty

- [Configuration](configuration.md) — file location, defaults, shortcuts, themes,
  tab labels, clipboard permissions, and logging options.

## Developing cometty

| Document | Read it when working on… |
| --- | --- |
| [Development](development.md) | Building, CI checks, test coverage, or manual verification |
| [Architecture](architecture.md) | Ownership, event flow, scheduling, settings persistence, or logging |
| [Terminal model](terminal.md) | Parsing, Unicode cells, row identity, scrollback, or resize semantics |
| [PTY transport and lifecycle](pty.md) | Backpressure, input admission, fair draining, or process shutdown |
| [Rendering](rendering.md) | Shaping, cache invalidation, pixel geometry, or GPU resource lifetimes |
| [Interaction](interaction.md) | Keyboard/IME focus, search, selection, links, or tab labels |
| [Memory](memory.md) | Storage budgets, retained capacity, known gaps, or memory diagnostics |
| [Performance](performance.md) | Release benchmarks, phase counters, profiling, or interpreting results |
| [Idle CPU and energy](idle.md) | Idle wakeups, frame sources, per-frame costs, and the plan to reduce them |
| [Throughput fixes and diagnosis](throughput.md) | Shipped fixes, same-host validation, remaining limits, and the preserved investigation baseline |
| [Roadmap](roadmap.md) | Planned features and proposed designs (not yet implemented) |

## Keeping these documents useful

These pages describe the current implementation, its rationale, and its
invariants—not a chronological optimization diary. Source files and tests are
linked by path rather than changeable line numbers. Update the relevant page
when a decision, limit, cache key, or lifecycle changes; keep proposed work
explicitly separate from implemented behavior.

Historical studies and before/after timing tables are not current guarantees.
Use Git history for that investigation record and the committed harness for new
measurements. Benchmark artifacts under `target/` are disposable, not prerequisites
for understanding or validating the design.
