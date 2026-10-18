# rmus

Client/server terminal music player: `rmusd` daemon (audio engine) + `rmus` client.

Close your terminal — the music keeps playing.

## Features

- Daemon/client split: the daemon owns playback, clients come and go
- FLAC / MP3 / OGG / WAV / M4A decoding (symphonia), resampled to your device (rubato)
- MPRIS: desktop media keys, player widgets, cover art, lyrics
- Music library with search, auto-updated as files change
- Named playlists, repeat / shuffle, state persists across restarts
- Multi-client NDJSON protocol over a Unix socket

## Build

```
cargo install --path . --locked
```

## Usage

The daemon is auto-spawned on first use, so `rmus` alone is enough:

```
rmus ~/Music/song.flac     # play files or directories (replaces the queue)
rmus ~/Music/              # queue up a whole directory
rmus toggle                # play / pause
rmus next                  # next / previous
rmus volume 70             # absolute, or relative like +10
rmus status
rmus queue                 # show the queue
rmus seek 1:23             # or +10 / -10 for relative
rmus quit                  # stop the daemon
```

## TUI

Run `rmus` with no subcommand to open the interactive player:

![rmus TUI](docs/tui.png)

- Top bar: view tabs with counts, current queue name, playback state, repeat / shuffle / volume
- Queue: one row per track — number, title, artist, album, duration — with the playing track marked
  by an accent bar and the selection by a raised row; long titles truncate with an ellipsis
- Library: everything indexed from `~/Music`; `Enter` jump-plays a track, `a` appends it to the
  queue without interrupting playback
- Playlists: the active playlist carries a `当前` badge
- Bottom: now-playing title line plus a full-width progress bar (elapsed / total)
- `/` fuzzy library search, `:` command palette, `?` scrollable help sheet, `w` save the queue
- The layout collapses gracefully as the window narrows: album column, then artist column, then
  the secondary status readouts drop out; the terminal title follows the current track

### Editing the queue

- `J` / `K` move the selected track down / up inside the queue; the cursor follows the track
- `d` removes the selected track, `D` / `x` remove the playing one, `C` clears, `S` shuffles
- `c` renames the current queue (an anonymous queue gets its first name; a named one keeps its
  history under the new name)
- In the search overlay, `Enter` jump-plays and `Tab` appends to the queue without playing
- In the playlists view, `c` renames and `d` deletes the selected playlist

While an overlay is open the terminal cursor moves into its input line; everywhere else it is hidden.

### Theme

One palette — ink-blue surfaces with a warm amber "listening lamp" accent, steel blue for secondary
metadata. Colors are authored in RGB and quantized to xterm-256 when the terminal is not known to
support 24-bit color.

| Variable | Effect |
| --- | --- |
| `RMUS_COLOR=truecolor` | Force 24-bit color |
| `RMUS_COLOR=256` | Force the 256-color approximation |
| `NO_COLOR=1` | Monochrome: hierarchy moves to reverse video, rules and bars to glyphs |

Detailed documentation is coming later.

## Layout

One file, one job — if a file needs a table of contents, it gets split. Every module below has a
single responsibility and a doc comment explaining it.

```
src/
  proto.rs              wire protocol (NDJSON)
  engine/               audio engine (single thread, owns queue + device)
    mod.rs              Engine state, spawn, status
    run.rs              event loop (commands, decoder reports, position pushes)
    handlers.rs         request dispatch
    playback.rs         play / pause / stop / next / prev / seek / volume
    track.rs            decoder thread lifecycle (incl. play-now)
    peek.rs             preview state: snapshot + restore
    decoder.rs output.rs queue.rs resample.rs store.rs
  daemon/               rmusd
    main.rs             startup wiring (engine, MPRIS, library, socket)
    conn.rs             one client connection (writer task, event fan-out)
    dispatch.rs         routing: library queries, scans, engine commands
    reply.rs            NDJSON reply encoding
    watch.rs            library directory watcher
    mpris/              desktop integration (art, metadata, events, root/player interfaces)
  client/
    main.rs             CLI argument types
    cli/                subcommand dispatch, output, argument parsing
    tui/                the interactive player
      app/              state + actions, one file per input mode
      cmd/              `:` command table and parser
      keymap.rs         Normal-mode key table (single source of truth)
      ui/               rendering: theme, widgets, views, overlays
```

## License

MIT
