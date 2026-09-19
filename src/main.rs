// Author: Jeff
// Date: 2026-09-19
// Description: mg-streamr command line — control mpd, browse music, follow what is playing
// Notes: mpd does the playing; this asks and tells it. `--json` works everywhere; with it a
//        failure is still JSON on stdout ({"ok":false,"error":…}, exit 1), like the other suite
//        tools the shell reads. `watch` prints the status on every mpd change and waits in mpd's
//        idle between them — nothing polls

use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use serde_json::json;

use mg_streamr::art;
use mg_streamr::mpd::{Mpd, Song, parse_songs};
use mg_streamr::player::{self, Now};
use mg_streamr::podcast;
use mg_streamr::store::{self, Store};

// mpd areas whose changes the status reflects
const WATCHED: [&str; 4] = ["player", "mixer", "playlist", "options"];
// pause before reconnecting after mpd goes away, so a stopped mpd is not hammered
const RECONNECT_AFTER: Duration = Duration::from_secs(3);
const DEFAULT_SEARCH_LIMIT: usize = 100;

#[derive(Parser)]
#[command(name = "mg-streamr", version, about = "Music and podcasts on mpd")]
struct Cli {
    /// Print JSON instead of text
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// What is playing now
    Status,
    /// Start playing (the song at a queue position, or where it paused)
    Play {
        pos: Option<u32>,
    },
    Pause,
    Toggle,
    Stop,
    Next,
    Prev,
    /// Jump to a time: 90, +30 or -10 (seconds)
    Seek {
        #[arg(allow_hyphen_values = true)]
        to: String,
    },
    /// Set the volume: 60, +5 or -5
    Volume {
        #[arg(allow_hyphen_values = true)]
        level: String,
    },
    /// The play queue
    Queue {
        #[command(subcommand)]
        action: Option<QueueAction>,
    },
    /// The music library in ~/music
    Library {
        #[command(subcommand)]
        action: LibraryAction,
    },
    /// Print the status again on every change, one JSON line each
    Watch,
    /// Podcasts: subscribe, list, refresh, episodes, play, download
    Podcast {
        #[command(subcommand)]
        action: PodcastAction,
    },
    /// Path of the cached cover for what is playing
    Art,
    /// Remember where each podcast episode was left (run by the user unit)
    Daemon,
}

#[derive(Subcommand)]
enum PodcastAction {
    /// Subscribe to a feed under a short name (letters, digits, - _ .)
    Add {
        name: String,
        url: String,
    },
    List,
    /// Fetch one show's feed again, or every show's
    Refresh {
        name: Option<String>,
    },
    Episodes {
        name: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Play an episode, resuming where it was left
    Play {
        id: i64,
    },
    /// Save an episode into ~/music/podcasts/<show>/ for offline play
    Download {
        id: i64,
    },
    /// Delete a downloaded episode's file
    RemoveDownload {
        id: i64,
    },
    /// Unsubscribe (downloaded files stay until removed)
    Forget {
        name: String,
    },
}

#[derive(Subcommand)]
enum QueueAction {
    List,
    /// Add a library path, folder, or http(s) stream
    Add {
        uri: String,
    },
    Clear,
    Remove {
        pos: u32,
    },
}

#[derive(Subcommand)]
enum LibraryAction {
    /// Folders and songs in one folder of the library
    Browse {
        #[arg(default_value = "")]
        dir: String,
    },
    /// Songs whose tags or path mention the text
    Search {
        text: String,
        #[arg(long, default_value_t = DEFAULT_SEARCH_LIMIT)]
        limit: usize,
    },
    /// Ask mpd to rescan the library
    Update,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if json {
                println!("{}", json!({ "ok": false, "error": format!("{e:#}") }));
            } else {
                eprintln!("mg-streamr: {e:#}");
            }
            ExitCode::FAILURE
        }
    }
}

// The podcast store, if it can be opened; status still works without it
fn podcasts() -> Option<Store> {
    Store::open(store::default_path()).ok()
}

fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    if matches!(cli.command, Command::Watch) {
        return watch();
    }
    if matches!(cli.command, Command::Daemon) {
        return daemon();
    }
    if let Command::Podcast { action } = cli.command {
        return podcast(json, action);
    }
    let mut mpd = Mpd::connect_default()?;
    match cli.command {
        Command::Status => show_now(json, &now_with_art(&mut mpd, podcasts().as_ref())?),
        Command::Art => {
            let now = now_with_art(&mut mpd, podcasts().as_ref())?;
            if json {
                println!("{}", json!({ "art": now.art }));
            } else if let Some(path) = now.art {
                println!("{path}");
            }
            Ok(())
        }
        Command::Play { pos } => {
            match pos {
                Some(p) => mpd.run("play", &[&p.to_string()])?,
                None => mpd.run("play", &[])?,
            };
            done(json, &mut mpd)
        }
        Command::Pause => act(json, &mut mpd, "pause", &["1"]),
        Command::Toggle => {
            // mpd's bare `pause` flips pause/play, but does nothing when stopped — start instead
            let command = if mpd.status()?.state == "stop" {
                "play"
            } else {
                "pause"
            };
            act(json, &mut mpd, command, &[])
        }
        Command::Stop => act(json, &mut mpd, "stop", &[]),
        Command::Next => act(json, &mut mpd, "next", &[]),
        Command::Prev => act(json, &mut mpd, "previous", &[]),
        Command::Seek { to } => {
            if !valid_amount(&to) {
                bail!("seek takes seconds: 90, +30 or -10")
            }
            act(json, &mut mpd, "seekcur", &[&to])
        }
        Command::Volume { level } => {
            if !valid_amount(&level) {
                bail!("volume takes 0–100, +5 or -5")
            }
            // a sign means "change by"; mpd's `volume` does that, `setvol` sets outright
            if level.starts_with(['+', '-']) {
                act(json, &mut mpd, "volume", &[&level])
            } else {
                act(json, &mut mpd, "setvol", &[&level])
            }
        }
        Command::Queue { action } => match action.unwrap_or(QueueAction::List) {
            QueueAction::List => show_songs(json, &mpd.queue()?),
            QueueAction::Add { uri } => act(json, &mut mpd, "add", &[&uri]),
            QueueAction::Clear => act(json, &mut mpd, "clear", &[]),
            QueueAction::Remove { pos } => act(json, &mut mpd, "delete", &[&pos.to_string()]),
        },
        Command::Library { action } => match action {
            LibraryAction::Browse { dir } => {
                let pairs = mpd.run("lsinfo", &[&dir])?;
                let folders: Vec<&str> = pairs
                    .iter()
                    .filter(|(k, _)| k == "directory")
                    .map(|(_, v)| v.as_str())
                    .collect();
                let songs = parse_songs(&pairs);
                if json {
                    println!("{}", json!({ "folders": folders, "songs": songs }));
                } else {
                    for f in &folders {
                        println!("{f}/");
                    }
                    print_songs(&songs);
                }
                Ok(())
            }
            LibraryAction::Search { text, limit } => {
                let mut songs = parse_songs(&mpd.run("search", &["any", &text])?);
                songs.truncate(limit.clamp(1, 1000));
                show_songs(json, &songs)
            }
            LibraryAction::Update => act(json, &mut mpd, "update", &[]),
        },
        Command::Watch | Command::Daemon | Command::Podcast { .. } => unreachable!("handled above"),
    }
}

// The status with its cover filled in (a missing or failed cover is simply none)
fn now_with_art(mpd: &mut Mpd, store: Option<&Store>) -> Result<Now> {
    let mut now = player::now(mpd, store)?;
    now.art = art::cover(mpd, &now, &art::default_dir())
        .ok()
        .flatten()
        .map(|p| p.display().to_string());
    Ok(now)
}

