// Author: Jeff
// Date: 2026-09-19
// Description: A small client for mpd's text protocol — commands, replies, pictures, idle
// Notes: mpd speaks lines: we send `command "arg" "arg"`, it answers `key: value` lines ending
//        in `OK`, or one `ACK [code@n] {command} message` line on failure. Pictures come as
//        chunks: a `binary: N` line, N raw bytes, a newline, then `OK`.
//        Every argument is quoted, and one holding a newline is refused outright — otherwise a
//        song name could smuggle a second command onto the wire.
//        Where mpd is: $MPD_HOST (a socket path, or [password@]host with $MPD_PORT), else the
//        user socket in $XDG_RUNTIME_DIR, else 127.0.0.1:6600. mpd listens on nothing else

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;

const DEFAULT_TCP: &str = "127.0.0.1:6600";
const DEFAULT_PORT: &str = "6600";
// a cover larger than this is not a cover
pub const MAX_PICTURE_BYTES: usize = 10 * 1024 * 1024;
// how long one reply may take; idle has no limit
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    Socket(PathBuf),
    Tcp(String),
}

// Where to find mpd, and the password to send if $MPD_HOST carries one
pub fn default_address() -> (Address, Option<String>) {
    if let Some(host) = std::env::var("MPD_HOST").ok().filter(|h| !h.is_empty()) {
        // "secret@host" carries a password
        let (password, host) = match host.rsplit_once('@') {
            Some((pw, h)) if !h.is_empty() => (Some(pw.to_string()), h.to_string()),
            _ => (None, host),
        };
        if host.starts_with('/') {
            return (Address::Socket(PathBuf::from(host)), password);
        }
        let port = std::env::var("MPD_PORT").unwrap_or_else(|_| DEFAULT_PORT.into());
        return (Address::Tcp(format!("{host}:{port}")), password);
    }
    let socket = std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("mpd/socket"));
    match socket.filter(|s| s.exists()) {
        Some(s) => (Address::Socket(s), None),
        None => (Address::Tcp(DEFAULT_TCP.into()), None),
    }
}

