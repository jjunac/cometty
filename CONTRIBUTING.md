# Contributing to cometty

Thanks for your interest in contributing to cometty! This page is the starting
point for building from source, running checks, and navigating the codebase.

## Building from source

cometty is a single-binary Rust crate (edition 2024, MSRV **1.89**).

- **macOS:** Uses the installed system frameworks and a current stable
  [Rust toolchain](https://rustup.rs/) with `clippy` and `rustfmt`.
- **Debian / Ubuntu:** Install the GUI development libraries first:

```sh
sudo apt-get install -y libx11-dev libxkbcommon-dev libgl1-mesa-dev
```

Build, install, or run from a checkout:

```sh
cargo build --locked
cargo install --path . --locked          # installs into ~/.cargo/bin
cargo run --release --locked             # requires a display and GPU
```

> Compiling and running ordinary tests work headlessly, but launching the app
> (`cargo run`) requires a graphical desktop and working GPU.

## Running tests and checks

Run the full local verification gate before opening a pull request:

```sh
make verify
```

`make verify` runs the same checks as CI:

```sh
cargo test --locked
sh -n install.sh
python3 -B -m unittest discover -s scripts -p 'test_*.py'
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
git diff --check
```

Useful shortcuts (`make help` lists all targets, including benchmarks, probes,
profiling, macOS bundling, and releasing):

- `make test FILTER=grid::history::` — run a targeted test subset
- `make test-scripts` — run `install.sh` syntax and offline script tests
- `make fmt` — format code with `rustfmt`

Do not run all `--ignored` tests indiscriminately without a display/GPU—some
opt-in probes require a live graphics device. See
[docs/development.md](docs/development.md) for test filters by subsystem and
display-backed manual verification checklists.

## Pull requests and commits

- **Conventional Commits:** Commit messages follow Conventional Commits
  (`feat:`, `fix:`, `perf:`, `docs:`, `refactor:`, `test:`, `build:`, `chore:`).
  Release changelogs are generated directly from commit history by
  `scripts/release.py`.
- **Keep docs in sync:**
  - Keep [`README.md`](README.md) concise and strictly for end users.
  - Put detailed user workflows in [`docs/usage.md`](docs/usage.md) and config
    options in [`docs/configuration.md`](docs/configuration.md).
  - Update the relevant subsystem document under [`docs/`](docs/README.md) when
    behavior, limits, cache keys, or lifecycle invariants change.
- **Coding style & architecture:** Keep model state in `src/app/` and pure
  helpers, and keep widgets/GPU work in `src/renderer/`. Match the patterns and
  invariants documented in [`docs/`](docs/README.md).

## Where to go next

- [Documentation index](docs/README.md) — subsystem guides for architecture,
  terminal model, PTY lifecycle, rendering, interaction, memory, performance,
  idle CPU/energy, and updates.
- [Development & verification](docs/development.md) — build details, test
  filters, manual verification checklists, macOS packaging, and the release
  process.
- [AGENTS.md](AGENTS.md) — concise repository map and high-risk invariants.
- [Roadmap](ROADMAP.md) — planned features not yet implemented.
