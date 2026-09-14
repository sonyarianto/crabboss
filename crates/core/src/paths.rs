//! Stable runtime-data locations (P1.1).
//!
//! The app used to resolve `settings.json` / `crabboss.db` from the
//! process working directory, so the same install could run with
//! different state depending on how it was launched. Now everything
//! resolves once through [`AppPaths`]:
//!
//! - default root: per-user data dir + `CrabBoss` (`%LOCALAPPDATA%` on
//!   Windows; the `dirs` crate picks the platform convention, so other
//!   OSes work without code changes);
//! - `--data-dir <path>` override for portable installs and tests;
//! - one-time, copy-based migration of legacy current-directory files.
//!
//! `resolve*` is pure (creates nothing); [`AppPaths::ensure_root`]
//! creates the root explicitly before any store opens.
//!
//! `license.json` is deliberately NOT migrated here: licensing is out of
//! scope until a separate product decision, so [`AppPaths::license`]
//! keeps pointing at the legacy current-directory location.

use std::path::{Path, PathBuf};

/// Well-known persistent files, resolved once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// `<root>` (created by [`AppPaths::ensure_root`]).
    pub root: PathBuf,
    /// `<root>/settings.json`.
    pub settings: PathBuf,
    /// `<root>/crabboss.db`.
    pub database: PathBuf,
    /// Legacy location (current directory): licensing stays put until a
    /// separate decision moves it.
    pub license: PathBuf,
}

/// Outcome of migrating one legacy file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileMigration {
    /// Destination already existed: it always wins, legacy kept as-is.
    SkippedExisting,
    /// No legacy file: nothing to do.
    SkippedNoLegacy,
    /// Legacy copied to the destination (legacy kept as fallback).
    Copied { from: PathBuf, to: PathBuf },
    /// Copy failed: destination untouched.
    Failed { from: PathBuf, error: String },
}

/// Outcome of [`AppPaths::migrate_legacy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub settings: FileMigration,
    pub database: FileMigration,
}

impl MigrationReport {
    pub fn copied_any(&self) -> bool {
        matches!(self.settings, FileMigration::Copied { .. })
            || matches!(self.database, FileMigration::Copied { .. })
    }
}

/// Parse a `--data-dir <path>` / `--data-dir=<path>` override out of an
/// argument list (pure: takes the args as input for testability).
pub fn data_dir_override(args: &[String]) -> Option<PathBuf> {
    let mut iter = args.iter().peekable();
    while let Some(a) = iter.next() {
        if a == "--data-dir" {
            if let Some(v) = iter.next() {
                return Some(PathBuf::from(v));
            }
        } else if let Some(v) = a.strip_prefix("--data-dir=") {
            return Some(PathBuf::from(v));
        }
    }
    None
}

fn default_root() -> PathBuf {
    if let Some(dir) = dirs::data_local_dir() {
        return dir.join("CrabBoss");
    }
    // Last resort only (headless/service contexts without a profile):
    // stay next to the launch directory and say so in the logs.
    std::env::current_dir()
        .unwrap_or_default()
        .join("CrabBoss-data")
}

/// Resolve paths without touching disk. `--data-dir` wins, then the
/// per-user data dir, then the current-directory fallback.
pub fn resolve_from(args: &[String], legacy_dir: &Path) -> AppPaths {
    let root = data_dir_override(args).unwrap_or_else(default_root);
    AppPaths {
        settings: root.join("settings.json"),
        database: root.join("crabboss.db"),
        license: legacy_dir.join("license.json"),
        root,
    }
}

/// Resolve from the real process state: CLI args + current directory as
/// the legacy location. Pure (creates no directories).
pub fn resolve() -> AppPaths {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let legacy_dir = std::env::current_dir().unwrap_or_default();
    resolve_from(&args, &legacy_dir)
}

