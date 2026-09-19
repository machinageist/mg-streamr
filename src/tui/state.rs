// Author: Jeff
// Date: 2026-09-19
// Description: Everything the player TUI knows and what each key does — no terminal involved
// Notes: Keys become an Effect the loop carries out (tell mpd something, load a list, play an
//        episode, start a download). Pure, so tests drive it with plain key events.
//        Clearing the queue asks y/n; everything else is undoable and acts at once.
//        The clock: `now.elapsed` is where mpd was at `now.at`; while playing, the screen adds
//        the time since, so it moves every frame without asking mpd

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::mpd::Song;
use crate::player::Now;
use crate::store::{Episode, Show};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Playing,
    Queue,
    Library,
    Podcasts,
}

pub const TABS: [Tab; 4] = [Tab::Playing, Tab::Queue, Tab::Library, Tab::Podcasts];

impl Tab {
    pub fn title(self) -> &'static str {
        match self {
            Tab::Playing => "Now Playing",
            Tab::Queue => "Queue",
            Tab::Library => "Library",
            Tab::Podcasts => "Podcasts",
        }
    }
    fn index(self) -> usize {
        TABS.iter().position(|t| *t == self).unwrap_or(0)
    }
}

// A row in the library view: a folder to open or a song to add
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Folder(String),
    Song(Song),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    None,
    Quit,
    // an mpd command with its arguments
    Mpd(&'static str, Vec<String>),
    LoadQueue,
    Browse(String),
    LoadShows,
    LoadEpisodes(String),
    PlayEpisode(i64),
    Download(i64),
}

#[derive(Default)]
pub struct State {
    pub tab: Tab,
    pub now: Option<Now>,
    pub queue: Vec<Song>,
    pub folder: String,
    pub entries: Vec<Entry>,
    pub shows: Vec<Show>,
    // the show whose episodes are open, if any
    pub show: Option<String>,
    pub episodes: Vec<Episode>,
    pub cursor: [usize; 4],
    pub confirm_clear: bool,
    pub message: Option<String>,
}

// the step one arrow press seeks, in seconds
const SEEK_STEP: &str = "10";
const VOLUME_STEP: &str = "5";

impl State {
    // Where playback is this moment: mpd's last word, plus the time since if it is playing
    pub fn elapsed(&self, now_ms: i64) -> f64 {
        let Some(n) = &self.now else { return 0.0 };
        let moved = if n.state == "play" {
            (now_ms - n.at).max(0) as f64 / 1000.0
        } else {
            0.0
        };
        let at = n.elapsed + moved;
        if n.duration > 0.0 {
            at.min(n.duration)
        } else {
            at
        }
    }

    fn rows(&self, tab: Tab) -> usize {
        match tab {
            Tab::Playing => 0,
            Tab::Queue => self.queue.len(),
            Tab::Library => self.entries.len(),
            Tab::Podcasts if self.show.is_some() => self.episodes.len(),
            Tab::Podcasts => self.shows.len(),
        }
    }

    pub fn cursor(&self) -> usize {
        self.cursor[self.tab.index()]
    }

    // Keep each cursor inside its list after the list changed
    pub fn clamp(&mut self) {
        for tab in TABS {
            let rows = self.rows(tab);
            let c = &mut self.cursor[tab.index()];
            *c = (*c).min(rows.saturating_sub(1));
        }
    }

    fn step(&mut self, delta: isize) {
        let last = self.rows(self.tab).saturating_sub(1) as isize;
        let c = &mut self.cursor[self.tab.index()];
        *c = (*c as isize + delta).clamp(0, last.max(0)) as usize;
    }

