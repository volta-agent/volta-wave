# Volta Wave

A terminal music player with real-time audio visualization and synced lyrics.

## Features

- Keyboard-driven TUI interface
- Real-time spectrum analyzer visualization
- 5 visualization modes (Spectrum, Wave, Circles, Stars, Mirror)
- Synced lyrics display with LRCLIB API integration
- MP3 playback from local library
- Artist/Track parsing from filename
- File browser for adding tracks/directories
- Playlist save/load functionality
- Smooth color transitions

## Installation

```bash
cd ~/projects/volta-wave
cargo build --release
sudo cp target/release/volta-wave /usr/local/bin/
```

## Usage

```
volta-wave
```

### Controls

| Key | Action |
|-----|--------|
| `j` / `↓` | Navigate down |
| `k` / `↑` | Navigate up |
| `Enter` | Play selected / Confirm |
| `Space` | Pause/Resume |
| `s` | Stop playback |
| `n` | Next track (while playing) |
| `p` | Previous track (while playing) |
| `v` | Cycle visualization modes |
| `a` | Open file browser |
| `o` | Open playlist menu |
| `d` | Delete track from playlist / Add directory (in browser) |
| `h` | Toggle help |
| `Esc` | Return to normal mode |
| `q` | Quit |

### File Browser

Press `a` to open the file browser. Navigate with `j`/`k` or arrow keys.
- Press `Enter` to add a single MP3 file
- Press `d` to add all MP3s from a directory
- Press `Esc` to return to normal mode

### Playlists

Press `o` to open the playlist menu. Playlists are stored in `~/.volta-wave/playlists/`.
- Select a saved playlist and press `Enter` to load it
- Press `s` to save the current playlist (type a name and press Enter)
- Press `Esc` to cancel and return to normal mode

## Library Location

Scans `~/Music/` for MP3 files (recursive, max depth 2) on startup.

## Filename Format

Expects filenames like: `Artist - Title.mp3`

## Lyrics

Lyrics are automatically fetched from [lrclib.net](https://lrclib.net) and cached as `.lrc` files alongside the MP3s.

## Built With

- [ratatui](https://github.com/ratatui-org/ratatui) - TUI framework
- [kittyaudio](https://github.com/rutrumai/kittyaudio) - Audio playback
- [walkdir](https://github.com/BurntSushi/walkdir) - Directory traversal
- Rust 1.85
