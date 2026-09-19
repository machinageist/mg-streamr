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

use mg_streamr::mpd::{Mpd, Song, parse_songs};
use mg_streamr::player::{self, Now};
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
    let mut mpd = Mpd::connect_default()?;
    match cli.command {
        Command::Status => show_now(json, &player::now(&mut mpd, podcasts().as_ref())?),
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
        Command::Watch => unreachable!("handled above"),
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
                let now = player::now(&mut asker, store.as_ref())?;
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
