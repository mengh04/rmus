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

Detailed documentation is coming later.

## License

MIT