    fn switch(&mut self, tab: Tab) -> Effect {
        self.tab = tab;
        match tab {
            Tab::Queue => Effect::LoadQueue,
            Tab::Library => Effect::Browse(self.folder.clone()),
            Tab::Podcasts => match &self.show {
                Some(name) => Effect::LoadEpisodes(name.clone()),
                None => Effect::LoadShows,
            },
            Tab::Playing => Effect::None,
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Effect {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Effect::Quit;
        }
        // the one question: only y clears the queue
        if self.confirm_clear {
            self.confirm_clear = false;
            return if key.code == KeyCode::Char('y') {
                Effect::Mpd("clear", vec![])
            } else {
                Effect::None
            };
        }
        let mpd = |command: &'static str, args: &[&str]| {
            Effect::Mpd(command, args.iter().map(|a| a.to_string()).collect())
        };
        match key.code {
            KeyCode::Char('q') => return Effect::Quit,
            KeyCode::Tab => return self.switch(TABS[(self.tab.index() + 1) % TABS.len()]),
            KeyCode::BackTab => {
                return self.switch(TABS[(self.tab.index() + TABS.len() - 1) % TABS.len()]);
            }
            KeyCode::Char(c @ '1'..='4') => return self.switch(TABS[c as usize - '1' as usize]),
            // transport works on every tab
            KeyCode::Char(' ') => {
                let stopped = self.now.as_ref().is_none_or(|n| n.state == "stop");
                return if stopped {
                    mpd("play", &[])
                } else {
                    mpd("pause", &[])
                };
            }
            KeyCode::Char('n') => return mpd("next", &[]),
            KeyCode::Char('p') => return mpd("previous", &[]),
            KeyCode::Right => return mpd("seekcur", &[&format!("+{SEEK_STEP}")]),
            KeyCode::Left => return mpd("seekcur", &[&format!("-{SEEK_STEP}")]),
            KeyCode::Char('+') | KeyCode::Char('=') => {
                return mpd("volume", &[&format!("+{VOLUME_STEP}")]);
            }
            KeyCode::Char('-') => return mpd("volume", &[&format!("-{VOLUME_STEP}")]),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::Home | KeyCode::Char('g') => self.step(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.step(isize::MAX / 2),
            _ => return self.tab_key(key.code),
        }
        Effect::None
    }

    // Keys that mean something on one tab only
    fn tab_key(&mut self, code: KeyCode) -> Effect {
        let at = self.cursor();
        match (self.tab, code) {
            (Tab::Queue, KeyCode::Enter) => {
                if let Some(song) = self.queue.get(at) {
                    return Effect::Mpd("play", vec![song.pos.unwrap_or(at as u32).to_string()]);
                }
            }
            (Tab::Queue, KeyCode::Char('d')) => {
                if let Some(song) = self.queue.get(at) {
                    return Effect::Mpd("delete", vec![song.pos.unwrap_or(at as u32).to_string()]);
                }
            }
            (Tab::Queue, KeyCode::Char('c')) => self.confirm_clear = true,
            (Tab::Library, KeyCode::Enter) => match self.entries.get(at) {
                Some(Entry::Folder(path)) => {
                    self.folder = path.clone();
                    self.cursor[Tab::Library.index()] = 0;
                    return Effect::Browse(path.clone());
                }
                Some(Entry::Song(song)) => return Effect::Mpd("add", vec![song.file.clone()]),
                None => {}
            },
            // a folder can be queued whole
            (Tab::Library, KeyCode::Char('a')) => {
                if let Some(Entry::Folder(path) | Entry::Song(Song { file: path, .. })) =
                    self.entries.get(at)
                {
                    return Effect::Mpd("add", vec![path.clone()]);
                }
            }
            (Tab::Library, KeyCode::Backspace) => {
                self.folder = self
                    .folder
                    .rsplit_once('/')
                    .map_or(String::new(), |(up, _)| up.to_string());
                self.cursor[Tab::Library.index()] = 0;
                return Effect::Browse(self.folder.clone());
            }
            (Tab::Podcasts, KeyCode::Enter) => match &self.show {
                Some(_) => {
                    if let Some(e) = self.episodes.get(at) {
                        return Effect::PlayEpisode(e.id);
                    }
                }
                None => {
                    if let Some(s) = self.shows.get(at) {
                        self.show = Some(s.name.clone());
                        self.cursor[Tab::Podcasts.index()] = 0;
                        return Effect::LoadEpisodes(s.name.clone());
                    }
                }
            },
            (Tab::Podcasts, KeyCode::Char('D')) => {
                if let (Some(_), Some(e)) = (&self.show, self.episodes.get(at)) {
                    return Effect::Download(e.id);
                }
            }
            // back from a show's episodes to the list of shows
            (Tab::Podcasts, KeyCode::Backspace | KeyCode::Esc) if self.show.is_some() => {
                self.show = None;
                self.cursor[Tab::Podcasts.index()] = 0;
                return Effect::LoadShows;
            }
            _ => {}
        }
        Effect::None
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState};

    pub fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    pub fn now(state: &str, elapsed: f64, duration: f64, at: i64) -> Now {
        Now {
            state: state.into(),
            elapsed,
            duration,
            volume: Some(50),
            random: false,
            repeat: false,
            queue_length: 1,
            title: "Song".into(),
            artist: "Band".into(),
            album: String::new(),
            file: Some("Band/Song.flac".into()),
            podcast: None,
            art: None,
            at,
        }
    }

    #[test]
    fn the_clock_moves_only_while_playing_and_stops_at_the_end() {
        let mut s = State {
            now: Some(now("play", 10.0, 100.0, 1_000)),
            ..Default::default()
        };
        assert_eq!(s.elapsed(4_000), 13.0);
        assert_eq!(s.elapsed(500_000), 100.0, "never past the end");
        s.now = Some(now("pause", 10.0, 100.0, 1_000));
        assert_eq!(s.elapsed(4_000), 10.0);
    }

    #[test]
    fn space_plays_when_stopped_and_pauses_otherwise() {
        let mut s = State {
            now: Some(now("stop", 0.0, 0.0, 0)),
            ..Default::default()
        };
        assert_eq!(s.key(key(KeyCode::Char(' '))), Effect::Mpd("play", vec![]));
        s.now = Some(now("play", 1.0, 9.0, 0));
        assert_eq!(s.key(key(KeyCode::Char(' '))), Effect::Mpd("pause", vec![]));
        assert_eq!(
            s.key(key(KeyCode::Right)),
            Effect::Mpd("seekcur", vec!["+10".into()])
        );
    }

    #[test]
    fn clearing_the_queue_asks_first() {
        let mut s = State {
            tab: Tab::Queue,
            ..Default::default()
        };
        assert_eq!(s.key(key(KeyCode::Char('c'))), Effect::None);
        assert_eq!(
            s.key(key(KeyCode::Char('n'))),
            Effect::None,
            "anything but y is no"
        );
        s.key(key(KeyCode::Char('c')));
        assert_eq!(s.key(key(KeyCode::Char('y'))), Effect::Mpd("clear", vec![]));
    }

    #[test]
    fn the_library_opens_folders_adds_songs_and_goes_back_up() {
        let mut s = State {
            tab: Tab::Library,
            entries: vec![
                Entry::Folder("Band".into()),
                Entry::Song(Song {
                    file: "a.flac".into(),
                    ..Default::default()
                }),
            ],
            ..Default::default()
        };
        assert_eq!(s.key(key(KeyCode::Enter)), Effect::Browse("Band".into()));
        s.entries = vec![Entry::Folder("Band/Album".into())];
        s.key(key(KeyCode::Enter));
        assert_eq!(
            s.key(key(KeyCode::Backspace)),
            Effect::Browse("Band".into())
        );
        s.entries = vec![Entry::Song(Song {
            file: "Band/x.flac".into(),
            ..Default::default()
        })];
        assert_eq!(
            s.key(key(KeyCode::Enter)),
            Effect::Mpd("add", vec!["Band/x.flac".into()])
        );
    }

    #[test]
    fn podcasts_open_a_show_then_play_or_download_an_episode() {
        let show = Show {
            id: 1,
            name: "pod".into(),
            feed_url: String::new(),
            title: None,
            image_url: None,
            refreshed_at: None,
            episodes: 1,
            unplayed: 1,
        };
        let mut s = State {
            shows: vec![show],
            ..Default::default()
        };
        assert_eq!(s.key(key(KeyCode::Char('4'))), Effect::LoadShows);
        assert_eq!(
            s.key(key(KeyCode::Enter)),
            Effect::LoadEpisodes("pod".into())
        );
        s.episodes = vec![Episode {
            id: 9,
            show: "pod".into(),
            guid: "g".into(),
            title: "Ep".into(),
            url: "https://c/e.mp3".into(),
            media_type: None,
            published_at: None,
            duration_seconds: None,
            image_url: None,
            summary: None,
            position_seconds: 0.0,
            played: false,
            download_path: None,
        }];
        assert_eq!(s.key(key(KeyCode::Enter)), Effect::PlayEpisode(9));
        assert_eq!(s.key(key(KeyCode::Char('D'))), Effect::Download(9));
        assert_eq!(s.key(key(KeyCode::Esc)), Effect::LoadShows);
        assert!(s.show.is_none());
    }
}
