//! SQLite persistence: projects, per-project layouts, and small app state.
//!
//! Three rules:
//!
//! - **Every write touches only the rows it means to.** There is no "load the whole set, swap one
//!   entry, delete everything and re-insert" path, which is how concurrent saves overwrote each
//!   other and deleted records came back.
//! - **An unreadable row is kept, never overwritten.** A layout this build cannot read is copied to
//!   a quarantine table before anything replaces it, and a project row that fails to
//!   parse is reported and left alone rather than dropped by the next save.
//! - **Migrations are idempotent and stamped atomically.** Each step and its `user_version` commit in
//!   one transaction, and every step can be re-run safely.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use thiserror::Error;
use throng_core::ids::ProjectId;
use throng_core::project::{Colour, Project};
use throng_core::workspace::{Layout, LayoutError};

/// A store failure.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("the database was written by a newer throng (schema {found}, this build knows {known})")]
    NewerSchema { found: i64, known: i64 },
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// One migration step. Every statement must be safe to run twice.
struct Migration {
    version: i64,
    apply: fn(&Transaction<'_>) -> rusqlite::Result<()>,
}

const MIGRATIONS: &[Migration] = &[Migration { version: 1, apply: migrate_v1 }];

/// The schema version this build writes.
pub const SCHEMA_VERSION: i64 = 1;

fn migrate_v1(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS projects (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            colour TEXT NOT NULL,
            root TEXT NOT NULL,
            hidden_paths TEXT NOT NULL DEFAULT '[]',
            position INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS layouts (
            project_id TEXT PRIMARY KEY,
            doc TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS layouts_quarantine (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id TEXT NOT NULL,
            doc TEXT NOT NULL,
            reason TEXT NOT NULL,
            quarantined_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS app_state (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;
    // Columns added after a table first shipped go through `add_column` so a re-run is a no-op.
    add_column(tx, "projects", "hidden_paths", "TEXT NOT NULL DEFAULT '[]'")?;
    add_column(tx, "projects", "position", "INTEGER NOT NULL DEFAULT 0")?;
    Ok(())
}

/// `ALTER TABLE … ADD COLUMN` only if the column is missing.
fn add_column(tx: &Transaction<'_>, table: &str, column: &str, decl: &str) -> rusqlite::Result<()> {
    let exists: bool = tx
        .prepare(&format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"))?
        .exists([column])?;
    if !exists {
        tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

/// How a stored layout came back.
#[derive(Debug, PartialEq)]
pub enum LayoutLoad {
    /// Nothing stored for this project.
    Missing,
    Loaded(Layout),
    /// Stored but unreadable by this build. The raw document is returned so the caller can
    /// quarantine it before writing anything else.
    Unreadable {
        raw: String,
        error: LayoutError,
    },
}

/// A project row that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadRow {
    pub id: String,
    pub reason: String,
}

/// The store.
pub struct Store {
    conn: Connection,
    path: Option<PathBuf>,
}

impl Store {
    /// Open (creating if needed) and migrate the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn, Some(path.to_path_buf()))
    }

    /// A private in-memory store (tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(mut conn: Connection, path: Option<PathBuf>) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if found > SCHEMA_VERSION {
            return Err(StoreError::NewerSchema { found, known: SCHEMA_VERSION });
        }
        for migration in MIGRATIONS.iter().filter(|m| m.version > found) {
            let tx = conn.transaction()?;
            (migration.apply)(&tx)?;
            tx.pragma_update(None, "user_version", migration.version)?;
            tx.commit()?;
            tracing::info!(version = migration.version, "applied database migration");
        }
        Ok(Self { conn, path })
    }

    /// Where the database lives, if on disk.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Current schema version.
    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    /// Every readable project in display order, plus the rows that could not be read.
    pub fn projects(&self) -> Result<(Vec<Project>, Vec<BadRow>)> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, colour, root, hidden_paths, created_at, updated_at
             FROM projects ORDER BY position, created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })?;
        let mut good = Vec::new();
        let mut bad = Vec::new();
        for row in rows {
            let (id, name, colour, root, hidden, created_at, updated_at) = row?;
            let parsed = (|| -> std::result::Result<Project, String> {
                Ok(Project {
                    id: id.parse().map_err(|e| format!("bad id: {e}"))?,
                    name,
                    colour: Colour::parse(&colour).map_err(|e| e.to_string())?,
                    root: PathBuf::from(root),
                    hidden_paths: serde_json::from_str(&hidden)
                        .map_err(|e| format!("bad hidden paths: {e}"))?,
                    created_at,
                    updated_at,
                })
            })();
            match parsed {
                Ok(project) => good.push(project),
                Err(reason) => {
                    tracing::warn!(%id, %reason, "skipping unreadable project row");
                    bad.push(BadRow { id, reason });
                }
            }
        }
        Ok((good, bad))
    }

    /// Insert or update one project at `position`.
    pub fn upsert_project(&self, project: &Project, position: usize) -> Result<()> {
        let hidden = serde_json::to_string(&project.hidden_paths).expect("strings serialise");
        self.conn.execute(
            "INSERT INTO projects (id, name, colour, root, hidden_paths, position, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET name = ?2, colour = ?3, root = ?4, hidden_paths = ?5,
                 position = ?6, updated_at = ?8",
            params![
                project.id.to_string(),
                project.name,
                project.colour.to_hex(),
                project.root.to_string_lossy(),
                hidden,
                position as i64,
                project.created_at,
                project.updated_at
            ],
        )?;
        Ok(())
    }

    /// Record display order for the given ids (other rows keep theirs).
    pub fn set_positions(&mut self, ordered: &[ProjectId]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for (position, id) in ordered.iter().enumerate() {
            tx.execute(
                "UPDATE projects SET position = ?1 WHERE id = ?2",
                params![position as i64, id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Delete one project and its layout.
    pub fn delete_project(&mut self, id: ProjectId) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM layouts WHERE project_id = ?1", [id.to_string()])?;
        tx.execute("DELETE FROM projects WHERE id = ?1", [id.to_string()])?;
        tx.execute("DELETE FROM app_state WHERE key = ?1", [format!("{FILE_HISTORY_KEY_PREFIX}{id}")])?;
        tx.commit()?;
        Ok(())
    }

    /// The stored layout for a project.
    pub fn layout(&self, project: ProjectId) -> Result<LayoutLoad> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT doc FROM layouts WHERE project_id = ?1", [project.to_string()], |r| r.get(0))
            .optional()?;
        Ok(match raw {
            None => LayoutLoad::Missing,
            Some(raw) => match Layout::from_json(&raw) {
                Ok(layout) => LayoutLoad::Loaded(layout),
                Err(error) => LayoutLoad::Unreadable { raw, error },
            },
        })
    }

    /// Store a project's layout (one row).
    pub fn save_layout(&self, project: ProjectId, layout: &Layout, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO layouts (project_id, doc, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET doc = ?2, updated_at = ?3",
            params![project.to_string(), layout.to_json(), now],
        )?;
        Ok(())
    }

    /// Keep an unreadable layout aside so replacing it loses nothing.
    pub fn quarantine_layout(&self, project: ProjectId, raw: &str, reason: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO layouts_quarantine (project_id, doc, reason, quarantined_at) VALUES (?1, ?2, ?3, ?4)",
            params![project.to_string(), raw, reason, now],
        )?;
        Ok(())
    }

    /// How many layouts are in quarantine for a project.
    pub fn quarantined(&self, project: ProjectId) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM layouts_quarantine WHERE project_id = ?1",
            [project.to_string()],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Read a small piece of app state.
    pub fn state(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM app_state WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    /// Every app-state entry whose key starts with `prefix`, with the prefix removed.
    pub fn states_with_prefix(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT key, value FROM app_state WHERE substr(key, 1, ?2) = ?1")?;
        let rows = stmt.query_map(params![prefix, prefix.len() as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (key, value) = row?;
            out.push((key[prefix.len()..].to_owned(), value));
        }
        Ok(out)
    }

    /// Write a small piece of app state; `None` deletes it.
    pub fn set_state(&self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => self.conn.execute(
                "INSERT INTO app_state (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
                params![key, v],
            )?,
            None => self.conn.execute("DELETE FROM app_state WHERE key = ?1", [key])?,
        };
        Ok(())
    }
}

/// App-state key for the active project id.
pub const ACTIVE_PROJECT_KEY: &str = "activeProject";
/// App-state key prefix for a file's hand-picked language, followed by the file's path.
pub const LANGUAGE_KEY_PREFIX: &str = "language:";
/// App-state keys holding a project's file-operation history, followed by the project id.
/// Deleted with the project, so histories never pile up.
pub const FILE_HISTORY_KEY_PREFIX: &str = "file-history:";
/// App-state key for the sub-workspace list (each one's layout is stored under its own id, as a
/// project's is).
pub const SUB_WORKSPACES_KEY: &str = "subWorkspaces";

#[cfg(test)]
mod tests {
    use super::*;
    use throng_core::project::{ProjectBook, ProjectInput};
    use throng_core::workspace::PanelKind;

    fn sample(book: &mut ProjectBook, name: &str, root: &str) -> Project {
        // A root must be absolute, and on Windows that takes a drive.
        let root = if cfg!(windows) { format!("C:{root}") } else { root.to_owned() };
        book.create(
            &throng_core::paths::PathRules::LINUX,
            &ProjectInput { name: name.into(), colour: "#4cc38a".into(), root: root.into() },
            1_000,
        )
        .unwrap()
        .clone()
    }

    #[test]
    fn projects_round_trip_in_order() {
        let mut store = Store::open_in_memory().unwrap();
        let mut book = ProjectBook::default();
        let a = sample(&mut book, "A", "/a");
        let b = sample(&mut book, "B", "/b");
        store.upsert_project(&a, 0).unwrap();
        store.upsert_project(&b, 1).unwrap();
        store.set_positions(&[b.id, a.id]).unwrap();
        let (projects, bad) = store.projects().unwrap();
        assert!(bad.is_empty());
        assert_eq!(projects, vec![b.clone(), a.clone()]);
        store.delete_project(a.id).unwrap();
        assert_eq!(store.projects().unwrap().0, vec![b]);
    }

    #[test]
    fn unreadable_rows_are_reported_and_survive_other_writes() {
        let store = Store::open_in_memory().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO projects (id, name, colour, root, created_at, updated_at) VALUES ('nope', 'X', 'blue', '/x', 0, 0)",
                [],
            )
            .unwrap();
        let mut book = ProjectBook::default();
        let a = sample(&mut book, "A", "/a");
        store.upsert_project(&a, 0).unwrap();
        let (projects, bad) = store.projects().unwrap();
        assert_eq!(projects, vec![a]);
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].id, "nope");
        let count: i64 = store.conn.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2, "the unreadable row must not be deleted");
    }

    #[test]
    fn layouts_round_trip_and_unreadable_ones_can_be_quarantined() {
        let store = Store::open_in_memory().unwrap();
        let project = ProjectId::new();
        assert_eq!(store.layout(project).unwrap(), LayoutLoad::Missing);
        let mut layout = Layout::new_default(project);
        layout.add_tab(project, PanelKind::Untyped);
        store.save_layout(project, &layout, 5).unwrap();
        assert_eq!(store.layout(project).unwrap(), LayoutLoad::Loaded(layout));

        store
            .conn
            .execute(
                "UPDATE layouts SET doc = '{\"version\": 42}' WHERE project_id = ?1",
                [project.to_string()],
            )
            .unwrap();
        let LayoutLoad::Unreadable { raw, error } = store.layout(project).unwrap() else {
            panic!("expected unreadable")
        };
        assert_eq!(error, LayoutError::NewerVersion(42));
        store.quarantine_layout(project, &raw, &error.to_string(), 6).unwrap();
        assert_eq!(store.quarantined(project).unwrap(), 1);
    }

    #[test]
    fn app_state_can_be_listed_by_prefix() {
        let store = Store::open_in_memory().unwrap();
        store.set_state("language:/a.txt", Some("Markdown")).unwrap();
        store.set_state("language:/b", Some("Rust")).unwrap();
        store.set_state("languages", Some("not me")).unwrap();
        let mut found = store.states_with_prefix(LANGUAGE_KEY_PREFIX).unwrap();
        found.sort();
        assert_eq!(found, vec![("/a.txt".into(), "Markdown".into()), ("/b".into(), "Rust".into())]);
    }

    #[test]
    fn app_state_round_trips() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.state(ACTIVE_PROJECT_KEY).unwrap(), None);
        store.set_state(ACTIVE_PROJECT_KEY, Some("x")).unwrap();
        store.set_state(ACTIVE_PROJECT_KEY, Some("y")).unwrap();
        assert_eq!(store.state(ACTIVE_PROJECT_KEY).unwrap().as_deref(), Some("y"));
        store.set_state(ACTIVE_PROJECT_KEY, None).unwrap();
        assert_eq!(store.state(ACTIVE_PROJECT_KEY).unwrap(), None);
    }

    #[test]
    fn migrations_are_idempotent_and_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        }
        // Simulate a kill between a step's commit and its stamp: the step runs again and must
        // succeed.
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", 0).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn a_newer_database_is_refused_not_downgraded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
        }
        assert!(matches!(Store::open(&path), Err(StoreError::NewerSchema { .. })));
    }
}
