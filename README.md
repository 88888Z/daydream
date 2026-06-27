# Daydream

**Idle video looper** — plays your favorite videos on loop when your computer is idle. A graceful, loveable desktop companion.

## Features

- **Drag & drop** videos from your file manager, or click to browse
- **Per-video parameters** — repeats, speed, volume (local override or global defaults)
- **Real thumbnails** from your OS cache (GNOME, Nautilus)
- **Playlist rotation** — resume from last played entry, or start from selection
- **Multi-select** — rubber band selection, bulk delete, select all
- **System tray** — runs minimized, quick access menu
- **Idle mode** — automatically plays when you're away, stops when you interact
- **Fullscreen playback** via mpv with Wayland support
- **Persistent config** — survives reboot, remembers your loop
- **Start on boot** — optional autostart

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
| `Delete` / `Backspace` | Delete selected |
| `Shift + click` | Toggle selection |

## Tech stack

- **Frontend**: React 19 + TypeScript + Tailwind CSS v4 + Zustand
- **Backend**: Rust + Tauri v2
- **Video**: mpv via Unix socket IPC
- **Idle detection**: GNOME Mutter D-Bus / loginctl / xprintidle

## License

MIT
