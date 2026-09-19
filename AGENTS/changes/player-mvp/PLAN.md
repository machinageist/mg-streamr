<!--
Author: Jeff
Date: 2026-09-19
Description: F1 slices, each committed with its gates green
-->

# mg-streamr plan

1. mpd client (connect, commands, parsing, binary, idle) with fake-server tests.
2. mg-brief: guarded streaming `download` (separate repo commit).
3. Store and podcasts: schema, add/list/refresh/episodes, from fixtures.
4. CLI: status, transport, queue, library, watch.
5. Podcast play/resume/download/forget, the position daemon, and the art cache.
6. TUI (ratatui): Now Playing, Queue, Library, Podcasts.
7. User units (mg-streamr daemon; enable mpd-mpris) in dotfiles.
8. Shell: MediaBridge on `watch`, Media panel, Volume card artwork and "Open player", IPC, launcher.

## Status (2026-09-19)

Done. mpd client 60419fb; store 28c3d44; podcasts 374829f and 8043a1f; player status d3b4bf8
and 19ec955; CLI 8fc93d0 and 1e42ebe; art 119bc53; TUI ce827a1. mg-brief gained
guarded_get/download_url (9facb85, 3a0a231) and duration and id fixes (b9d6dfb, a3285b8,
63bf9d1). Units: mg-streamr.service and the packaged mpd-mpris.service are enabled
(dotfiles 6ff6ed8). Shell: dotfiles 79bc8c2..7f4e436.
Verified end to end with NPR's Up First in a scratch store: subscribe, play, resume, cover,
download and remove. One early test played audio at full volume for about 25 s, because a
volume of 0 set while mpd was stopped did not carry into the new stream. Tests now disable the
output instead.
