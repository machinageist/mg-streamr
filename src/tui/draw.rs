// Author: Jeff
// Date: 2026-09-19
// Description: Paint the player TUI — tabs, the tab's body, one line of keys or news at the bottom
// Notes: Named ANSI colours only, so the terminal theme decides the shades

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
                format!(" {} ", state.folder)
            };
            let rows = state
                .entries
                .iter()
                .map(|e| match e {
                    Entry::Folder(path) => {
                        Line::from(format!("{}/", path.rsplit('/').next().unwrap_or(path)))
                            .fg(ACCENT)
                    }
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
                        Line::from(vec![
                            Span::raw(s.title.clone().unwrap_or_else(|| s.name.clone())),
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
                            Span::raw(e.title.clone()),
                            Span::styled(format!("{at}{saved}"), Style::new().fg(DIM)),
                        ])
                    })
                    .collect();
                draw_list(frame, body, &format!(" {name} "), rows, state.cursor())
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
        Paragraph::new(keys).fg(if state.confirm_clear {
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
    match &song.artist {
        Some(a) => Line::from(vec![
            Span::styled(format!("{a} \u{2014} "), Style::new().fg(DIM)),
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
                .title(title.to_string())
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
        now.title.clone()
    };
    frame.render_widget(Paragraph::new(name).bold(), title);
    let by = [now.artist.as_str(), now.album.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
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
    let podcast = now
        .podcast
        .as_ref()
        .map_or(String::new(), |p| format!(" \u{b7} podcast {}", p.show));
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
}
