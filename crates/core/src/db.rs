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

impl Database {
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
