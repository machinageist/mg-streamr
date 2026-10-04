// Author: Jeff
// Date: 2026-09-19
// Description: Paint the player TUI — tabs, the tab's body, one line of keys or news at the bottom
// Notes: Named ANSI colours only, so the terminal theme decides the shades

use mg_brief::feed::sanitize_terminal_text;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Tabs};

use super::state::{Entry, State, TABS, Tab};
use crate::player::clock;

const ACCENT: Color = Color::Magenta;
const DIM: Color = Color::DarkGray;

pub fn draw(frame: &mut Frame, state: &State, now_ms: i64) {
    let [top, body, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let titles = TABS
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{} {}", i + 1, t.title()));
    let selected = TABS.iter().position(|t| *t == state.tab).unwrap_or(0);
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .highlight_style(Style::new().fg(ACCENT).bold())
            .divider("\u{2502}"),
        top,
    );
    match state.tab {
        Tab::Playing => draw_playing(frame, body, state, now_ms),
        Tab::Queue => draw_list(
            frame,
            body,
            " queue ",
            state.queue.iter().map(song_line).collect(),
            state.cursor(),
        ),
        Tab::Library => {
            let title = if state.folder.is_empty() {
                " library ".to_string()
            } else {
                format!(" {} ", sanitize_terminal_text(&state.folder))
            };
            let rows = state
                .entries
                .iter()
                .map(|e| match e {
                    Entry::Folder(path) => Line::from(format!(
                        "{}/",
                        sanitize_terminal_text(path.rsplit('/').next().unwrap_or(path))
                    ))
                    .fg(ACCENT),
                    Entry::Song(song) => song_line(song),
                })
                .collect();
            draw_list(frame, body, &title, rows, state.cursor())
        }
        Tab::Podcasts => match &state.show {
            None => {
                let rows = state
                    .shows
                    .iter()
                    .map(|s| {
                        let title = s.title.as_deref().unwrap_or(&s.name);
                        Line::from(vec![
                            Span::raw(sanitize_terminal_text(title)),
                            Span::styled(format!("  {} new", s.unplayed), Style::new().fg(DIM)),
                        ])
                    })
                    .collect();
                draw_list(frame, body, " podcasts ", rows, state.cursor())
            }
            Some(name) => {
                let rows = state
                    .episodes
                    .iter()
                    .map(|e| {
                        let mark = if e.played { "  " } else { "\u{2022} " };
                        let at = if e.position_seconds > 0.0 && !e.played {
                            format!("  at {}", clock(e.position_seconds))
                        } else {
                            String::new()
                        };
                        let saved = if e.download_path.is_some() {
                            "  saved"
                        } else {
                            ""
                        };
                        Line::from(vec![
                            Span::styled(mark, Style::new().fg(ACCENT)),
                            Span::raw(sanitize_terminal_text(&e.title)),
                            Span::styled(format!("{at}{saved}"), Style::new().fg(DIM)),
                        ])
                    })
                    .collect();
                draw_list(
                    frame,
                    body,
                    &format!(" {} ", sanitize_terminal_text(name)),
                    rows,
                    state.cursor(),
                )
            }
        },
    }
    let keys = match state.tab {
        _ if state.confirm_clear => "Clear the whole queue? y = yes".to_string(),
        _ if state.message.is_some() => state.message.clone().unwrap_or_default(),
        Tab::Playing => "space play/pause \u{b7} n/p next/prev \u{b7} \u{2190}/\u{2192} seek \u{b7} +/- volume \u{b7} tab/1-4 \u{b7} q quit".into(),
        Tab::Queue => "enter play \u{b7} d remove \u{b7} c clear \u{b7} space play/pause \u{b7} q quit".into(),
        Tab::Library => "enter open/add \u{b7} a add folder \u{b7} backspace up \u{b7} q quit".into(),
        Tab::Podcasts => "enter open/play (resumes) \u{b7} D download \u{b7} esc back \u{b7} q quit".into(),
    };
    frame.render_widget(
        Paragraph::new(sanitize_terminal_text(&keys)).fg(if state.confirm_clear {
            Color::Yellow
        } else {
            DIM
        }),
        bottom,
    );
}

// "Artist — Title" for a queue or library row
fn song_line(song: &crate::mpd::Song) -> Line<'static> {
    let title = song
        .title
        .clone()
        .or_else(|| song.name.clone())
        .unwrap_or_else(|| song.file.clone());
    let title = sanitize_terminal_text(&title);
    match &song.artist {
        Some(a) => Line::from(vec![
            Span::styled(
                format!("{} — ", sanitize_terminal_text(a)),
                Style::new().fg(DIM),
            ),
            Span::raw(title),
        ]),
        None => Line::from(title),
    }
}

