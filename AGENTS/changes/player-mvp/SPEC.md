<!--
Author: Jeff
Date: 2026-09-19
Description: mg-streamr MVP — music and podcasts on mpd, with a CLI, TUI and the shell's Media panel
Notes: Geistos cycle 02, slice F1. Decided with Jeff 2026-09-18/19
-->

# mg-streamr MVP

## Decisions (Jeff)

- mpd is the engine. mpd listens only on its user socket and 127.0.0.1 (2026-09-19).
- Music stays in `~/music`. Podcasts stream; any episode can be downloaded on request to
  `~/music/podcasts/<show>/`, and then mpd plays the file.
- mg-streamr owns podcast data (shows, episodes, resume positions, downloads) in
  `$XDG_DATA_HOME/mg-streamr/streamr.sqlite`. Feeds are read through mg-brief's guarded fetch
  (`mg_brief::fetch_feed_url`), and episode and artwork downloads through a guarded streaming
  download added to mg-brief, so there is one hardened network path.
- Shell: a Media panel (Now Playing with artwork and seek, Queue, Library, Podcasts). The
  Volume card gains artwork and "Open player". Media keys work through MPRIS via mpd-mpris
  (installed by Jeff); no bind changes.

## Behaviour

- mpd is reached at `$MPD_HOST` if set, else `$XDG_RUNTIME_DIR/mpd/socket`, else
  127.0.0.1:6600. Arguments are quoted. An argument containing a newline is refused, so no
  second command can be smuggled into one.
- CLI, `--json` everywhere: `status`, `play|pause|toggle|next|prev`, `seek <s|+s|-s>`,
  `volume <0-100|+n|-n>`, `queue [list|add <uri>|clear|play <pos>]`,
  `library [browse <dir>|search <text>]`, `podcast add|list|refresh|episodes|play|download|forget`,
  `art` (path of the cached cover for what is playing), `watch` (one status line per mpd
  change, no polling), `daemon` (records podcast positions), `tui`.
- Resume: playing an episode seeks to its saved position (anything under 10 s starts from
  the top). The daemon saves the position of a playing episode every 10 s and marks it played
  within 60 s of the end.
- Artwork is cached under `$XDG_CACHE_HOME/mg-streamr/art/`; the shell only ever shows local
  files. Music covers come from mpd (`readpicture`, then `albumart`); episode and show images
  come through the guarded download.

## Acceptance

- Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- Tests: the mpd protocol against a scripted fake server (greeting, OK/ACK, key/value
  parsing, song lists, binary chunks, quoting and newline refusal); the store; podcast
  parsing from fixtures; resume and played rules.
- Live: status and watch against the real mpd, a real podcast subscribed and streamed,
  a resume after stop, a download, the Media panel and the Volume card.