// where mpd's library is; podcast downloads go inside it so mpd can play them
fn music_dir() -> std::path::PathBuf {
    dirs::home_dir().unwrap_or_default().join("music")
}

// Everything under `mg-streamr podcast …`
fn podcast(json: bool, action: PodcastAction) -> Result<()> {
    let store = Store::open(store::default_path())?;
    let print = |value: serde_json::Value, text: String| {
        if json {
            println!("{value}");
        } else {
            println!("{text}");
        }
    };
    match action {
        PodcastAction::Add { name, url } => {
            let (show, added) = podcast::subscribe(&store, &name, &url)?;
            print(
                json!({ "ok": true, "show": show, "added": added }),
                format!(
                    "{} subscribed: {added} episodes",
                    show.title.clone().unwrap_or(show.name.clone())
                ),
            );
        }
        PodcastAction::List => {
            let shows = store.shows()?;
            if json {
                println!("{}", serde_json::to_string(&shows)?);
            } else {
                for s in &shows {
                    println!(
                        "{:<20} {:>4} episodes, {:>3} new  {}",
                        s.name,
                        s.episodes,
                        s.unplayed,
                        s.title.as_deref().unwrap_or("")
                    );
                }
            }
        }
        PodcastAction::Refresh { name } => {
            let names: Vec<String> = match name {
                Some(n) => vec![n],
                None => store.shows()?.into_iter().map(|s| s.name).collect(),
            };
            let mut results = Vec::new();
            for n in names {
                // one broken feed should not stop the rest
                match podcast::refresh(&store, &n) {
                    Ok(added) => results.push(json!({ "show": n, "added": added })),
                    Err(e) => results.push(json!({ "show": n, "error": format!("{e:#}") })),
                }
            }
            print(
                json!({ "ok": true, "results": results }),
                results
                    .iter()
                    .map(|r| r.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        PodcastAction::Episodes { name, limit } => {
            let episodes = store.episodes(&name, limit)?;
            if json {
                println!("{}", serde_json::to_string(&episodes)?);
            } else {
                for e in &episodes {
                    let mark = if e.played { " " } else { "\u{2022}" };
                    let at = if e.position_seconds > 0.0 && !e.played {
                        format!("  at {}", player::clock(e.position_seconds))
                    } else {
                        String::new()
                    };
                    let saved = if e.download_path.is_some() {
                        "  [saved]"
                    } else {
                        ""
                    };
                    println!("{mark} {:>6}  {}{at}{saved}", e.id, e.title);
                }
            }
        }
        PodcastAction::Play { id } => {
            let mut mpd = Mpd::connect_default()?;
            let episode = podcast::play(&store, &mut mpd, id)?;
            print(
                json!({ "ok": true, "episode": episode.id }),
                format!("playing {}", episode.title),
            );
        }
        PodcastAction::Download { id } => {
            let path = podcast::download(&store, id, &music_dir())?;
            // tell mpd the file is there so it can play it
            if let Ok(mut mpd) = Mpd::connect_default() {
                let folder = path.rsplit_once('/').map_or(path.as_str(), |(dir, _)| dir);
                let _ = mpd.run("update", &[folder]);
            }
            print(
                json!({ "ok": true, "path": path }),
                format!("saved to ~/music/{path}"),
            );
        }
        PodcastAction::RemoveDownload { id } => {
            podcast::remove_download(&store, id, &music_dir())?;
            print(
                json!({ "ok": true }),
                format!("removed episode {id}'s file"),
            );
        }
        PodcastAction::Forget { name } => {
            store.forget(&name)?;
            print(json!({ "ok": true }), format!("unsubscribed from {name}"));
        }
    }
    Ok(())
}

// how often the daemon notes where a playing episode is
const RECORD_EVERY: Duration = Duration::from_secs(10);

// Note where the playing podcast episode is every few seconds, forever; mpd going away is waited out
fn daemon() -> Result<()> {
    let store = Store::open(store::default_path())?;
    let mut mpd: Option<Mpd> = None;
    loop {
        if mpd.is_none() {
            mpd = Mpd::connect_default().ok();
        }
        if let Some(connection) = mpd.as_mut() {
            match player::now(connection, Some(&store)) {
                Ok(now) => {
                    if let Some(p) = &now.podcast {
                        let duration = if now.duration > 0.0 {
                            Some(now.duration as i64)
                        } else {
                            None
                        };
                        if let Some((at, heard)) =
                            podcast::progress(&now.state, now.elapsed, duration)
                            && let Err(e) = store.set_position(p.episode_id, at, heard)
                        {
                            eprintln!("mg-streamr: could not save the position: {e:#}");
                        }
                    }
                }
                // a dropped connection: forget it and connect again next time
                Err(_) => mpd = None,
            }
        }
        std::thread::sleep(RECORD_EVERY);
    }
}

// A seek or volume amount: digits, optionally signed, optionally with a fraction
fn valid_amount(text: &str) -> bool {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    !digits.is_empty()
        && digits.len() <= 8
        && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
        && digits.matches('.').count() <= 1
}

// Run one command, then report the new state
fn act(json: bool, mpd: &mut Mpd, command: &str, args: &[&str]) -> Result<()> {
    mpd.run(command, args)?;
    done(json, mpd)
}

// After a change: the status, as JSON or one line
fn done(json: bool, mpd: &mut Mpd) -> Result<()> {
    show_now(json, &player::now(mpd, podcasts().as_ref())?)
}

fn show_now(json: bool, now: &Now) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(now)?);
        return Ok(());
    }
    let mark = match now.state.as_str() {
        "play" => "\u{25b6}",
        "pause" => "\u{23f8}",
        _ => "\u{25a0}",
    };
    let who = if now.artist.is_empty() {
        String::new()
    } else {
        format!("{} \u{2014} ", now.artist)
    };
    let volume = now.volume.map_or(String::new(), |v| format!("  vol {v}%"));
    let time = if now.duration > 0.0 {
        format!(
            "{} / {}",
            player::clock(now.elapsed),
            player::clock(now.duration)
        )
    } else {
        player::clock(now.elapsed)
    };
    if now.state == "stop" && now.title.is_empty() {
        println!("{mark} stopped{volume}");
    } else {
        println!("{mark} {who}{}  {time}{volume}", now.title);
    }
    Ok(())
}

