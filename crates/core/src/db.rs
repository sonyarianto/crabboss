//! Central SQLite bootstrap (P1.2).
//!
//! Five managers share one database file through separate connections.
//! This module owns what they share:
//!
//! - [`Database::open_connection`]: uniform per-connection setup —
//!   `foreign_keys = ON` (the `ON DELETE CASCADE` schemas rely on it)
//!   plus a bounded 5 s busy timeout so lock contention across the
//!   connections fails fast instead of hanging a tick.
//! - [`Database::initialize`]: numbered, transactional, idempotent
//!   migrations tracked by `PRAGMA user_version`. The version bumps only
//!   after a migration commits.
//!
//! Journal policy is deliberately SQLite's default (rollback-journal,
//! `synchronous = FULL`): no `-wal`/`-shm` sidecars, so the database
//! stays a single file for backup/restore (P0.3) and the P1.1 data-dir
//! migration. Revisit only with a measured contention problem.
//!
//! Manager `init_tables` keep their own idempotent `CREATE TABLE IF NOT
//! EXISTS` as a safety net (tests open managers directly without going
//! through bootstrap), but schema *evolution* lives here.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, Transaction};

use crate::error::{CrabError, Result};

/// Current schema version. Bump when appending to [`MIGRATIONS`].
pub const SCHEMA_VERSION: u32 = 3;

/// How long a connection waits on a locked database before erroring.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Single-purpose database bootstrap; all methods are associated fns.
pub struct Database;

struct Migration {
    version: u32,
    name: &'static str,
    apply: fn(&Transaction<'_>) -> std::result::Result<(), rusqlite::Error>,
}

/// Numbered migrations, oldest first. Each must be idempotent on its
/// own (a crash between migrations re-runs the pending ones).
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "baseline",
        apply: m1_baseline,
    },
    Migration {
        version: 2,
        name: "scheduler expires_on",
        apply: m2_scheduler_expires_on,
    },
    Migration {
        version: 3,
        name: "tracks enrichment columns",
        apply: m3_tracks_extra_columns,
    },
];

/// One scheduler row for wholesale replacement. No id: fresh ids are
/// assigned, so pre-existing fire-dedupe keys go stale (callers clear
/// them, like the restore path does).
#[derive(Debug, Clone)]
pub struct SchedulerRow {
    pub name: String,
    pub action_type: String,
    pub target: String,
    pub start_time: String,
    pub days: String,
    pub expires_on: Option<String>,
    pub enabled: bool,
}

/// One cart pad for wholesale replacement.
#[derive(Debug, Clone)]
pub struct CartRow {
    pub label: String,
    pub file_path: String,
    pub position: i32,
}

/// One ad block for wholesale replacement. Dates are `YYYY-MM-DD`.
#[derive(Debug, Clone)]
pub struct AdBlockRow {
    pub name: String,
    pub spot_path: String,
    pub intro_path: Option<String>,
    pub outro_path: Option<String>,
    pub start_date: String,
    pub end_date: String,
    pub play_time: String,
    pub days: String,
    pub enabled: bool,
}

/// How many rows each list has after [`Database::replace_station_lists`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplaceCounts {
    pub scheduler: usize,
    pub carts: usize,
    pub ads: usize,
}

