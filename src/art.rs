// Author: Jeff
// Date: 2026-09-19
// Description: Cover art as local files — so the shell and TUI never fetch a picture themselves
// Notes: Cached in $XDG_CACHE_HOME/mg-streamr/art/ under a hash of where it came from. Music
//        covers come from mpd (keyed by the album folder, so one album is asked once); podcast
//        pictures come through mg-brief's guarded download. A song with no cover leaves a small
//        `.none` marker, so mpd is not asked again every time the song changes

use std::path::{Path, PathBuf};

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::mpd::Mpd;
use crate::player::Now;

const MAX_ART_BYTES: u64 = 10 * 1024 * 1024;
const ART_TIMEOUT_SECONDS: u64 = 20;
const NONE_SUFFIX: &str = ".none";

// $XDG_CACHE_HOME/mg-streamr/art
pub fn default_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mg-streamr/art")
}

// A stable file name for whatever the picture belongs to
pub fn key(source: &str) -> String {
    Sha256::digest(source.as_bytes())
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

// The folder a library file lives in — one cover per album
fn album_of(file: &str) -> &str {
    file.rsplit_once('/').map_or(file, |(dir, _)| dir)
}

// What the picture for this song should be keyed by, and where it comes from
enum Origin<'a> {
    Web(&'a str),
    Library(&'a str),
}

fn origin(now: &Now) -> Option<Origin<'_>> {
    if let Some(p) = &now.podcast {
        return p.image_url.as_deref().map(Origin::Web);
    }
    let file = now.file.as_deref()?;
    // a radio stream has no cover to read
    if file.contains("://") {
        return None;
    }
    Some(Origin::Library(file))
}

// A cached cover for what is playing, fetching it once if needed; None when there is none
pub fn cover(mpd: &mut Mpd, now: &Now, dir: &Path) -> Result<Option<PathBuf>> {
    let Some(origin) = origin(now) else {
        return Ok(None);
    };
    let name = match &origin {
        Origin::Web(url) => key(url),
        Origin::Library(file) => key(album_of(file)),
    };
    let path = dir.join(&name);
    let none = dir.join(format!("{name}{NONE_SUFFIX}"));
    if path.exists() {
        return Ok(Some(path));
    }
    if none.exists() {
        return Ok(None);
    }
    std::fs::create_dir_all(dir)?;
    let found = match origin {
        Origin::Web(url) => {
            mg_brief::download_url(url, None, &path, MAX_ART_BYTES, ART_TIMEOUT_SECONDS).is_ok()
        }
        Origin::Library(file) => match mpd.picture(file)? {
            Some(bytes) => {
                // write beside, then rename, so a reader never sees half a picture
                let part = dir.join(format!(".{name}.part"));
                std::fs::write(&part, bytes)?;
                std::fs::rename(&part, &path)?;
                true
            }
            None => false,
        },
    };
    if found {
        return Ok(Some(path));
    }
    std::fs::write(&none, b"")?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::PodcastRef;

    fn now(file: Option<&str>, podcast_image: Option<&str>) -> Now {
        Now {
            state: "play".into(),
            elapsed: 0.0,
            duration: 0.0,
            volume: None,
            random: false,
            repeat: false,
            queue_length: 1,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            file: file.map(Into::into),
            podcast: podcast_image.map(|i| PodcastRef {
                episode_id: 1,
                show: "s".into(),
                image_url: Some(i.into()),
            }),
            art: None,
            at: 0,
        }
    }

    #[test]
    fn keys_are_stable_and_an_album_shares_one_cover() {
        assert_eq!(key("a"), key("a"));
        assert_ne!(key("a"), key("b"));
        assert_eq!(key("a").len(), 32);
        assert_eq!(album_of("Band/Album/01 - x.flac"), "Band/Album");
        assert!(matches!(
            origin(&now(Some("Band/Album/1.flac"), None)),
            Some(Origin::Library("Band/Album/1.flac"))
        ));
        assert!(
            origin(&now(Some("https://radio.example/live"), None)).is_none(),
            "streams have no cover"
        );
        assert!(matches!(
            origin(&now(Some("https://cdn/e.mp3"), Some("https://cdn/e.jpg"))),
            Some(Origin::Web("https://cdn/e.jpg"))
        ));
    }
}
