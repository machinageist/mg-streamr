// Author: Jeff
// Date: 2026-09-19
// Description: Podcasts — subscribe, refresh, resume rules, and downloading an episode for mpd
// Notes: Feeds are read through mg-brief's guarded fetch and files through its guarded download;
//        nothing here opens a socket itself. An entry is an episode only if it carries an http(s)
//        audio or video file. A download lands in <music>/podcasts/<show>/ under a name made
//        here (id, a slug of the title, an extension from the type), never one taken from the
//        feed, so a feed cannot choose where a file goes

use std::path::Path;

use anyhow::{Context, Result, bail};
use mg_brief::feed::ParsedFeed;

use crate::store::{Episode, NewEpisode, Show, Store};

// a podcast feed with years of episodes can be large; nothing sensible is bigger
const FEED_MAX_BYTES: u64 = 20 * 1024 * 1024;
const FEED_TIMEOUT_SECONDS: u64 = 30;
// an episode file (video episodes included)
const EPISODE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const EPISODE_TIMEOUT_SECONDS: u64 = 1800;
// under this, starting over is kinder than resuming
pub const RESUME_MIN_SECONDS: f64 = 10.0;
// this close to the end counts as heard
pub const PLAYED_WITHIN_SECONDS: f64 = 60.0;
pub const PODCAST_FOLDER: &str = "podcasts";
const SLUG_MAX: usize = 60;

// Where to pick up an episode: its saved spot, unless that is the very start or the very end
pub fn resume_from(position: f64, duration: Option<i64>) -> Option<f64> {
    if position < RESUME_MIN_SECONDS || is_played(position, duration) {
        return None;
    }
    Some(position)
}

// Heard to (nearly) the end
pub fn is_played(position: f64, duration: Option<i64>) -> bool {
    duration.is_some_and(|d| d > 0 && position >= d as f64 - PLAYED_WITHIN_SECONDS)
}

// A playable file: http(s), and audio, video, or a type the feed did not say
fn playable(url: &str, media_type: Option<&str>) -> bool {
    let web = url.starts_with("https://") || url.starts_with("http://");
    let kind = media_type.map(str::to_ascii_lowercase);
    web && kind.is_none_or(|t| t.starts_with("audio/") || t.starts_with("video/"))
}

// A parsed feed → the episodes worth keeping
pub fn episodes_from(feed: &ParsedFeed) -> Vec<NewEpisode> {
    feed.entries
        .iter()
        .filter_map(|entry| {
            let file = entry.enclosure.as_ref()?;
            if !playable(&file.url, file.media_type.as_deref()) {
                return None;
            }
            Some(NewEpisode {
                // no guid → the file itself identifies the episode
                guid: entry.guid.clone().unwrap_or_else(|| file.url.clone()),
                title: mg_brief::feed::title_text(&entry.title),
                url: file.url.clone(),
                media_type: file.media_type.clone(),
                published_at: entry.published.map(|d| d.to_rfc3339()),
                duration_seconds: entry.duration_seconds.map(|d| d as i64),
                image_url: entry.image_url.clone(),
                summary: entry.summary.clone(),
            })
        })
        .collect()
}

// Fetch a show's feed and store what it says; returns how many episodes are new
pub fn refresh(store: &Store, name: &str) -> Result<usize> {
    let show = store.show(name)?;
    let feed = mg_brief::fetch_feed_url(&show.feed_url, None, FEED_MAX_BYTES, FEED_TIMEOUT_SECONDS)
        .with_context(|| format!("reading {name}'s feed"))?;
    let episodes = episodes_from(&feed);
    if episodes.is_empty() {
        bail!("{name}'s feed has no audio or video episodes")
    }
    store.save_feed(
        name,
        feed.title.as_deref(),
        feed.image_url.as_deref(),
        &episodes,
    )
}

// Subscribe: record the show, then fetch it; a feed that fails leaves nothing behind
pub fn subscribe(store: &Store, name: &str, feed_url: &str) -> Result<(Show, usize)> {
    store.add_show(name, feed_url)?;
    match refresh(store, name) {
        Ok(added) => Ok((store.show(name)?, added)),
        Err(e) => {
            let _ = store.forget(name);
            Err(e)
        }
    }
}

// The file extension for a media type, else the URL's own, else mp3
fn extension(media_type: Option<&str>, url: &str) -> String {
    let by_type = match media_type.map(str::to_ascii_lowercase).as_deref() {
        Some("audio/mpeg") | Some("audio/mp3") => Some("mp3"),
        Some("audio/mp4") | Some("audio/x-m4a") | Some("audio/aac") => Some("m4a"),
        Some("audio/ogg") | Some("audio/vorbis") => Some("ogg"),
        Some("audio/opus") => Some("opus"),
        Some("audio/flac") | Some("audio/x-flac") => Some("flac"),
        Some("video/mp4") => Some("mp4"),
        Some("video/webm") => Some("webm"),
        _ => None,
    };
    if let Some(ext) = by_type {
        return ext.to_string();
    }
    // the last path segment's extension, if it is short and plain
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('/')
        .next()
        .and_then(|segment| {
            segment
                .rsplit_once('.')
                .map(|(_, ext)| ext.to_ascii_lowercase())
        })
        .filter(|ext| {
            (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
        })
        .unwrap_or_else(|| "mp3".into())
}

// "Episode 12: Hello, World!" → "episode-12-hello-world"
fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= SLUG_MAX {
            break;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "episode".into()
    } else {
        trimmed.to_string()
    }
}

// Where an episode's download goes, relative to the music folder (the path mpd uses)
pub fn download_path(episode: &Episode) -> String {
    format!(
        "{PODCAST_FOLDER}/{}/{}-{}.{}",
        episode.show,
        episode.id,
        slug(&episode.title),
        extension(episode.media_type.as_deref(), &episode.url)
    )
}