impl Database {
    /// Atomically replace the three automation lists in ONE transaction:
    /// delete-all + insert-all commit together or roll back together. A
    /// failure anywhere leaves every list exactly as it was.
    ///
    /// Rows must be pre-validated (the UI `validate_backup` mirrors the
    /// manager rules); out-of-range cart positions fail the whole
    /// replace closed rather than writing an illegal pad. Duplicate cart
    /// positions resolve last-wins, mirroring `assign_at`.
    ///
    /// Insert shapes mirror the manager `create()`s (fresh uuids,
    /// `created_at = now`, empty intro/outro stored as `''` like
    /// `AdsManager::create` does).
    pub fn replace_station_lists(
        path: &Path,
        scheduler: &[SchedulerRow],
        carts: &[CartRow],
        ads: &[AdBlockRow],
    ) -> Result<ReplaceCounts> {
        let mut conn = Self::open_connection(path)?;
        let tx = conn.transaction()?;
        let fail = |what: &str, e: rusqlite::Error| CrabError::BulkReplace(format!("{what}: {e}"));
        tx.execute_batch("DELETE FROM scheduled_events; DELETE FROM carts; DELETE FROM ad_blocks;")
            .map_err(|e| fail("clear lists", e))?;
        for s in scheduler {
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO scheduled_events
                 (id, name, action_type, target, start_time, days, enabled, created_at, expires_on)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    id,
                    s.name,
                    s.action_type,
                    s.target,
                    s.start_time,
                    s.days,
                    s.enabled as i32,
                    chrono::Utc::now().to_rfc3339(),
                    s.expires_on,
                ],
            )
            .map_err(|e| fail(&format!("insert scheduler '{}'", s.name), e))?;
        }
        for c in carts {
            if !(0..crate::cart::WALL_SIZE as i32).contains(&c.position) {
                return Err(CrabError::BulkReplace(format!(
                    "bad cart position {}",
                    c.position
                )));
            }
            // Last-wins per pad, like `assign_at`.
            tx.execute("DELETE FROM carts WHERE position = ?1", [c.position])
                .map_err(|e| fail(&format!("clear cart pad {}", c.position), e))?;
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO carts (id, label, file_path, position, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    id,
                    c.label,
                    c.file_path,
                    c.position,
                    chrono::Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|e| fail(&format!("insert cart '{}'", c.label), e))?;
        }
        for a in ads {
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO ad_blocks
                 (id, name, spot_path, intro_path, outro_path,
                  start_date, end_date, play_time, days, enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    id,
                    a.name,
                    a.spot_path,
                    a.intro_path.as_deref().unwrap_or(""),
                    a.outro_path.as_deref().unwrap_or(""),
                    a.start_date,
                    a.end_date,
                    a.play_time,
                    a.days,
                    a.enabled as i32,
                ],
            )
            .map_err(|e| fail(&format!("insert ad block '{}'", a.name), e))?;
        }
        tx.commit()?;
        Ok(ReplaceCounts {
            scheduler: scheduler.len(),
            carts: carts.len(),
            ads: ads.len(),
        })
    }

    /// Open one uniformly configured connection. Does NOT migrate;
    /// call [`Database::initialize`] once at startup first.
    pub fn open_connection(path: &Path) -> Result<Connection> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(conn)
    }

    /// Bring the database file to [`SCHEMA_VERSION`], running every
    /// pending migration inside its own transaction. Safe to call on
    /// every startup and on already-current files.
    pub fn initialize(path: &Path) -> Result<()> {
        let mut conn = Self::open_connection(path)?;
        let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if current > SCHEMA_VERSION {
            return Err(CrabError::SchemaTooNew {
                found: current,
                supported: SCHEMA_VERSION,
            });
        }
        apply_pending(&mut conn, current, MIGRATIONS)
    }
}

