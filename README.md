# monitorzinho

A lightweight terminal system monitor, written in Rust.

- **CPU**, **memory**, **disk** (occupancy + read/write throughput), **network**
  (down/up), and **GPU** (NVIDIA, auto-detected) — last value, peak, and a
  recent-history sparkline for each.
- History is persisted to disk and restored on restart, so the charts aren't
  empty on launch.
- Small, fast, no garbage collector: the release binary is a couple of
  megabytes and starts instantly.
- Built to grow: adding a new metric is implementing one trait and registering
  it — see [Architecture](#architecture).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/willguitaradmfar/monitorzinho/main/install.sh | sh
```

Downloads the latest release binary to `~/.local/bin/monitorzinho` (Linux
x86_64 only for now). Then just run:

```sh
monitorzinho
```

**Keys:** `q`, `Esc`, or `Ctrl+C` to quit (saves history cleanly before exit).

### Build from source

Requires a stable Rust toolchain ([rustup.rs](https://rustup.rs)).

```sh
cargo build --release
./target/release/monitorzinho
```

or just `cargo run --release` during development.

## What it shows

- **System** — CPU usage, memory usage (+ used/total in GB).
- **Disk** — occupancy of the root filesystem as a compact numeric line
  (changes too slowly for a chart to be useful), plus read/write throughput
  charts.
- **Network** — download/upload throughput.
- **GPU** — utilization and VRAM usage, only shown on machines with a working
  NVIDIA driver (via [NVML](https://developer.nvidia.com/nvidia-management-library-nvml),
  dynamically loaded — the binary runs fine without one).
- **Top CPU** / **Top Memory** — the 10 heaviest processes by each metric,
  with full command line and run time.

Panels are grouped and color-coded by category, and turn yellow/red as a
metric approaches its natural limit (e.g. memory nearing 100%).

## Architecture

Two small traits drive everything:

- `Monitor` — a metric tracked over time (sparkline + persisted history).
  Implement it in `src/monitor/<name>.rs` and add it to `all_monitors()` in
  `src/monitor/mod.rs` to add a new one.
- `TableMonitor` — a ranked snapshot list (like the top-processes tables),
  for data that doesn't fit a time series.

History persists to `~/.local/share/monitorzinho/history.json` (or your
platform's equivalent data directory).

## License

MIT — see [LICENSE](LICENSE).
