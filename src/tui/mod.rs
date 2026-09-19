// Author: Jeff
// Date: 2026-09-19
// Description: `mg-streamr tui` — the player in a terminal: sample, draw, read a key, act
// Notes: A thread waits in mpd's idle and sends a fresh status on every change (and on a
//        reconnect after mpd restarts), so the screen follows mpd without polling. Keys run
//        their mpd command on a separate connection. Downloads run on their own thread so a
//        big episode never freezes the screen; their result comes back as a message

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::mpd::{Mpd, parse_songs};
use crate::player::{self, Now};
use crate::podcast;
use crate::store::{self, Store};

mod draw;
pub mod state;

use state::{Effect, Entry, State};

const FRAME: Duration = Duration::from_millis(250);
const RECONNECT_AFTER: Duration = Duration::from_secs(3);
const WATCHED: [&str; 4] = ["player", "mixer", "playlist", "options"];
const EPISODES_SHOWN: usize = 200;

// What the background threads report
enum Update {
    Now(Box<Now>),
    Message(String),
}

// Send a status now and after every mpd change, reconnecting when mpd goes away
fn follow(to_loop: Sender<Update>) {
    let store = Store::open(store::default_path()).ok();
    loop {
        let run = || -> Result<()> {
            let mut waiter = Mpd::connect_default()?;
            let mut asker = Mpd::connect_default()?;
            loop {
                let now = player::now(&mut asker, store.as_ref())?;
                if to_loop.send(Update::Now(Box::new(now))).is_err() {
                    return Ok(());
                }
                waiter.idle(&WATCHED)?;
            }
        };
        if let Err(e) = run() {
            let _ = to_loop.send(Update::Message(format!("mpd: {e:#}")));
        }
        std::thread::sleep(RECONNECT_AFTER);
    }
}

// Take over the terminal until quit, and always give it back
pub fn run() -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let follower = tx.clone();
    std::thread::spawn(move || follow(follower));
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &rx, &tx);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    rx: &Receiver<Update>,
    tx: &Sender<Update>,
) -> Result<()> {
    let mut state = State::default();
    let store = Store::open(store::default_path())?;
    let mut mpd = Mpd::connect_default().ok();
    loop {
        while let Ok(update) = rx.try_recv() {
            match update {
                Update::Now(now) => state.now = Some(*now),
                Update::Message(m) => state.message = Some(m),
            }
        }
        terminal.draw(|frame| draw::draw(frame, &state, chrono::Utc::now().timestamp_millis()))?;
        if !event::poll(FRAME)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        state.message = None;
        let effect = state.key(key);
        if effect == Effect::Quit {
            return Ok(());
        }
        // (re)connect for commands if mpd went away
        if mpd.is_none() {
            mpd = Mpd::connect_default().ok();
        }
        if let Err(e) = apply(effect, &mut state, &store, mpd.as_mut(), tx) {
            state.message = Some(format!("{e:#}"));
            mpd = None;
        }
        state.clamp();
    }
}

// Carry out one effect
fn apply(
    effect: Effect,
    state: &mut State,
    store: &Store,
    mpd: Option<&mut Mpd>,
    tx: &Sender<Update>,
) -> Result<()> {
    // downloads and store reads need no mpd
    match &effect {
        Effect::LoadShows => {
            state.shows = store.shows()?;
            return Ok(());
        }
        Effect::LoadEpisodes(name) => {
            state.episodes = store.episodes(name, EPISODES_SHOWN)?;
            return Ok(());
        }
        Effect::Download(id) => {
            let (id, tx) = (*id, tx.clone());
            state.message = Some(format!("downloading episode {id}\u{2026}"));
            std::thread::spawn(move || {
                let music = dirs::home_dir().unwrap_or_default().join("music");
                let said = Store::open(store::default_path())
                    .and_then(|s| podcast::download(&s, id, &music))
                    .map_or_else(
                        |e| format!("download failed: {e:#}"),
                        |path| format!("saved to ~/music/{path}"),
                    );
                let _ = tx.send(Update::Message(said));
            });
            return Ok(());
        }
        Effect::None | Effect::Quit => return Ok(()),
        _ => {}
    }
    let Some(mpd) = mpd else {
        anyhow::bail!("mpd is not answering")
    };
    match effect {
        Effect::Mpd(command, args) => {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            mpd.run(command, &args)?;
            // the queue view follows its own changes at once
            if matches!(command, "clear" | "delete" | "add") {
                state.queue = mpd.queue()?;
            }
            if command == "add" {
                state.message = Some("added to the queue".into());
            }
        }
        Effect::LoadQueue => state.queue = mpd.queue()?,
        Effect::Browse(folder) => {
            let pairs = mpd.run("lsinfo", &[&folder])?;
            let mut entries: Vec<Entry> = pairs
                .iter()
                .filter(|(k, _)| k == "directory")
                .map(|(_, v)| Entry::Folder(v.clone()))
                .collect();
            entries.extend(parse_songs(&pairs).into_iter().map(Entry::Song));
            state.entries = entries;
        }
        Effect::PlayEpisode(id) => {
            let episode = podcast::play(store, mpd, id)?;
            state.message = Some(format!("playing {}", episode.title));
        }
        _ => {}
    }
    Ok(())
}
