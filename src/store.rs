// Author: Jeff
// Date: 2026-09-19
// Description: mg-streamr's own records — podcast shows, episodes, where you stopped, downloads
// Notes: One SQLite file, $MG_STREAMR_DB or $XDG_DATA_HOME/mg-streamr/streamr.sqlite. mpd keeps
//        the music library; this keeps only what mpd cannot: podcasts. WAL so the position daemon
//        and the CLI share it; foreign keys on, so forgetting a show takes its episodes with it.
//        Migrations are append-only and recorded, like mg-brief's

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE shows (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, feed_url TEXT NOT NULL UNIQUE, \
     title TEXT, image_url TEXT, added_at TEXT NOT NULL, refreshed_at TEXT); \
     CREATE TABLE episodes (id INTEGER PRIMARY KEY, show_id INTEGER NOT NULL REFERENCES shows(id) ON DELETE CASCADE, \
     guid TEXT NOT NULL, title TEXT NOT NULL, url TEXT NOT NULL, media_type TEXT, published_at TEXT, \
     duration_seconds INTEGER, image_url TEXT, summary TEXT, position_seconds REAL NOT NULL DEFAULT 0, \
     played INTEGER NOT NULL DEFAULT 0, download_path TEXT, UNIQUE(show_id, guid)); \
     CREATE INDEX idx_episodes_show_published ON episodes(show_id, published_at DESC);",
];
// a show's name is a short handle used on the command line and as its download folder
const MAX_NAME: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Show {
    pub id: i64,
    pub name: String,
    pub feed_url: String,
    pub title: Option<String>,
    pub image_url: Option<String>,
    pub refreshed_at: Option<String>,
    pub episodes: i64,
    pub unplayed: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Episode {
    pub id: i64,
    pub show: String,
    pub guid: String,
    pub title: String,
    pub url: String,
    pub media_type: Option<String>,
    pub published_at: Option<String>,
    pub duration_seconds: Option<i64>,
    pub image_url: Option<String>,
    pub summary: Option<String>,
    pub position_seconds: f64,
    pub played: bool,
    pub download_path: Option<String>,
}

// One episode as a feed describes it, before it has an id
#[derive(Debug, Clone, PartialEq)]
pub struct NewEpisode {
    pub guid: String,
    pub title: String,
    pub url: String,
    pub media_type: Option<String>,
    pub published_at: Option<String>,
    pub duration_seconds: Option<i64>,
    pub image_url: Option<String>,
    pub summary: Option<String>,
}

pub struct Store {
    path: PathBuf,
}

// $MG_STREAMR_DB, else the XDG data folder
pub fn default_path() -> PathBuf {
    std::env::var_os("MG_STREAMR_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("mg-streamr/streamr.sqlite")
        })
}

// A show name is safe as a folder: letters, digits, dash, underscore, dot; not starting with a dot
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

const SHOW_SELECT: &str = "SELECT s.id,s.name,s.feed_url,s.title,s.image_url,s.refreshed_at, \
    (SELECT COUNT(*) FROM episodes e WHERE e.show_id=s.id), \
    (SELECT COUNT(*) FROM episodes e WHERE e.show_id=s.id AND e.played=0) FROM shows s";
const EPISODE_SELECT: &str = "SELECT e.id,s.name,e.guid,e.title,e.url,e.media_type,e.published_at, \
    e.duration_seconds,e.image_url,e.summary,e.position_seconds,e.played,e.download_path \
    FROM episodes e JOIN shows s ON s.id=e.show_id";

fn show_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Show> {
    Ok(Show {
        id: r.get(0)?,
        name: r.get(1)?,
        feed_url: r.get(2)?,
        title: r.get(3)?,
        image_url: r.get(4)?,
        refreshed_at: r.get(5)?,
        episodes: r.get(6)?,
        unplayed: r.get(7)?,
    })
}

fn episode_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Episode> {
    Ok(Episode {
        id: r.get(0)?,
        show: r.get(1)?,
        guid: r.get(2)?,
        title: r.get(3)?,
        url: r.get(4)?,
        media_type: r.get(5)?,
        published_at: r.get(6)?,
        duration_seconds: r.get(7)?,
        image_url: r.get(8)?,
        summary: r.get(9)?,
        position_seconds: r.get(10)?,
        played: r.get::<_, i64>(11)? != 0,
        download_path: r.get(12)?,
    })
}

