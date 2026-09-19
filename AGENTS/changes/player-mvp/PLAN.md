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