// Download an episode into the music folder and record it; returns the mpd path
pub fn download(store: &Store, id: i64, music_dir: &Path) -> Result<String> {
    let episode = store.episode(id)?;
    let relative = download_path(&episode);
    let dest = music_dir.join(&relative);
    std::fs::create_dir_all(dest.parent().context("download path has no folder")?)?;
    mg_brief::download_url(
        &episode.url,
        None,
        &dest,
        EPISODE_MAX_BYTES,
        EPISODE_TIMEOUT_SECONDS,
    )
    .with_context(|| format!("downloading episode {id}"))?;
    store.set_download(id, Some(&relative))?;
    Ok(relative)
}

// Delete a downloaded episode's file and forget it was downloaded
pub fn remove_download(store: &Store, id: i64, music_dir: &Path) -> Result<()> {
    let episode = store.episode(id)?;
    let Some(relative) = episode.download_path else {
        bail!("episode {id} is not downloaded")
    };
    // only ever inside the podcast folder, whatever the record says
    if !relative.starts_with(&format!("{PODCAST_FOLDER}/")) || relative.contains("..") {
        bail!("refusing to delete {relative}: not a podcast download")
    }
    match std::fs::remove_file(music_dir.join(&relative)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    store.set_download(id, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd"><channel><title>Show</title>
<itunes:image href="https://cdn.example/cover.jpg"/>
<item><title>Ep &amp;#8217;1</title><guid>g1</guid><enclosure url="https://cdn.example/1.mp3" type="audio/mpeg" length="9"/>
<itunes:duration>30:00</itunes:duration><pubDate>Fri, 18 Sep 2026 10:00:00 GMT</pubDate></item>
<item><title>Blog post</title><guid>g2</guid><link>https://example.com/post</link></item>
<item><title>Bad file</title><guid>g3</guid><enclosure url="file:///etc/passwd" type="audio/mpeg" length="9"/></item>
<item><title>Transcript</title><guid>g4</guid><enclosure url="https://cdn.example/4.pdf" type="application/pdf" length="9"/></item>
<item><title>No guid</title><enclosure url="https://cdn.example/5.m4a" type="audio/x-m4a" length="9"/></item>
</channel></rss>"#;

    #[test]
    fn only_http_audio_or_video_entries_become_episodes() {
        let feed = mg_brief::feed::parse_feed(FEED.as_bytes()).unwrap();
        let eps = episodes_from(&feed);
        assert_eq!(eps.len(), 2, "blog post, file:// and pdf are dropped");
        assert_eq!(eps[0].title, "Ep \u{2019}1");
        assert_eq!(eps[0].duration_seconds, Some(1800));
        assert!(eps[0].published_at.is_some());
        assert_eq!(
            eps[1].guid, eps[1].url,
            "no guid → the file is the identity"
        );
    }

    #[test]
    fn resume_skips_the_first_seconds_and_the_last_minute() {
        assert_eq!(resume_from(5.0, Some(1800)), None);
        assert_eq!(resume_from(600.0, Some(1800)), Some(600.0));
        assert_eq!(resume_from(1760.0, Some(1800)), None, "heard: start over");
        assert!(is_played(1745.0, Some(1800)));
        assert!(
            !is_played(1000.0, None),
            "no length, never counted as heard"
        );
    }

    #[test]
    fn download_names_are_made_here_not_taken_from_the_feed() {
        let ep = |title: &str, media: Option<&str>, url: &str| Episode {
            id: 42,
            show: "late-show".into(),
            guid: "g".into(),
            title: title.into(),
            url: url.into(),
            media_type: media.map(Into::into),
            published_at: None,
            duration_seconds: None,
            image_url: None,
            summary: None,
            position_seconds: 0.0,
            played: false,
            download_path: None,
        };
        assert_eq!(
            download_path(&ep(
                "Episode 12: Hello, World!",
                Some("audio/mpeg"),
                "https://x/a"
            )),
            "podcasts/late-show/42-episode-12-hello-world.mp3"
        );
        assert_eq!(
            download_path(&ep("../../etc", None, "https://x/f.OGG?t=1")),
            "podcasts/late-show/42-etc.ogg"
        );
        assert_eq!(
            download_path(&ep("!!!", None, "https://x/noext")),
            "podcasts/late-show/42-episode.mp3"
        );
        assert_eq!(
            download_path(&ep("x", None, "https://x/evil.sh;rm")),
            "podcasts/late-show/42-x.mp3",
            "odd extensions fall back"
        );
    }

    #[test]
    fn removing_a_download_never_leaves_the_podcast_folder() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("s.sqlite")).unwrap();
        store.add_show("pod", "https://pod.example/feed").unwrap();
        let feed = mg_brief::feed::parse_feed(FEED.as_bytes()).unwrap();
        store
            .save_feed("pod", None, None, &episodes_from(&feed))
            .unwrap();
        let id = store.episodes("pod", 10).unwrap()[0].id;
        let music = dir.path().join("music");
        std::fs::create_dir_all(music.join("podcasts/pod")).unwrap();
        std::fs::write(music.join("podcasts/pod/a.mp3"), "x").unwrap();
        store.set_download(id, Some("podcasts/pod/a.mp3")).unwrap();
        remove_download(&store, id, &music).unwrap();
        assert!(!music.join("podcasts/pod/a.mp3").exists());
        assert!(store.episode(id).unwrap().download_path.is_none());
        store
            .set_download(id, Some("podcasts/../../secret"))
            .unwrap();
        assert!(remove_download(&store, id, &music).is_err());
        store.set_download(id, Some("elsewhere/a.mp3")).unwrap();
        assert!(remove_download(&store, id, &music).is_err());
    }
}