fn apply_pending(conn: &mut Connection, from: u32, migrations: &[Migration]) -> Result<()> {
    for m in migrations {
        if from < m.version {
            let tx = conn.transaction()?;
            (m.apply)(&tx).map_err(|source| CrabError::Migration {
                version: m.version,
                name: m.name,
                source,
            })?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", m.version))?;
            tx.commit()?;
            tracing::info!("Database migrated to schema v{} ({})", m.version, m.name);
        }
    }
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = ?",
        [table, column],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// v1: the full schema as of this commit. Fresh files converge here;
/// pre-versioning files no-op through the `IF NOT EXISTS` guards and
/// get repaired by v2/v3.
fn m1_baseline(tx: &Transaction<'_>) -> std::result::Result<(), rusqlite::Error> {
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS tracks (
            id              TEXT PRIMARY KEY,
            file_path       TEXT NOT NULL UNIQUE,
            file_name       TEXT NOT NULL,
            title           TEXT,
            artist          TEXT,
            album           TEXT,
            genre           TEXT,
            year            INTEGER,
            track_number    INTEGER,
            duration_secs   REAL,
            bpm             REAL,
            file_size       INTEGER,
            sample_rate     INTEGER,
            channels        INTEGER,
            kind            TEXT NOT NULL DEFAULT 'music',
            daypart_start   INTEGER,
            daypart_end     INTEGER,
            daypart_days    TEXT NOT NULL DEFAULT 'Daily',
            loudness_lufs   REAL,
            loudness_gain_db REAL,
            added_at        TEXT NOT NULL,
            last_played_at  TEXT,
            play_count      INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist);
        CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(album);
        CREATE INDEX IF NOT EXISTS idx_tracks_genre ON tracks(genre);
        CREATE TABLE IF NOT EXISTS tags (
            track_id TEXT NOT NULL,
            tag      TEXT NOT NULL,
            PRIMARY KEY (track_id, tag),
            FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS play_log (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            track_id    TEXT NOT NULL,
            played_at   TEXT NOT NULL,
            duration    REAL,
            FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS playlists (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            description TEXT,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS playlist_items (
            playlist_id TEXT NOT NULL,
            track_id    TEXT NOT NULL,
            position    INTEGER NOT NULL,
            is_jingle   INTEGER NOT NULL DEFAULT 0,
            is_ad       INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (playlist_id, position),
            FOREIGN KEY (playlist_id) REFERENCES playlists(id) ON DELETE CASCADE,
            FOREIGN KEY (track_id)    REFERENCES tracks(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS scheduled_events (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            action_type TEXT NOT NULL,
            target      TEXT NOT NULL DEFAULT '',
            start_time  TEXT NOT NULL,
            days        TEXT NOT NULL DEFAULT 'Daily',
            enabled     INTEGER NOT NULL DEFAULT 1,
            created_at  TEXT NOT NULL,
            expires_on  TEXT
        );
        CREATE TABLE IF NOT EXISTS carts (
            id          TEXT PRIMARY KEY,
            label       TEXT NOT NULL,
            file_path   TEXT NOT NULL DEFAULT '',
            position    INTEGER NOT NULL DEFAULT 0,
            created_at  TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS ad_blocks (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            spot_path   TEXT NOT NULL,
            intro_path  TEXT NOT NULL DEFAULT '',
            outro_path  TEXT NOT NULL DEFAULT '',
            start_date  TEXT NOT NULL,
            end_date    TEXT NOT NULL,
            play_time   TEXT NOT NULL,
            days        TEXT NOT NULL DEFAULT 'Daily',
            enabled     INTEGER NOT NULL DEFAULT 1
        );
        ",
    )
}

/// v2: repair guard for databases created before `expires_on` existed.
/// (Mirrors the historical `SchedulerManager::init_tables` ALTER.)
fn m2_scheduler_expires_on(tx: &Transaction<'_>) -> std::result::Result<(), rusqlite::Error> {
    if !has_column(tx, "scheduled_events", "expires_on").map_err(|e| match e {
        CrabError::Database(inner) => inner,
        _ => rusqlite::Error::InvalidPath("column probe failed".into()),
    })? {
        tx.execute_batch("ALTER TABLE scheduled_events ADD COLUMN expires_on TEXT;")?;
    }
    Ok(())
}

/// v3: repair guard for databases created before the track enrichment
/// columns existed. (Mirrors the historical `Library::init_tables`
/// loop, same DDLs.)
fn m3_tracks_extra_columns(tx: &Transaction<'_>) -> std::result::Result<(), rusqlite::Error> {
    const COLS: [(&str, &str); 6] = [
        ("kind", "kind TEXT NOT NULL DEFAULT 'music'"),
        ("daypart_start", "daypart_start INTEGER"),
        ("daypart_end", "daypart_end INTEGER"),
        ("daypart_days", "daypart_days TEXT NOT NULL DEFAULT 'Daily'"),
        ("loudness_lufs", "loudness_lufs REAL"),
        ("loudness_gain_db", "loudness_gain_db REAL"),
    ];
    for (name, ddl) in COLS {
        let missing = !has_column(tx, "tracks", name).map_err(|e| match e {
            CrabError::Database(inner) => inner,
            _ => rusqlite::Error::InvalidPath("column probe failed".into()),
        })?;
        if missing {
            tx.execute_batch(&format!("ALTER TABLE tracks ADD COLUMN {ddl};"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_db(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(name);
        std::fs::remove_file(&p).ok();
        p
    }

    #[test]
    fn fresh_initialize_reaches_current_version_idempotently() {
        let path = tmp_db("crabboss-db-fresh.db");
        Database::initialize(&path).unwrap();
        let conn = Database::open_connection(&path).unwrap();
        let v: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        // Every table the managers expect exists.
        for table in [
            "tracks",
            "tags",
            "play_log",
            "playlists",
            "playlist_items",
            "scheduled_events",
            "carts",
            "ad_blocks",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "missing table {table}");
        }
        // Reopen: idempotent, version untouched, no extra writes fail.
        Database::initialize(&path).unwrap();
        let v2: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v2, SCHEMA_VERSION);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn legacy_pre_versioning_db_migrates_with_data_intact() {
        let path = tmp_db("crabboss-db-legacy.db");
        {
            let conn = Connection::open(&path).unwrap();
            // Schema as written by the old code: no expires_on, bare tracks.
            conn.execute_batch(
                "CREATE TABLE tracks (
                    id TEXT PRIMARY KEY, file_path TEXT NOT NULL UNIQUE,
                    file_name TEXT NOT NULL, title TEXT, artist TEXT,
                    album TEXT, genre TEXT, year INTEGER, track_number INTEGER,
                    duration_secs REAL, bpm REAL, file_size INTEGER,
                    sample_rate INTEGER, channels INTEGER,
                    added_at TEXT NOT NULL, last_played_at TEXT,
                    play_count INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE scheduled_events (
                    id TEXT PRIMARY KEY, name TEXT NOT NULL,
                    action_type TEXT NOT NULL, target TEXT NOT NULL DEFAULT '',
                    start_time TEXT NOT NULL, days TEXT NOT NULL DEFAULT 'Daily',
                    enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL
                );
                INSERT INTO scheduled_events
                    (id, name, action_type, target, start_time, days, enabled, created_at)
                    VALUES ('e1','Morning','play','x.mp3','08:00','Daily',1,'2026-01-01');",
            )
            .unwrap();
        }
        Database::initialize(&path).unwrap();
        let conn = Database::open_connection(&path).unwrap();
        let v: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert!(has_column(&conn, "scheduled_events", "expires_on").unwrap());
        assert!(has_column(&conn, "tracks", "kind").unwrap());
        assert!(has_column(&conn, "tracks", "loudness_gain_db").unwrap());
        // Old row survived with NULL in the new column.
        let (name, expires): (String, Option<String>) = conn
            .query_row(
                "SELECT name, expires_on FROM scheduled_events WHERE id='e1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "Morning");
        assert_eq!(expires, None);
        // All other tables were created around the legacy ones.
        let carts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='carts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(carts, 1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn failed_migration_reports_context_and_bumps_nothing() {
        let path = tmp_db("crabboss-db-bad.db");
        let mut conn = Database::open_connection(&path).unwrap();
        const BAD: &[Migration] = &[Migration {
            version: 1,
            name: "broken",
            apply: |tx| tx.execute_batch("THIS IS NOT SQL;"),
        }];
        let err = apply_pending(&mut conn, 0, BAD).expect_err("bad SQL must fail");
        match err {
            CrabError::Migration { version, name, .. } => {
                assert_eq!(version, 1);
                assert_eq!(name, "broken");
            }
            other => panic!("expected Migration error, got {other:?}"),
        }
        let v: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 0, "version bumps only after success");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn newer_database_is_rejected_with_context() {
        let path = tmp_db("crabboss-db-new.db");
        let conn = Database::open_connection(&path).unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {}", SCHEMA_VERSION + 10))
            .unwrap();
        drop(conn);
        let err = Database::initialize(&path).expect_err("downgrade must fail");
        assert!(matches!(err, CrabError::SchemaTooNew { .. }), "{err:?}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn connections_enforce_foreign_keys_and_cascades() {
        let path = tmp_db("crabboss-db-fk.db");
        Database::initialize(&path).unwrap();
        let conn = Database::open_connection(&path).unwrap();
        let fk: i32 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
        conn.execute(
            "INSERT INTO tracks (id, file_path, file_name, added_at)
             VALUES ('t1','/a.mp3','a.mp3','2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO play_log (track_id, played_at) VALUES ('t1','2026-01-02')",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM tracks WHERE id='t1'", [])
            .unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM play_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "ON DELETE CASCADE must fire");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn busy_timeout_is_bounded() {
        let path = tmp_db("crabboss-db-busy.db");
        Database::initialize(&path).unwrap();
        let conn = Database::open_connection(&path).unwrap();
        let ms: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ms, BUSY_TIMEOUT.as_millis() as i64);
        std::fs::remove_file(&path).ok();
    }

    fn sched_row(name: &str) -> SchedulerRow {
        SchedulerRow {
            name: name.into(),
            action_type: "play".into(),
            target: "x.mp3".into(),
            start_time: "08:00".into(),
            days: "Daily".into(),
            expires_on: None,
            enabled: true,
        }
    }

    fn cart_row(label: &str, position: i32) -> CartRow {
        CartRow {
            label: label.into(),
            file_path: "C:/a.mp3".into(),
            position,
        }
    }

    fn ad_row(name: &str) -> AdBlockRow {
        AdBlockRow {
            name: name.into(),
            spot_path: "C:/s.mp3".into(),
            intro_path: None,
            outro_path: Some("C:/o.mp3".into()),
            start_date: "2026-01-01".into(),
            end_date: "2026-12-31".into(),
            play_time: "09:00".into(),
            days: "Daily".into(),
            enabled: false,
        }
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn replace_swaps_all_lists_together() {
        let path = tmp_db("crabboss-db-replace.db");
        Database::initialize(&path).unwrap();
        let conn = Database::open_connection(&path).unwrap();
        conn.execute(
            "INSERT INTO scheduled_events
             (id, name, action_type, target, start_time, days, enabled, created_at)
             VALUES ('old','Old','play','o.mp3','07:00','Daily',1,'2026-01-01')",
            [],
        )
        .unwrap();
        drop(conn);
        let counts = Database::replace_station_lists(
            &path,
            &[sched_row("New")],
            &[cart_row("Pad", 3)],
            &[ad_row("Break")],
        )
        .unwrap();
        assert_eq!(
            counts,
            ReplaceCounts {
                scheduler: 1,
                carts: 1,
                ads: 1
            }
        );
        let conn = Database::open_connection(&path).unwrap();
        let names: Vec<String> = conn
            .prepare("SELECT name FROM scheduled_events")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(names, vec!["New".to_string()]);
        let (label, pos): (String, i32) = conn
            .query_row("SELECT label, position FROM carts", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((label.as_str(), pos), ("Pad", 3));
        // Disabled flag and empty intro survive the round trip.
        let (enabled, intro): (i32, String) = conn
            .query_row("SELECT enabled, intro_path FROM ad_blocks", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((enabled, intro.as_str()), (0, ""));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn replace_failure_rolls_back_every_list() {
        let path = tmp_db("crabboss-db-rollback.db");
        Database::initialize(&path).unwrap();
        let conn = Database::open_connection(&path).unwrap();
        conn.execute(
            "INSERT INTO scheduled_events
             (id, name, action_type, target, start_time, days, enabled, created_at)
             VALUES ('keep','Keep','play','k.mp3','07:00','Daily',1,'2026-01-01')",
            [],
        )
        .unwrap();
        // Sabotage one table: the multi-statement replace must abort and
        // leave the other lists exactly as they were.
        conn.execute_batch("DROP TABLE carts;").unwrap();
        drop(conn);
        let err = Database::replace_station_lists(
            &path,
            &[sched_row("New")],
            &[cart_row("Pad", 0)],
            &[ad_row("Break")],
        )
        .expect_err("missing table must fail the replace");
        assert!(matches!(err, CrabError::BulkReplace(_)), "{err:?}");
        let conn = Database::open_connection(&path).unwrap();
        assert_eq!(count(&conn, "scheduled_events"), 1);
        let name: String = conn
            .query_row("SELECT name FROM scheduled_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "Keep");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn replace_cart_positions_are_last_wins() {
        let path = tmp_db("crabboss-db-lastwins.db");
        Database::initialize(&path).unwrap();
        Database::replace_station_lists(
            &path,
            &[],
            &[cart_row("First", 0), cart_row("Second", 0)],
            &[],
        )
        .unwrap();
        let conn = Database::open_connection(&path).unwrap();
        assert_eq!(count(&conn, "carts"), 1);
        let label: String = conn
            .query_row("SELECT label FROM carts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(label, "Second");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn replace_rejects_bad_position_without_touching_anything() {
        let path = tmp_db("crabboss-db-badpos.db");
        Database::initialize(&path).unwrap();
        let conn = Database::open_connection(&path).unwrap();
        conn.execute(
            "INSERT INTO scheduled_events
             (id, name, action_type, target, start_time, days, enabled, created_at)
             VALUES ('keep','Keep','play','k.mp3','07:00','Daily',1,'2026-01-01')",
            [],
        )
        .unwrap();
        drop(conn);
        let err = Database::replace_station_lists(&path, &[], &[cart_row("Bad", 99)], &[])
            .expect_err("out-of-range pad must fail closed");
        assert!(err.to_string().contains("bad cart position 99"));
        let conn = Database::open_connection(&path).unwrap();
        assert_eq!(count(&conn, "scheduled_events"), 1);
        assert_eq!(count(&conn, "carts"), 0);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn invalid_path_is_an_error_not_a_panic() {
        let bad = Path::new("/no/such/dir/crabboss.db");
        // Windows-safe variant of the same probe.
        let bad = if cfg!(windows) {
            PathBuf::from("Z:\\no\\such\\dir\\crabboss.db")
        } else {
            bad.to_path_buf()
        };
        assert!(Database::open_connection(&bad).is_err());
        assert!(Database::initialize(&bad).is_err());
    }
}
