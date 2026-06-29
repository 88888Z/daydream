# Daydream

**Idle video looper** — plays your favorite videos on loop when your computer is idle. A graceful, loveable desktop companion.

## The Hard Problem

Daydream solves a detection problem that existing idle-watchers don't address:

> Stop playback when the user returns to the computer. **Don't stop during video transitions.**

The difficulty: mpv file transitions (end-file → start-file → playback-restart) produce **exactly the same idle-time readings** as a user returning — both cause `idle_ms` to drop from high to near-zero then climb. Standard idle detection can't distinguish them.

The system learns three distributions entirely from observation:

| Distribution | What it captures | How it's measured |
|---|---|---|
| `noise_drops` | Drop magnitudes during quiet playback | Samples during verified idle periods |
| `transition_rec` | Time for idle to recover after mpv transitions | Timed from `end-file` until idle exceeds 500ms |
| `trans_low_idle` | Consecutive ticks idle stays below 200ms during transitions | Counted during transition block windows |

Every parameter is derived from these distributions — zero machine-specific hardcoded values:

| Parameter | Derivation |
|---|---|
| Block window | `transition_rec.p95` — cover 95% of transition durations |
| Recovery threshold | `block_window + transition_rec.p50` — margin beyond block |
| Min confirmations | `trans_low_idle.p99 + 1` — outlast 99% of transition low-idle periods |
| Signal scoring | `noise_drops` percentiles — anomaly scale matched to observed noise |

## How Detection Works

Three independent trigger mechanisms cover every scenario:

| Trigger | Fires when | Catch scenario |
|---|---|---|
| **Drop-based** | `idle_ms` drops sharply relative to `noise_drops` distribution | User returns after being away |
| **Duration-based** | `idle_ms` stays below 500ms for `confirms × 2` consecutive ticks | User already present at playback start |
| **Force trigger** | Block window expires and idle is still below 200ms | User present through entire transition |

Once triggered, a **dual-threshold confirmation** phase prevents false stops from brief interactions:

- Idle below 200ms → increments confirmation counter
- Idle between 200ms and recovery threshold → **stalls** (counter unchanged)
- Idle above recovery threshold → **aborts** (counter reset)

This means a single media key press (idle climbs past 200ms after the press) stalls the counter before reaching STOP, while sustained user activity (idle stays below 200ms continuously) reaches STOP in ~500ms.

## Transition Correlation

When mpv fires `end-file` or `playback-restart`, the system:

1. **Resets** any in-progress detection — the idle drop was from mpv, not the user
2. **Blocks** new triggers for `transition_rec.p95` milliseconds — learned per-machine window
3. **Measures** `trans_low_idle` — counts consecutive low-idle ticks (if the user was present during the block, this measurement is discarded via a contamination guard, preventing user-presence data from polluting the transition noise model)
4. **Force-triggers** on block expiry if idle is still below 200ms — catches the case where the user returned during the transition

## Self-Calibration on First Run

| After | Behavior |
|---|---|
| 0 transitions | Conservative bootstrap: 21 confirms needed (~1.5s). Safe on any machine. |
| 1 transition | `transition_rec` measured. Block window adapts to actual transition duration. |
| 3 transitions | `trans_low_idle` distribution reliable. `min_confirmations` tightens to `tli_p99 + 1`. |
| 50+ clean samples | `noise_drops` distribution stabilizes. Scoring thresholds converge. |

## Features

- **Drag & drop** videos from your file manager, or click to browse
- **Per-video parameters** — repeats, speed, volume (local override or global defaults)
- **Real thumbnails** from your OS cache (GNOME, Nautilus)
- **Playlist rotation** — resume from last played entry via UUID (survives reorder/delete)
- **Multi-select** — rubber band selection, bulk delete, select all
- **System tray** — runs minimized, quick access menu
- **Idle mode** — automatically plays when you're away, stops when you interact
- **Fullscreen playback** via mpv with Wayland support
- **Persistent config** — survives reboot, remembers your loop
- **Start on boot** — optional autostart
- **Full observability** — per-thread tracing, TRIGGER/CONFIRM/ABORT/STOP logging, frontend telemetry overlay (Ctrl+Shift+T)
- **Generation counter** — prevents duplicate monitor threads when idle is toggled rapidly

## Requirements

- Linux (X11 or Wayland)
- [mpv](https://mpv.io/) video player
- dbus-send (for idle detection on GNOME)
- xprintidle (optional, for X11 idle detection)

## Install

### Debian / Ubuntu

```bash
sudo apt install mpv dbus-user-session
```

Download the `.deb` from [Releases](https://github.com/88888Z/daydream/releases) and install:

```bash
sudo dpkg -i Daydream_0.1.0_amd64.deb
```

### AppImage

Download the `.AppImage` from [Releases](https://github.com/88888Z/daydream/releases):

```bash
chmod +x Daydream_0.1.0_amd64.AppImage
./Daydream_0.1.0_amd64.AppImage
```

### Build from source

```bash
# Install dependencies
sudo apt install mpv libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev

# Install Tauri CLI
cargo install tauri-cli --version "^2"

# Build
git clone https://github.com/88888Z/daydream.git
cd daydream
npm install
cargo tauri build
```

## Usage

1. **Add videos** — drag & drop files onto the drop zone, or click to browse
2. **Adjust params** — click the pencil icon on any video to set repeats, speed, volume
3. **Play** — click Play Loop or press Space
4. **Idle mode** — toggle Idle in the footer, set timeout in Settings

### Keyboard shortcuts

| Key | Action |
|---|---|
| `Space` | Play / Stop |
| `Escape` | Clear selection |
| `Delete` / `Backspace` / `Del` (numpad) | Delete selected |
| `Shift + Delete` / `Shift + Del` (numpad) | Delete selected |
| `Ctrl + A` | Select all |
| `Shift + click` | Toggle selection |
| `Ctrl + Shift + T` | Telemetry overlay |

## Tech stack

- **Frontend**: React 19 + TypeScript + Tailwind CSS v4 + Zustand
- **Backend**: Rust + Tauri v2
- **Video**: mpv via Unix socket IPC
- **Idle detection**: GNOME Mutter D-Bus / loginctl / xprintidle

## License

MIT