// Quote one argument the way mpd expects; a newline would end the command early, so refuse it
pub fn quote(arg: &str) -> Result<String> {
    if arg.contains('\n') || arg.contains('\r') {
        bail!("an mpd argument cannot contain a line break")
    }
    Ok(format!(
        "\"{}\"",
        arg.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

// ── What mpd reports ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Status {
    // play | pause | stop
    pub state: String,
    // None when mpd has no mixer
    pub volume: Option<i32>,
    pub elapsed: f64,
    pub duration: f64,
    // position of the current song in the queue
    pub song: Option<u32>,
    pub queue_length: u32,
    pub random: bool,
    pub repeat: bool,
    pub single: bool,
    pub consume: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Song {
    pub file: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    // a radio or podcast stream's own name
    pub name: Option<String>,
    pub duration: Option<f64>,
    pub pos: Option<u32>,
    pub id: Option<u32>,
}

// The value of one key in a reply, if present
fn get<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

// `status` reply → Status; missing or odd numbers read as zero rather than failing
pub fn parse_status(pairs: &[(String, String)]) -> Status {
    let num = |key: &str| get(pairs, key).and_then(|v| v.parse::<f64>().ok());
    let flag = |key: &str| get(pairs, key) == Some("1");
    Status {
        state: get(pairs, "state").unwrap_or("stop").to_string(),
        // mpd says -1 when there is no volume control
        volume: get(pairs, "volume")
            .and_then(|v| v.parse().ok())
            .filter(|v: &i32| *v >= 0),
        elapsed: num("elapsed").unwrap_or(0.0),
        duration: num("duration").unwrap_or(0.0),
        song: get(pairs, "song").and_then(|v| v.parse().ok()),
        queue_length: get(pairs, "playlistlength")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        random: flag("random"),
        repeat: flag("repeat"),
        single: flag("single"),
        consume: flag("consume"),
        error: get(pairs, "error").map(str::to_string),
    }
}

// A reply listing songs → one Song per `file:` line and the keys after it
pub fn parse_songs(pairs: &[(String, String)]) -> Vec<Song> {
    let mut songs: Vec<Song> = Vec::new();
    for (key, value) in pairs {
        if key == "file" {
            songs.push(Song {
                file: value.clone(),
                ..Default::default()
            });
            continue;
        }
        let Some(song) = songs.last_mut() else {
            continue;
        };
        match key.as_str() {
            "Title" => song.title = Some(value.clone()),
            "Artist" => song.artist = Some(value.clone()),
            "Album" => song.album = Some(value.clone()),
            "Name" => song.name = Some(value.clone()),
            "duration" => song.duration = value.parse().ok(),
            "Pos" => song.pos = value.parse().ok(),
            "Id" => song.id = value.parse().ok(),
            _ => {}
        }
    }
    songs
}

// ── The connection ───────────────────────────────────────────────────────

pub struct Mpd {
    reader: BufReader<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
    pub version: String,
}

impl Mpd {
    // Connect, read the greeting, and log in if a password was given
    pub fn connect(address: &Address, password: Option<&str>) -> Result<Self> {
        let (read, write): (Box<dyn Read + Send>, Box<dyn Write + Send>) = match address {
            Address::Socket(path) => {
                let s = UnixStream::connect(path)
                    .with_context(|| format!("mpd is not answering on {}", path.display()))?;
                s.set_read_timeout(Some(REPLY_TIMEOUT))?;
                (Box::new(s.try_clone()?), Box::new(s))
            }
            Address::Tcp(addr) => {
                let s = TcpStream::connect(addr)
                    .with_context(|| format!("mpd is not answering on {addr}"))?;
                s.set_read_timeout(Some(REPLY_TIMEOUT))?;
                (Box::new(s.try_clone()?), Box::new(s))
            }
        };
        let mut mpd = Mpd {
            reader: BufReader::new(read),
            writer: write,
            version: String::new(),
        };
        let greeting = mpd.read_line()?;
        mpd.version = greeting
            .strip_prefix("OK MPD ")
            .context("that is not mpd")?
            .to_string();
        if let Some(password) = password {
            mpd.run("password", &[password])?;
        }
        Ok(mpd)
    }

    // Connect wherever the environment says mpd is
    pub fn connect_default() -> Result<Self> {
        let (address, password) = default_address();
        Self::connect(&address, password.as_deref())
    }

    // One line from mpd, without its newline; the connection closing is an error
    fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            bail!("mpd closed the connection")
        }
        Ok(line.trim_end_matches('\n').to_string())
    }

    // Send one command with quoted arguments
    fn send(&mut self, command: &str, args: &[&str]) -> Result<()> {
        let mut line = command.to_string();
        for arg in args {
            line.push(' ');
            line.push_str(&quote(arg)?);
        }
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }

    // Read `key: value` lines up to OK; an ACK becomes an error carrying mpd's message
    fn read_reply(&mut self) -> Result<Vec<(String, String)>> {
        let mut pairs = Vec::new();
        loop {
            let line = self.read_line()?;
            if line == "OK" {
                return Ok(pairs);
            }
            if let Some(ack) = line.strip_prefix("ACK ") {
                // "[50@0] {play} No such song" → "No such song"
                let message = ack.split_once("} ").map_or(ack, |(_, m)| m);
                bail!("mpd: {message}")
            }
            if let Some((key, value)) = line.split_once(": ") {
                pairs.push((key.to_string(), value.to_string()));
            }
        }
    }

    // Run one command and return its reply
    pub fn run(&mut self, command: &str, args: &[&str]) -> Result<Vec<(String, String)>> {
        self.send(command, args)?;
        self.read_reply()
    }

    pub fn status(&mut self) -> Result<Status> {
        Ok(parse_status(&self.run("status", &[])?))
    }

    pub fn current_song(&mut self) -> Result<Option<Song>> {
        Ok(parse_songs(&self.run("currentsong", &[])?)
            .into_iter()
            .next())
    }

    pub fn queue(&mut self) -> Result<Vec<Song>> {
        Ok(parse_songs(&self.run("playlistinfo", &[])?))
    }

    // One chunk of a picture from `readpicture` or `albumart`: (total size, bytes), or None if
    // there is no picture
    fn picture_chunk(
        &mut self,
        command: &str,
        uri: &str,
        offset: usize,
    ) -> Result<Option<(usize, Vec<u8>)>> {
        self.send(command, &[uri, &offset.to_string()])?;
        let mut size = None;
        loop {
            let line = self.read_line()?;
            if line == "OK" {
                // readpicture answers a bare OK when the file has no embedded picture
                return Ok(None);
            }
            if let Some(ack) = line.strip_prefix("ACK ") {
                // albumart says "No file exists" when the folder has no cover
                let _ = ack;
                return Ok(None);
            }
            if let Some(v) = line.strip_prefix("size: ") {
                size = v.parse::<usize>().ok();
            } else if let Some(v) = line.strip_prefix("binary: ") {
                let length: usize = v.parse().context("bad binary length from mpd")?;
                let total = size.context("mpd sent picture bytes without a size")?;
                if total > MAX_PICTURE_BYTES || length > MAX_PICTURE_BYTES {
                    bail!("the picture is larger than {MAX_PICTURE_BYTES} bytes")
                }
                let mut bytes = vec![0u8; length];
                self.reader.read_exact(&mut bytes)?;
                // a newline after the bytes, then OK
                let mut newline = [0u8; 1];
                self.reader.read_exact(&mut newline)?;
                if self.read_line()? != "OK" {
                    bail!("mpd sent something after the picture bytes")
                }
                return Ok(Some((total, bytes)));
            }
        }
    }

    // A song's cover: the picture in the file first, else cover.jpg/png in its folder
    pub fn picture(&mut self, uri: &str) -> Result<Option<Vec<u8>>> {
        for command in ["readpicture", "albumart"] {
            let mut bytes = Vec::new();
            while let Some((total, chunk)) = self.picture_chunk(command, uri, bytes.len())? {
                // an empty chunk before the end would loop forever
                if chunk.is_empty() {
                    break;
                }
                bytes.extend_from_slice(&chunk);
                if bytes.len() >= total {
                    return Ok(Some(bytes));
                }
            }
        }
        Ok(None)
    }

    // Wait for mpd to report a change in any of these areas; returns which ones changed
    pub fn idle(&mut self, subsystems: &[&str]) -> Result<Vec<String>> {
        let mut line = String::from("idle");
        for s in subsystems {
            // subsystem names are fixed words, never user text
            if !s.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                bail!("not an mpd subsystem: {s}")
            }
            line.push(' ');
            line.push_str(s);
        }
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;
        // idle waits as long as it takes; only replies are bounded
        let changed = loop {
            match self.read_reply() {
                Ok(pairs) => break pairs,
                Err(e) if is_timeout(&e) => continue,
                Err(e) => return Err(e),
            }
        };
        Ok(changed
            .into_iter()
            .filter(|(k, _)| k == "changed")
            .map(|(_, v)| v)
            .collect())
    }
}

