<!--
Author: Jeff
Date: 2026-09-19
Description: mg-streamr — music and podcasts on mpd for the Geist suite
-->

# mg-streamr

mpd plays; mg-streamr asks it and tells it, and owns what mpd cannot keep: podcasts, with
where you stopped in each episode. Music stays in `~/music`. Podcast episodes stream, and any
episode can be saved to `~/music/podcasts/<show>/` for offline play.

```sh
mg-streamr status | watch                  # what is playing; watch prints on every mpd change
mg-streamr play [pos] | pause | toggle | stop | next | prev
mg-streamr seek 90|+30|-10   volume 60|+5|-5
mg-streamr queue [list|add <uri>|clear|remove <pos>]
mg-streamr library browse [dir] | search <text> | update
mg-streamr podcast add <name> <feed-url> | list | refresh [name] | episodes <name>
mg-streamr podcast play <id> | download <id> | remove-download <id> | forget <name>
mg-streamr art                             # path of the cached cover for what is playing
mg-streamr daemon                          # remembers podcast positions (user unit)
mg-streamr tui                             # Now Playing, Queue, Library, Podcasts
```

`--json` works everywhere, and failures print `{"ok":false,"error":…}` with exit 1.

- **mpd** is reached at `$MPD_HOST` if set, else `$XDG_RUNTIME_DIR/mpd/socket`, else
  127.0.0.1:6600. Arguments are quoted, and one containing a line break is refused.
- **Podcasts** live in `$MG_STREAMR_DB` or `$XDG_DATA_HOME/mg-streamr/streamr.sqlite`. Feeds and
  downloads go through mg-brief's guarded network path: SSRF checks, pinned DNS and size caps.
  An episode resumes where it was left, except in the first 10 s or the last minute. The daemon
  records the position every 10 s.
- **Artwork** is cached in `$XDG_CACHE_HOME/mg-streamr/art/`. Music covers come from mpd;
  episode and show images come through the guarded download.

The shell side lives in dotfiles: `Services/MediaBridge.qml` (listens to `watch`), the Media
panel (`qs -c mgeist ipc call player toggle`), and the Volume card's cover and "Open player".

## Gates

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```