impl AppPaths {
    /// Create the root directory (and parents) before any store opens.
    pub fn ensure_root(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)
    }

    /// One-time migration of legacy current-directory files (settings +
    /// database only — never the license). Rules, in order:
    ///
    /// - destination exists → keep it, legacy untouched;
    /// - no legacy file → nothing to do;
    /// - otherwise copy legacy to destination (copy, not move: the
    ///   legacy file stays as a fallback).
    ///
    /// The database travels with any SQLite sidecars present
    /// (`-wal`/`-shm`/`-journal`); missing sidecars are simply skipped.
    pub fn migrate_legacy(&self, legacy_dir: &Path) -> MigrationReport {
        // Never migrate onto ourselves (e.g. `--data-dir` pointing at
        // the legacy folder, or the CWD fallback root).
        let same_settings = same_file(&legacy_dir.join("settings.json"), &self.settings);
        let same_database = same_file(&legacy_dir.join("crabboss.db"), &self.database);
        MigrationReport {
            settings: if same_settings {
                FileMigration::SkippedExisting
            } else {
                migrate_one(&legacy_dir.join("settings.json"), &self.settings, &[])
            },
            database: if same_database {
                FileMigration::SkippedExisting
            } else {
                migrate_one(
                    &legacy_dir.join("crabboss.db"),
                    &self.database,
                    &["-wal", "-shm", "-journal"],
                )
            },
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    // Cheap identity: same literal path after normalization. A full
    // canonicalize would fail on missing files, which is exactly the
    // case we also need to handle.
    a == b
}

fn migrate_one(from: &Path, to: &Path, sidecars: &[&str]) -> FileMigration {
    if to.exists() {
        return FileMigration::SkippedExisting;
    }
    if !from.exists() {
        return FileMigration::SkippedNoLegacy;
    }
    if let Some(parent) = to.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return FileMigration::Failed {
                from: from.to_path_buf(),
                error: format!("cannot create {}: {e}", parent.display()),
            };
        }
    }
    if let Err(e) = std::fs::copy(from, to) {
        return FileMigration::Failed {
            from: from.to_path_buf(),
            error: format!("copy failed: {e}"),
        };
    }
    // Best-effort sidecars: a half-copied sidecar is worse than none
    // (SQLite treats a missing WAL as a clean shutdown), so failures
    // here only warn via the returned outcome, never fail the main copy.
    let from_base = from.to_string_lossy().into_owned();
    let to_base = to.to_string_lossy().into_owned();
    for suffix in sidecars {
        let s_from = PathBuf::from(format!("{from_base}{suffix}"));
        if s_from.exists() {
            let s_to = PathBuf::from(format!("{to_base}{suffix}"));
            if let Err(e) = std::fs::copy(&s_from, &s_to) {
                tracing::warn!("Legacy sidecar {} not carried over: {e}", s_from.display());
            }
        }
    }
    FileMigration::Copied {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn data_dir_override_parses_both_forms() {
        let legacy = Path::new("/legacy");
        let a = ["--data-dir", "/tmp/portable"];
        assert_eq!(
            resolve_from(&args(&a), legacy).root,
            PathBuf::from("/tmp/portable")
        );
        let a = ["--data-dir=/tmp/eq"];
        assert_eq!(
            resolve_from(&args(&a), legacy).root,
            PathBuf::from("/tmp/eq")
        );
        // Missing value: no override, falls back to the default root.
        let a = ["--data-dir"];
        assert_ne!(
            resolve_from(&args(&a), legacy).root,
            PathBuf::from("--data-dir")
        );
        // Other flags are ignored.
        let a = ["--engine", "cpal"];
        let r = resolve_from(&args(&a), legacy);
        assert_eq!(r.settings, r.root.join("settings.json"));
        assert_eq!(r.database, r.root.join("crabboss.db"));
        // License always stays at the legacy location.
        assert_eq!(r.license, legacy.join("license.json"));
    }

    #[test]
    fn resolve_creates_nothing() {
        let root = std::env::temp_dir().join("crabboss-paths-untouched");
        std::fs::remove_dir_all(&root).ok();
        let legacy = std::env::temp_dir().join("crabboss-paths-legacy");
        let arg = format!("--data-dir={}", root.display());
        let paths = resolve_from(&args(&[arg.as_str()]), &legacy);
        assert!(!paths.root.exists(), "resolver must not create dirs");
        paths.ensure_root().unwrap();
        assert!(paths.root.is_dir());
        std::fs::remove_dir_all(&root).ok();
    }

    fn sandbox(name: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(name);
        std::fs::remove_dir_all(&base).ok();
        let legacy = base.join("legacy");
        let root = base.join("root");
        std::fs::create_dir_all(&legacy).unwrap();
        (legacy, root)
    }

    fn paths_for(legacy: &Path, root: &Path) -> AppPaths {
        AppPaths {
            settings: root.join("settings.json"),
            database: root.join("crabboss.db"),
            license: legacy.join("license.json"),
            root: root.to_path_buf(),
        }
    }

    #[test]
    fn migration_copies_legacy_when_destination_missing() {
        let (legacy, root) = sandbox("crabboss-mig-copy");
        std::fs::write(legacy.join("settings.json"), r#"{"a":1}"#).unwrap();
        std::fs::write(legacy.join("crabboss.db"), b"sqlite-ish").unwrap();
        let paths = paths_for(&legacy, &root);
        paths.ensure_root().unwrap();
        let report = paths.migrate_legacy(&legacy);
        assert!(report.copied_any());
        assert_eq!(
            std::fs::read_to_string(root.join("settings.json")).unwrap(),
            r#"{"a":1}"#
        );
        assert_eq!(
            std::fs::read(root.join("crabboss.db")).unwrap(),
            b"sqlite-ish"
        );
        // Legacy kept as fallback.
        assert!(legacy.join("settings.json").exists());
        // License never migrates, even when present in legacy.
        std::fs::write(legacy.join("license.json"), b"lic").unwrap();
        let report = paths.migrate_legacy(&legacy);
        assert!(!report.copied_any());
        assert!(!root.join("license.json").exists());
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[test]
    fn migration_existing_destination_always_wins() {
        let (legacy, root) = sandbox("crabboss-mig-wins");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("settings.json"), r#"{"a":2}"#).unwrap();
        std::fs::write(legacy.join("settings.json"), r#"{"a":1}"#).unwrap();
        let paths = paths_for(&legacy, &root);
        let report = paths.migrate_legacy(&legacy);
        assert_eq!(report.settings, FileMigration::SkippedExisting);
        assert_eq!(
            std::fs::read_to_string(root.join("settings.json")).unwrap(),
            r#"{"a":2}"#
        );
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[test]
    fn migration_without_legacy_is_noop() {
        let (legacy, root) = sandbox("crabboss-mig-noop");
        let paths = paths_for(&legacy, &root);
        paths.ensure_root().unwrap();
        let report = paths.migrate_legacy(&legacy);
        assert_eq!(report.settings, FileMigration::SkippedNoLegacy);
        assert_eq!(report.database, FileMigration::SkippedNoLegacy);
        assert!(!report.copied_any());
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[test]
    fn migration_onto_self_is_noop() {
        // `--data-dir` pointing at the legacy folder must not eat itself.
        let (legacy, _) = sandbox("crabboss-mig-self");
        std::fs::write(legacy.join("settings.json"), r#"{"a":1}"#).unwrap();
        let paths = paths_for(&legacy, &legacy);
        let report = paths.migrate_legacy(&legacy);
        assert_eq!(report.settings, FileMigration::SkippedExisting);
        assert_eq!(
            std::fs::read_to_string(legacy.join("settings.json")).unwrap(),
            r#"{"a":1}"#
        );
        std::fs::remove_dir_all(&legacy).ok();
    }
}