fn draw_list(frame: &mut Frame, area: Rect, title: &str, rows: Vec<Line<'static>>, cursor: usize) {
    let empty = rows.is_empty();
    let list = List::new(rows.into_iter().map(ListItem::new))
        .block(
            Block::new()
                .borders(Borders::TOP)
                .title(sanitize_terminal_text(title))
                .border_style(Style::new().fg(DIM)),
        )
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut list_state = ListState::default().with_selected((!empty).then_some(cursor));
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn draw_playing(frame: &mut Frame, area: Rect, state: &State, now_ms: i64) {
    let Some(now) = &state.now else {
        frame.render_widget(Paragraph::new("connecting to mpd\u{2026}").fg(DIM), area);
        return;
    };
    let [title, artist, gauge, info] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(area);
    let name = if now.title.is_empty() {
        "nothing playing".to_string()
    } else {
        sanitize_terminal_text(&now.title)
    };
    frame.render_widget(Paragraph::new(name).bold(), title);
    let by = [now.artist.as_str(), now.album.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| sanitize_terminal_text(s))
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    frame.render_widget(Paragraph::new(by).fg(ACCENT), artist);
    let elapsed = state.elapsed(now_ms);
    let ratio = if now.duration > 0.0 {
        (elapsed / now.duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let label = if now.duration > 0.0 {
        format!("{} / {}", clock(elapsed), clock(now.duration))
    } else {
        clock(elapsed)
    };
    frame.render_widget(
        Gauge::default()
            .ratio(ratio)
            .label(label)
            .gauge_style(Style::new().fg(ACCENT)),
        gauge,
    );
    let state_word = match now.state.as_str() {
        "play" => "playing",
        "pause" => "paused",
        _ => "stopped",
    };
    let volume = now
        .volume
        .map_or(String::new(), |v| format!(" \u{b7} volume {v}%"));
    let podcast = now.podcast.as_ref().map_or(String::new(), |p| {
        format!(" · podcast {}", sanitize_terminal_text(&p.show))
    });
    frame.render_widget(
        Paragraph::new(format!(
            "{state_word}{volume}{podcast} \u{b7} {} in queue",
            now.queue_length
        ))
        .fg(DIM),
        info,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpd::Song;
    use crate::store::{Episode, Show};
    use crate::tui::state::tests::now;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(state: &State, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, state, 5_000)).unwrap();
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn terminal_active_control(c: char) -> bool {
        matches!(c, '\u{0000}'..='\u{001f}' | '\u{007f}'..='\u{009f}')
    }

    fn hostile_terminal_text() -> String {
        let esc = char::from_u32(27).expect("ESC");
        let bel = char::from_u32(7).expect("BEL");
        format!("unsafe{esc}]8;;https://evil.test{bel}{esc}[2J")
    }

    #[test]
    fn every_tab_draws_and_the_clock_advances() {
        let mut s = State {
            now: Some(now("play", 10.0, 100.0, 0)),
            ..Default::default()
        };
        let text = screen(&s, 90, 12);
        assert!(text.contains("Song") && text.contains("Band") && text.contains("0:15 / 1:40"));
        for tab in TABS {
            s.tab = tab;
            screen(&s, 90, 12);
            // cramped must not panic
            screen(&s, 12, 3);
        }
        s.tab = Tab::Queue;
        s.confirm_clear = true;
        assert!(screen(&s, 90, 12).contains("Clear the whole queue?"));
    }

    #[test]
    fn mpd_and_podcast_text_never_reach_the_test_backend_as_controls() {
        let hostile = hostile_terminal_text();
        let mut current = now("play", 10.0, 100.0, 0);
        current.title = hostile.clone();
        current.artist = hostile.clone();
        current.album = hostile.clone();
        let episode = Episode {
            id: 1,
            show: hostile.clone(),
            guid: hostile.clone(),
            title: hostile.clone(),
            url: "https://pod.example/episode.mp3".into(),
            media_type: None,
            published_at: None,
            duration_seconds: None,
            image_url: None,
            summary: Some(hostile.clone()),
            position_seconds: 0.0,
            played: false,
            download_path: None,
        };
        let show = Show {
            id: 1,
            name: hostile.clone(),
            feed_url: "https://pod.example/feed".into(),
            title: Some(hostile.clone()),
            image_url: None,
            refreshed_at: None,
            episodes: 1,
            unplayed: 1,
        };
        let song = Song {
            file: hostile.clone(),
            title: Some(hostile.clone()),
            artist: Some(hostile.clone()),
            album: Some(hostile.clone()),
            name: Some(hostile.clone()),
            ..Default::default()
        };
        let mut state = State {
            now: Some(current),
            queue: vec![song.clone()],
            folder: hostile.clone(),
            entries: vec![Entry::Folder(hostile.clone()), Entry::Song(song)],
            shows: vec![show],
            episodes: vec![episode],
            message: Some(hostile.clone()),
            ..Default::default()
        };
        let mut rendered = String::new();
        for tab in [Tab::Playing, Tab::Queue, Tab::Library, Tab::Podcasts] {
            state.tab = tab;
            state.show = None;
            rendered.push_str(&screen(&state, 180, 12));
        }
        state.tab = Tab::Podcasts;
        state.show = Some(hostile);
        rendered.push_str(&screen(&state, 180, 12));

        assert!(
            !rendered.chars().any(terminal_active_control),
            "TestBackend must not receive ESC, BEL, or another C0/C1 control"
        );
        assert!(rendered.contains("]8;;https://evil.test") && rendered.contains("[2J"));
    }
}
