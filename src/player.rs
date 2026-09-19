// Author: Jeff
// Date: 2026-09-19
// Description: What is playing, in one object — mpd's state, the song, and the podcast it belongs to
// Notes: `status` prints this once and `watch` prints it on every mpd change, so the shell, the
//        TUI and scripts all read the same shape. `title` and `artist` are filled for display:
//        a podcast episode shows its own title and its show; a stream its station name; a file
//        with no tags its file name. `at` is when mpd was asked (ms), so a reader can advance
//        `elapsed` itself between changes instead of asking every second

use anyhow::Result;
use serde::Serialize;

use crate::mpd::{Mpd, Song, Status};
use crate::store::Store;

#[derive(Debug, Clone, Serialize)]
pub struct PodcastRef {
    pub episode_id: i64,
    pub show: String,
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Now {
    pub state: String,
    pub elapsed: f64,
    pub duration: f64,
    pub volume: Option<i32>,
    pub random: bool,
    pub repeat: bool,
    pub queue_length: u32,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub file: Option<String>,
    pub podcast: Option<PodcastRef>,
    // a local cover image, when one is known
    pub art: Option<String>,
    pub at: i64,
}

// The last path piece of a file or URL, for songs with no tags
fn base_name(file: &str) -> String {
    let path = file.split(['?', '#']).next().unwrap_or(file);
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

// Put mpd's status, its current song and any podcast match together for display
pub fn compose(
    status: Status,
    song: Option<Song>,
    podcast: Option<(crate::store::Episode, Option<String>)>,
    at: i64,
) -> Now {
    let (title, artist, album) = match (&song, &podcast) {
        (_, Some((episode, show_title))) => (
            episode.title.clone(),
            show_title.clone().unwrap_or_else(|| episode.show.clone()),
            String::new(),
        ),
        (Some(s), None) => (
            s.title
                .clone()
                .or_else(|| s.name.clone())
                .unwrap_or_else(|| base_name(&s.file)),
            s.artist.clone().unwrap_or_default(),
            s.album.clone().unwrap_or_default(),
        ),
        (None, None) => (String::new(), String::new(), String::new()),
    };
    // a stream's duration is unknown to mpd; a podcast's feed may know it
    let duration = match (&podcast, status.duration) {
        (Some((episode, _)), d) if d <= 0.0 => episode.duration_seconds.unwrap_or(0) as f64,
        (_, d) => d,
    };
    Now {
        state: status.state,
        elapsed: status.elapsed,
        duration,
        volume: status.volume,
        random: status.random,
        repeat: status.repeat,
        queue_length: status.queue_length,
        title,
        artist,
        album,
        file: song.map(|s| s.file),
        podcast: podcast.map(|(e, _)| PodcastRef {
            episode_id: e.id,
            show: e.show,
            image_url: e.image_url,
        }),
        art: None,
        at,
    }
}

// Ask mpd (and the podcast store, if there is one) what is playing right now
pub fn now(mpd: &mut Mpd, store: Option<&Store>) -> Result<Now> {
    let status = mpd.status()?;
    let song = mpd.current_song()?;
    let podcast = match (store, &song) {
        (Some(store), Some(song)) => store.episode_by_uri(&song.file)?.map(|episode| {
            let show_title = store.show(&episode.show).ok().and_then(|s| s.title);
            (episode, show_title)
        }),
        _ => None,
    };
    Ok(compose(
        status,
        song,
        podcast,
        chrono::Utc::now().timestamp_millis(),
    ))
}

// "3:07" or "1:02:03"
pub fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Episode;

    fn song(file: &str, title: Option<&str>) -> Song {
        Song {
            file: file.into(),
            title: title.map(Into::into),
            artist: Some("Artist".into()),
            ..Default::default()
        }
    }

    fn playing(duration: f64) -> Status {
        Status {
            state: "play".into(),
            elapsed: 12.0,
            duration,
            ..Default::default()
        }
    }

    #[test]
    fn songs_fall_back_to_the_file_name_and_podcasts_show_their_show() {
        let n = compose(
            playing(200.0),
            Some(song("music/Band/01 - Song.flac", Some("Song"))),
            None,
            1,
        );
        assert_eq!((n.title.as_str(), n.artist.as_str()), ("Song", "Artist"));
        let untagged = compose(
            playing(200.0),
            Some(song("https://radio.example/live?x=1", None)),
            None,
            1,
        );
        assert_eq!(untagged.title, "live");
        let episode = Episode {
            id: 5,
            show: "late-show".into(),
            guid: "g".into(),
            title: "Ep 1".into(),
            url: "https://cdn.example/1.mp3".into(),
            media_type: None,
            published_at: None,
            duration_seconds: Some(1800),
            image_url: None,
            summary: None,
            position_seconds: 0.0,
            played: false,
            download_path: None,
        };
        let pod = compose(
            playing(0.0),
            Some(song("https://cdn.example/1.mp3", None)),
            Some((episode, Some("The Late Show".into()))),
            1,
        );
        assert_eq!(
            (pod.title.as_str(), pod.artist.as_str()),
            ("Ep 1", "The Late Show")
        );
        assert_eq!(
            pod.duration, 1800.0,
            "a stream's length comes from the feed"
        );
        assert_eq!(pod.podcast.unwrap().episode_id, 5);
    }

    #[test]
    fn clocks_read_like_a_player() {
        assert_eq!(clock(187.0), "3:07");
        assert_eq!(clock(3723.0), "1:02:03");
        assert_eq!(clock(-5.0), "0:00");
    }
}