impl Store {
    // Open (creating) the store and bring its schema up to date
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let store = Store { path };
        let mut c = store.conn()?;
        let mode: String = c.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            bail!("store could not switch to WAL (journal mode {mode})")
        }
        // the ledger and every pending migration in one write transaction
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY)",
        )?;
        let applied: i64 =
            tx.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))?;
        if applied as usize > MIGRATIONS.len() {
            bail!("this store was written by a newer mg-streamr")
        }
        for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied as usize) {
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO schema_migrations(version) VALUES (?1)",
                params![index as i64 + 1],
            )?;
        }
        tx.commit()?;
        Ok(store)
    }

    fn conn(&self) -> Result<Connection> {
        let c = Connection::open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))?;
        c.busy_timeout(Duration::from_secs(5))?;
        c.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(c)
    }

    // Add a show, or update its feed URL if the name exists
    pub fn add_show(&self, name: &str, feed_url: &str) -> Result<Show> {
        if !valid_name(name) {
            bail!("a show name is up to {MAX_NAME} letters, digits, dashes, underscores or dots")
        }
        self.conn()?.execute(
            "INSERT INTO shows(name,feed_url,added_at) VALUES (?1,?2,?3) ON CONFLICT(name) DO UPDATE SET feed_url=excluded.feed_url",
            params![name, feed_url, Utc::now().to_rfc3339()],
        )?;
        self.show(name)
    }

    pub fn show(&self, name: &str) -> Result<Show> {
        self.conn()?
            .query_row(
                &format!("{SHOW_SELECT} WHERE s.name=?1"),
                params![name],
                show_from_row,
            )
            .optional()?
            .with_context(|| format!("no show named {name}"))
    }

    pub fn shows(&self) -> Result<Vec<Show>> {
        let c = self.conn()?;
        let mut st = c.prepare(&format!("{SHOW_SELECT} ORDER BY s.name"))?;
        let rows = st
            .query_map([], show_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // Store what a refresh learned: the show's title and art, and every episode — new ones
    // added, known ones updated, where you stopped and what you downloaded left alone
    pub fn save_feed(
        &self,
        name: &str,
        title: Option<&str>,
        image_url: Option<&str>,
        episodes: &[NewEpisode],
    ) -> Result<usize> {
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let show_id: i64 = tx
            .query_row("SELECT id FROM shows WHERE name=?1", params![name], |r| {
                r.get(0)
            })
            .optional()?
            .with_context(|| format!("no show named {name}"))?;
        tx.execute(
            "UPDATE shows SET title=COALESCE(?1,title),image_url=COALESCE(?2,image_url),refreshed_at=?3 WHERE id=?4",
            params![title, image_url, Utc::now().to_rfc3339(), show_id],
        )?;
        let mut added = 0;
        for e in episodes {
            // an upsert reports one row either way, so ask first whether this one is new
            let known: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM episodes WHERE show_id=?1 AND guid=?2)",
                params![show_id, e.guid],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO episodes(show_id,guid,title,url,media_type,published_at,duration_seconds,image_url,summary) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) \
                 ON CONFLICT(show_id,guid) DO UPDATE SET title=excluded.title,url=excluded.url,media_type=excluded.media_type, \
                 published_at=excluded.published_at,duration_seconds=excluded.duration_seconds, \
                 image_url=excluded.image_url,summary=excluded.summary",
                params![show_id, e.guid, e.title, e.url, e.media_type, e.published_at, e.duration_seconds, e.image_url, e.summary],
            )?;
            if !known {
                added += 1;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    // A show's episodes, newest first
    pub fn episodes(&self, name: &str, limit: usize) -> Result<Vec<Episode>> {
        let c = self.conn()?;
        let mut st = c.prepare(&format!(
            "{EPISODE_SELECT} WHERE s.name=?1 ORDER BY e.published_at DESC, e.id DESC LIMIT ?2"
        ))?;
        let rows = st
            .query_map(params![name, limit.clamp(1, 1000) as i64], episode_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn episode(&self, id: i64) -> Result<Episode> {
        self.conn()?
            .query_row(
                &format!("{EPISODE_SELECT} WHERE e.id=?1"),
                params![id],
                episode_from_row,
            )
            .optional()?
            .with_context(|| format!("no episode {id}"))
    }

    // The episode mpd is playing, recognised by its stream URL or its downloaded file
    pub fn episode_by_uri(&self, uri: &str) -> Result<Option<Episode>> {
        Ok(self
            .conn()?
            .query_row(
                &format!("{EPISODE_SELECT} WHERE e.url=?1 OR e.download_path=?1 LIMIT 1"),
                params![uri],
                episode_from_row,
            )
            .optional()?)
    }

    // Remember where playback is, and whether the episode counts as heard
    pub fn set_position(&self, id: i64, seconds: f64, played: bool) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET position_seconds=?1, played=?2 WHERE id=?3",
            params![seconds.max(0.0), played as i64, id],
        )?;
        Ok(())
    }

    // Record (or clear) where an episode was downloaded, as mpd's path inside the music folder
    pub fn set_download(&self, id: i64, path: Option<&str>) -> Result<()> {
        self.conn()?.execute(
            "UPDATE episodes SET download_path=?1 WHERE id=?2",
            params![path, id],
        )?;
        Ok(())
    }

    // Forget a show and its episodes (downloaded files are the caller's to remove)
    pub fn forget(&self, name: &str) -> Result<()> {
        if self
            .conn()?
            .execute("DELETE FROM shows WHERE name=?1", params![name])?
            != 1
        {
            bail!("no show named {name}")
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(guid: &str, published: &str) -> NewEpisode {
        NewEpisode {
            guid: guid.into(),
            title: format!("Episode {guid}"),
            url: format!("https://cdn.example/{guid}.mp3"),
            media_type: Some("audio/mpeg".into()),
            published_at: Some(published.into()),
            duration_seconds: Some(1800),
            image_url: None,
            summary: None,
        }
    }

    #[test]
    fn a_show_collects_episodes_newest_first_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("s.sqlite")).unwrap();
        store
            .add_show("late-show", "https://pod.example/feed")
            .unwrap();
        let added = store
            .save_feed(
                "late-show",
                Some("Late Show"),
                Some("https://cdn.example/c.jpg"),
                &[ep("1", "2026-09-01"), ep("2", "2026-09-08")],
            )
            .unwrap();
        assert_eq!(added, 2);
        let again = store
            .save_feed(
                "late-show",
                None,
                None,
                &[ep("2", "2026-09-08"), ep("3", "2026-09-15")],
            )
            .unwrap();
        assert_eq!(again, 1, "only episode 3 is new");
        let list = store.episodes("late-show", 10).unwrap();
        assert_eq!(
            list.iter().map(|e| e.guid.as_str()).collect::<Vec<_>>(),
            ["3", "2", "1"]
        );
        let show = store.show("late-show").unwrap();
        assert_eq!(
            (show.title.as_deref(), show.episodes, show.unplayed),
            (Some("Late Show"), 3, 3)
        );
    }

    #[test]
    fn positions_and_downloads_survive_a_refresh_and_are_found_by_uri() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("s.sqlite")).unwrap();
        store.add_show("pod", "https://pod.example/feed").unwrap();
        store
            .save_feed("pod", None, None, &[ep("1", "2026-09-01")])
            .unwrap();
        let id = store.episodes("pod", 1).unwrap()[0].id;
        store.set_position(id, 612.5, false).unwrap();
        store.set_download(id, Some("podcasts/pod/1.mp3")).unwrap();
        store
            .save_feed("pod", None, None, &[ep("1", "2026-09-01")])
            .unwrap();
        let e = store.episode(id).unwrap();
        assert_eq!(
            (e.position_seconds, e.download_path.as_deref()),
            (612.5, Some("podcasts/pod/1.mp3"))
        );
        assert_eq!(
            store
                .episode_by_uri("https://cdn.example/1.mp3")
                .unwrap()
                .unwrap()
                .id,
            id
        );
        assert_eq!(
            store
                .episode_by_uri("podcasts/pod/1.mp3")
                .unwrap()
                .unwrap()
                .id,
            id
        );
        assert!(store.episode_by_uri("other.mp3").unwrap().is_none());
    }

    #[test]
    fn names_are_folder_safe_and_forgetting_takes_the_episodes() {
        for bad in ["", ".hidden", "../up", "a/b", "sp ace", &"x".repeat(65)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("s.sqlite")).unwrap();
        assert!(store.add_show("../up", "https://x").is_err());
        store.add_show("pod", "https://pod.example/feed").unwrap();
        store
            .save_feed("pod", None, None, &[ep("1", "2026-09-01")])
            .unwrap();
        store.forget("pod").unwrap();
        assert!(store.shows().unwrap().is_empty());
        assert!(
            store
                .episode_by_uri("https://cdn.example/1.mp3")
                .unwrap()
                .is_none(),
            "episodes went with the show"
        );
        assert!(store.forget("pod").is_err());
    }

    #[test]
    fn reopening_keeps_data_and_a_newer_store_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sqlite");
        Store::open(&path)
            .unwrap()
            .add_show("pod", "https://x")
            .unwrap();
        assert_eq!(Store::open(&path).unwrap().shows().unwrap().len(), 1);
        Connection::open(&path)
            .unwrap()
            .execute("INSERT INTO schema_migrations(version) VALUES (99)", [])
            .unwrap();
        assert!(Store::open(&path).is_err());
    }
}