// Did a read give up because the reply timeout passed (as opposed to failing)
fn is_timeout(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|e| {
        matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    // A fake mpd: greets, then for each expected command line writes the scripted reply bytes
    fn fake(script: Vec<(&'static str, Vec<u8>)>) -> (tempfile::TempDir, Address) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mpd.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            writer.write_all(b"OK MPD 0.24.0\n").unwrap();
            for (expected, reply) in script {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(
                    line.trim_end(),
                    expected,
                    "the client sent the wrong command"
                );
                writer.write_all(&reply).unwrap();
            }
        });
        (dir, Address::Socket(path))
    }

    #[test]
    fn arguments_are_quoted_and_line_breaks_refused() {
        assert_eq!(quote(r#"a "b" \c"#).unwrap(), r#""a \"b\" \\c""#);
        assert!(
            quote("song\nclear").is_err(),
            "a newline would start a second command"
        );
    }

    #[test]
    fn status_and_songs_parse_and_an_ack_becomes_an_error() {
        let (_dir, address) = fake(vec![
            ("status", b"volume: -1\nrandom: 1\nstate: play\nsong: 2\nelapsed: 12.5\nduration: 300.0\nplaylistlength: 3\nOK\n".to_vec()),
            ("playlistinfo", b"file: a.mp3\nTitle: A\nArtist: X\nPos: 0\nId: 7\nfile: http://pod/ep.mp3\nName: Show\nPos: 1\nOK\n".to_vec()),
            (r#"play "9""#, b"ACK [2@0] {play} Bad song index\n".to_vec()),
        ]);
        let mut mpd = Mpd::connect(&address, None).unwrap();
        assert_eq!(mpd.version, "0.24.0");
        let status = mpd.status().unwrap();
        assert_eq!(
            (
                status.state.as_str(),
                status.song,
                status.elapsed,
                status.volume
            ),
            ("play", Some(2), 12.5, None)
        );
        assert!(status.random && !status.repeat);
        let queue = mpd.queue().unwrap();
        assert_eq!(queue.len(), 2);
        assert_eq!(
            (queue[0].title.as_deref(), queue[0].id),
            (Some("A"), Some(7))
        );
        assert_eq!(queue[1].name.as_deref(), Some("Show"));
        let err = mpd.run("play", &["9"]).unwrap_err();
        assert_eq!(err.to_string(), "mpd: Bad song index");
    }

    #[test]
    fn pictures_are_read_in_chunks_with_albumart_as_fallback() {
        let mut first = b"size: 6\ntype: image/png\nbinary: 4\n".to_vec();
        first.extend_from_slice(b"\x89PNG\nOK\n");
        let mut second = b"size: 6\ntype: image/png\nbinary: 2\n".to_vec();
        second.extend_from_slice(b"ab\nOK\n");
        let (_dir, address) = fake(vec![
            (r#"readpicture "x.flac" "0""#, first),
            (r#"readpicture "x.flac" "4""#, second),
            (r#"readpicture "y.mp3" "0""#, b"OK\n".to_vec()),
            (
                r#"albumart "y.mp3" "0""#,
                b"ACK [50@0] {albumart} No file exists\n".to_vec(),
            ),
        ]);
        let mut mpd = Mpd::connect(&address, None).unwrap();
        assert_eq!(mpd.picture("x.flac").unwrap().unwrap(), b"\x89PNGab");
        assert_eq!(
            mpd.picture("y.mp3").unwrap(),
            None,
            "no embedded picture and no cover file"
        );
    }

    #[test]
    fn idle_reports_what_changed() {
        let (_dir, address) = fake(vec![(
            "idle player mixer",
            b"changed: player\nchanged: mixer\nOK\n".to_vec(),
        )]);
        let mut mpd = Mpd::connect(&address, None).unwrap();
        assert_eq!(mpd.idle(&["player", "mixer"]).unwrap(), ["player", "mixer"]);
        assert!(mpd.idle(&["player; clear"]).is_err());
    }

    #[test]
    fn something_that_is_not_mpd_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(b"HTTP/1.1 400 Bad Request\n").unwrap();
        });
        assert!(Mpd::connect(&Address::Socket(path), None).is_err());
    }
}