fn show_songs(json: bool, songs: &[Song]) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(songs)?);
    } else {
        print_songs(songs);
    }
    Ok(())
}

fn print_songs(songs: &[Song]) {
    for s in songs {
        let pos = s.pos.map_or(String::new(), |p| format!("{p:>3}  "));
        let title = s
            .title
            .clone()
            .or_else(|| s.name.clone())
            .unwrap_or_else(|| s.file.clone());
        match &s.artist {
            Some(a) => println!("{pos}{a} \u{2014} {title}"),
            None => println!("{pos}{title}"),
        }
    }
}

// Print the status now and after every change, forever; reconnect if mpd restarts.
// Two connections: one waits in idle, the other asks for the status after each change
fn watch() -> Result<()> {
    let store = podcasts();
    let mut out = std::io::stdout().lock();
    loop {
        let result = (|| -> Result<()> {
            let mut waiter = Mpd::connect_default()?;
            let mut asker = Mpd::connect_default()?;
            loop {
                let now = now_with_art(&mut asker, store.as_ref())?;
                // a closed pipe means the shell stopped listening: end quietly
                if writeln!(out, "{}", serde_json::to_string(&now)?)
                    .and_then(|_| out.flush())
                    .is_err()
                {
                    std::process::exit(0);
                }
                waiter.idle(&WATCHED)?;
            }
        })();
        if let Err(e) = result {
            let line = json!({ "ok": false, "error": format!("{e:#}") });
            if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                return Ok(());
            }
        }
        std::thread::sleep(RECONNECT_AFTER);
    }
}
