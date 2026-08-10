//! The controller's own durable state: the cross-session task DAG plus
//! runtime-event dedupe. SQLite only — one controller process owns the file.
// ponytail: sync rusqlite behind a Mutex; queries are single-row on a
// controller-sized table. Pool/postgres when a second replica exists.

use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    Done,
    Failed,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "done" => Self::Done,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub ns: String,
    pub id: String,
    pub assignee: String,
    pub deps: Vec<String>,
    pub status: TaskStatus,
    pub spec: String,
    pub result: Option<String>,
    pub created_by: String,
    pub session_id: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InsertError {
    DuplicateId(String),
    UnknownDep(String),
    SelfDep(String),
}

impl std::fmt::Display for InsertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "task '{id}' already exists"),
            Self::UnknownDep(id) => write!(f, "unknown dep '{id}'"),
            Self::SelfDep(id) => write!(f, "task '{id}' cannot depend on itself"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventAdmission {
    New,
    Duplicate,
    Conflict,
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                ns TEXT NOT NULL,
                id TEXT NOT NULL,
                assignee TEXT NOT NULL,
                deps TEXT NOT NULL,
                status TEXT NOT NULL,
                spec TEXT NOT NULL,
                result TEXT,
                created_by TEXT NOT NULL,
                session_id TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (ns, id)
            );
            CREATE UNIQUE INDEX IF NOT EXISTS tasks_session
                ON tasks(session_id) WHERE session_id IS NOT NULL;
            CREATE TABLE IF NOT EXISTS runtime_events (
                event_id TEXT PRIMARY KEY,
                body_sha256 TEXT NOT NULL,
                received_at INTEGER NOT NULL
            );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// At-least-once delivery: dedupe by event id, conflict on a different
    /// body under a replayed id.
    pub fn record_runtime_event(
        &self,
        event_id: &str,
        body_sha256: &str,
    ) -> rusqlite::Result<EventAdmission> {
        let conn = self.conn.lock().unwrap();
        let existing: Option<String> = conn
            .query_row(
                "SELECT body_sha256 FROM runtime_events WHERE event_id = ?1",
                params![event_id],
                |row| row.get(0),
            )
            .optional()?;
        match existing {
            Some(hash) if hash == body_sha256 => Ok(EventAdmission::Duplicate),
            Some(_) => Ok(EventAdmission::Conflict),
            None => {
                conn.execute(
                    "INSERT INTO runtime_events (event_id, body_sha256, received_at)
                     VALUES (?1, ?2, ?3)",
                    params![event_id, body_sha256, now_unix()],
                )?;
                Ok(EventAdmission::New)
            }
        }
    }

    /// Insert-time DAG validation: deps must exist in the namespace, no
    /// self-dep. (Existing-deps-only already rules out cycles among tasks
    /// inserted through this path; full cycle detection when an edit surface
    /// appears.)
    pub fn insert_task(&self, task: &Task) -> rusqlite::Result<Result<(), InsertError>> {
        let conn = self.conn.lock().unwrap();
        for dep in &task.deps {
            if *dep == task.id {
                return Ok(Err(InsertError::SelfDep(dep.clone())));
            }
            let exists: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM tasks WHERE ns = ?1 AND id = ?2",
                    params![task.ns, dep],
                    |row| row.get(0),
                )
                .optional()?;
            if exists.is_none() {
                return Ok(Err(InsertError::UnknownDep(dep.clone())));
            }
        }
        let now = now_unix();
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO tasks
                (ns, id, assignee, deps, status, spec, result, created_by,
                 session_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, NULL, ?8, ?8)",
            params![
                task.ns,
                task.id,
                task.assignee,
                serde_json::to_string(&task.deps).expect("string vec"),
                task.status.as_str(),
                task.spec,
                task.created_by,
                now
            ],
        )?;
        if inserted == 0 {
            return Ok(Err(InsertError::DuplicateId(task.id.clone())));
        }
        Ok(Ok(()))
    }

    pub fn task(&self, ns: &str, id: &str) -> rusqlite::Result<Option<Task>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("{SELECT_TASK} WHERE ns = ?1 AND id = ?2"),
            params![ns, id],
            row_to_task,
        )
        .optional()
    }

    pub fn tasks_in_ns(&self, ns: &str) -> rusqlite::Result<Vec<Task>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "{SELECT_TASK} WHERE ns = ?1 ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map(params![ns], row_to_task)?;
        rows.collect()
    }

    pub fn task_by_session(&self, session_id: &str) -> rusqlite::Result<Option<Task>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("{SELECT_TASK} WHERE session_id = ?1"),
            params![session_id],
            row_to_task,
        )
        .optional()
    }

    pub fn mark_running(&self, ns: &str, id: &str, session_id: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE tasks SET status = 'running', session_id = ?3, updated_at = ?4
             WHERE ns = ?1 AND id = ?2",
            params![ns, id, session_id, now_unix()],
        )?;
        Ok(())
    }

    /// Terminal transition. Guarded on `running` so a redelivered terminal
    /// event cannot rewrite an already-settled task.
    pub fn mark_terminal(
        &self,
        ns: &str,
        id: &str,
        status: TaskStatus,
        result: Option<&str>,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let changed = conn.execute(
            "UPDATE tasks SET status = ?3, result = ?4, updated_at = ?5
             WHERE ns = ?1 AND id = ?2 AND status IN ('running', 'pending')",
            params![ns, id, status.as_str(), result, now_unix()],
        )?;
        Ok(changed > 0)
    }

    /// Namespaces that still have pending tasks — the periodic scheduler
    /// tick's worklist (retry path for a failed `open_session`).
    pub fn pending_namespaces(&self) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT DISTINCT ns FROM tasks WHERE status = 'pending'")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect()
    }
}

const SELECT_TASK: &str = "SELECT ns, id, assignee, deps, status, spec, result, created_by, \
                           session_id FROM tasks";

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    let deps: String = row.get(3)?;
    let status: String = row.get(4)?;
    Ok(Task {
        ns: row.get(0)?,
        id: row.get(1)?,
        assignee: row.get(2)?,
        deps: serde_json::from_str(&deps).unwrap_or_default(),
        status: TaskStatus::parse(&status),
        spec: row.get(5)?,
        result: row.get(6)?,
        created_by: row.get(7)?,
        session_id: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, deps: &[&str]) -> Task {
        Task {
            ns: "demo".into(),
            id: id.into(),
            assignee: "worker".into(),
            deps: deps.iter().map(|d| d.to_string()).collect(),
            status: TaskStatus::Pending,
            spec: format!("spec for {id}"),
            result: None,
            created_by: "b0".into(),
            session_id: None,
        }
    }

    #[test]
    fn insert_validates_deps_and_uniqueness() {
        let store = Store::memory().unwrap();
        assert_eq!(store.insert_task(&task("b1", &[])).unwrap(), Ok(()));
        assert_eq!(
            store.insert_task(&task("b1", &[])).unwrap(),
            Err(InsertError::DuplicateId("b1".into()))
        );
        assert_eq!(
            store.insert_task(&task("b2", &["ghost"])).unwrap(),
            Err(InsertError::UnknownDep("ghost".into()))
        );
        assert_eq!(
            store.insert_task(&task("b2", &["b2"])).unwrap(),
            Err(InsertError::SelfDep("b2".into()))
        );
        assert_eq!(store.insert_task(&task("b2", &["b1"])).unwrap(), Ok(()));
        let loaded = store.task("demo", "b2").unwrap().unwrap();
        assert_eq!(loaded.deps, vec!["b1".to_string()]);
        assert_eq!(loaded.status, TaskStatus::Pending);
    }

    #[test]
    fn terminal_transition_is_guarded_and_session_lookup_works() {
        let store = Store::memory().unwrap();
        store.insert_task(&task("b1", &[])).unwrap().unwrap();
        store.mark_running("demo", "b1", "ses_1").unwrap();
        assert_eq!(store.task_by_session("ses_1").unwrap().unwrap().id, "b1");
        assert!(store
            .mark_terminal("demo", "b1", TaskStatus::Done, Some("answer"))
            .unwrap());
        // Redelivered terminal event: already settled, no rewrite.
        assert!(!store
            .mark_terminal("demo", "b1", TaskStatus::Failed, None)
            .unwrap());
        let done = store.task("demo", "b1").unwrap().unwrap();
        assert_eq!(done.status, TaskStatus::Done);
        assert_eq!(done.result.as_deref(), Some("answer"));
    }

    #[test]
    fn event_dedupe_admits_by_id_and_body() {
        let store = Store::memory().unwrap();
        assert_eq!(
            store.record_runtime_event("cev_1", "aaa").unwrap(),
            EventAdmission::New
        );
        assert_eq!(
            store.record_runtime_event("cev_1", "aaa").unwrap(),
            EventAdmission::Duplicate
        );
        assert_eq!(
            store.record_runtime_event("cev_1", "bbb").unwrap(),
            EventAdmission::Conflict
        );
    }
}
