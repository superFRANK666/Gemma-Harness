use anyhow::{bail, Context, Result};
use harness_model::ChatMessage;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, sync::Mutex};

pub struct Store {
    db: Mutex<Connection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub title: String,
    pub workspace: String,
    pub device_id: String,
    pub connection_id: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub workspace: String,
    pub device_id: String,
    pub created_at: String,
    pub updated_at: String,
}

fn project_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get(0)?,
        name: row.get(1)?,
        workspace: row.get(2)?,
        device_id: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn save_setting_on(db: &Connection, value: &Value) -> Result<()> {
    db.execute("INSERT INTO settings(id,payload) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload", [value.to_string()])?;
    Ok(())
}

fn register_project_on(db: &Connection, workspace: &str, device_id: &str) -> Result<Project> {
    // Workspace bindings are opaque: legacy and remote paths need not exist locally.
    let name = workspace
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(workspace);
    let now = chrono::Utc::now().to_rfc3339();
    db.execute(
        "INSERT INTO projects(id,name,workspace,device_id,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?5) ON CONFLICT(device_id,workspace) DO UPDATE SET updated_at=excluded.updated_at",
        params![uuid::Uuid::new_v4().to_string(), name, workspace, device_id, now],
    )?;
    Ok(db.query_row(
        "SELECT id,name,workspace,device_id,created_at,updated_at FROM projects WHERE device_id=?1 AND workspace=?2",
        params![device_id, workspace],
        project_row,
    )?)
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.create_scalar_function(
            "harness_unicode_instr",
            2,
            rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| {
                let source = ctx.get::<String>(0)?;
                let needle = ctx.get::<String>(1)?;
                let folded = source.to_lowercase();
                let Some(offset) = folded.find(&needle) else {
                    return Ok(0i64);
                };
                // SQLite substr uses character offsets, while Rust's find uses byte
                // offsets. Case conversion may expand one source character.
                let mut folded_bytes = 0usize;
                for (index, ch) in source.chars().enumerate() {
                    if folded_bytes >= offset {
                        return Ok(index as i64 + 1);
                    }
                    folded_bytes += ch.to_lowercase().map(char::len_utf8).sum::<usize>();
                    if folded_bytes > offset {
                        return Ok(index as i64 + 1);
                    }
                }
                Ok(source.chars().count() as i64 + 1)
            },
        )?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,title TEXT NOT NULL,workspace TEXT NOT NULL,device_id TEXT NOT NULL,connection_id TEXT NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL REFERENCES sessions(id),kind TEXT NOT NULL,payload TEXT NOT NULL,created_at TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS events_session ON events(session_id,seq);
          CREATE INDEX IF NOT EXISTS events_projection ON events(session_id,kind,seq);
          CREATE TABLE IF NOT EXISTS settings(id INTEGER PRIMARY KEY CHECK(id=1),payload TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS projects(id TEXT PRIMARY KEY,name TEXT NOT NULL,workspace TEXT NOT NULL,device_id TEXT NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,UNIQUE(device_id,workspace));
          CREATE TABLE IF NOT EXISTS store_metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
        Ok(Self { db: Mutex::new(db) })
    }
    pub fn projects(&self) -> Result<Vec<Project>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT id,name,workspace,device_id,created_at,updated_at FROM projects ORDER BY updated_at DESC,id")?;
        let rows = stmt.query_map([], project_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn project(&self, id: &str) -> Result<Project> {
        self.db.lock().unwrap().query_row(
            "SELECT id,name,workspace,device_id,created_at,updated_at FROM projects WHERE id=?1",
            [id],
            project_row,
        ).optional()?.context("项目不存在")
    }
    pub fn register_project(&self, workspace: &str, device_id: &str) -> Result<Project> {
        if workspace.trim().is_empty() {
            bail!("项目目录不能为空")
        }
        if device_id.trim().is_empty() {
            bail!("项目设备不能为空")
        }
        register_project_on(&self.db.lock().unwrap(), workspace, device_id)
    }
    pub fn register_project_and_save_setting(
        &self,
        workspace: &str,
        device_id: &str,
        config: &Value,
    ) -> Result<Project> {
        if workspace.trim().is_empty() || device_id.trim().is_empty() {
            bail!("项目目录和设备不能为空")
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let project = register_project_on(&tx, workspace, device_id)?;
        save_setting_on(&tx, config)?;
        tx.commit()?;
        Ok(project)
    }
    pub fn rename_project(&self, id: &str, name: &str) -> Result<Project> {
        let name = name.trim();
        if name.is_empty() {
            bail!("项目名称不能为空")
        }
        if name.chars().count() > 128 {
            bail!("项目名称不能超过 128 个字符")
        }
        let db = self.db.lock().unwrap();
        let changed = db.execute(
            "UPDATE projects SET name=?2,updated_at=?3 WHERE id=?1",
            params![id, name, chrono::Utc::now().to_rfc3339()],
        )?;
        if changed == 0 {
            bail!("项目不存在")
        }
        Ok(db.query_row(
            "SELECT id,name,workspace,device_id,created_at,updated_at FROM projects WHERE id=?1",
            [id],
            project_row,
        )?)
    }
    pub fn remove_project(&self, id: &str) -> Result<()> {
        let changed = self
            .db
            .lock()
            .unwrap()
            .execute("DELETE FROM projects WHERE id=?1", [id])?;
        if changed == 0 {
            bail!("项目不存在")
        }
        Ok(())
    }
    pub fn remove_project_and_save_setting(&self, id: &str, config: &Value) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        if tx.execute("DELETE FROM projects WHERE id=?1", [id])? == 0 {
            bail!("项目不存在")
        }
        save_setting_on(&tx, config)?;
        tx.commit()?;
        Ok(())
    }
    pub fn migrate_projects(&self, current_workspace: &str, current_device_id: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let migrated = tx
            .query_row(
                "SELECT value FROM store_metadata WHERE key='projects_v1'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .is_some();
        if migrated {
            return Ok(());
        }
        let bindings = {
            let mut stmt = tx.prepare("SELECT DISTINCT workspace,device_id FROM sessions")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if !current_workspace.trim().is_empty() {
            register_project_on(&tx, current_workspace, current_device_id)?;
        }
        for (workspace, device_id) in bindings {
            if !workspace.trim().is_empty() {
                register_project_on(&tx, &workspace, &device_id)?;
            }
        }
        tx.execute(
            "INSERT INTO store_metadata(key,value) VALUES('projects_v1',?1)",
            [chrono::Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn setting(&self) -> Result<Option<Value>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT payload FROM settings WHERE id=1")?;
        let mut rows = stmt.query([])?;
        Ok(rows
            .next()?
            .map(|r| r.get::<_, String>(0))
            .transpose()?
            .map(|s| serde_json::from_str(&s))
            .transpose()?)
    }
    pub fn save_setting(&self, value: &Value) -> Result<()> {
        save_setting_on(&self.db.lock().unwrap(), value)
    }
    pub fn create_session(&self, session: &Session) -> Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT INTO sessions VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                session.id,
                session.title,
                session.workspace,
                session.device_id,
                session.connection_id,
                session.created_at,
                session.updated_at
            ],
        )?;
        Ok(())
    }
    pub fn sessions(&self) -> Result<Vec<Session>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT id,title,workspace,device_id,connection_id,created_at,updated_at FROM sessions ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], |r| {
            Ok(Session {
                id: r.get(0)?,
                title: r.get(1)?,
                workspace: r.get(2)?,
                device_id: r.get(3)?,
                connection_id: r.get(4)?,
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn session(&self, id: &str) -> Result<Session> {
        self.db.lock().unwrap().query_row(
            "SELECT id,title,workspace,device_id,connection_id,created_at,updated_at FROM sessions WHERE id=?1",
            [id], |r| Ok(Session { id:r.get(0)?,title:r.get(1)?,workspace:r.get(2)?,device_id:r.get(3)?,connection_id:r.get(4)?,created_at:r.get(5)?,updated_at:r.get(6)? })
        ).optional()?.context("会话不存在")
    }
    pub fn original_goal(&self, id: &str) -> Result<Option<String>> {
        let payload: Option<String> = self.db.lock().unwrap().query_row(
            "SELECT payload FROM events WHERE session_id=?1 AND kind='message' AND json_extract(payload,'$.role')='user' ORDER BY seq LIMIT 1",
            [id], |r| r.get(0)
        ).optional()?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let message: ChatMessage = serde_json::from_str(&payload)?;
        Ok(Some(match message.content {
            Some(Value::String(text)) => text,
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .filter(|text| !text.starts_with("[Saved image artifact:"))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }))
    }
    /// Return original user messages, not synthetic summaries. Compaction must
    /// fail explicitly rather than silently discard an oversized constraint set.
    pub fn user_text_history(&self, id: &str) -> Result<Vec<String>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT payload FROM events WHERE session_id=?1 AND kind='message' AND json_extract(payload,'$.role')='user' ORDER BY seq LIMIT 1001")?;
        let mut rows = stmt.query([id])?;
        let mut messages = Vec::new();
        let mut total_bytes = 0usize;
        while let Some(row) = rows.next()? {
            if messages.len() >= 1000 {
                bail!("用户消息数量超过压缩保护上限；请新建会话，原始记录已保留")
            }
            let payload: String = row.get(0)?;
            total_bytes = total_bytes.saturating_add(payload.len());
            if total_bytes > 4 * 1024 * 1024 {
                bail!("用户消息超过压缩保护上限；请扩大上下文或新建会话，原始记录已保留")
            }
            let message: ChatMessage = serde_json::from_str(&payload)?;
            if let Some(text) = crate::context::user_message_text(&message) {
                messages.push(text);
            }
        }
        Ok(messages)
    }
    /// Search on disk; only bounded snippets cross the SQLite/Rust boundary.
    /// No unbounded event deserialization or SQL wildcard interpretation.
    pub fn search_events(&self, id: &str, query: &str) -> Result<Vec<Value>> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            bail!("搜索内容不能为空")
        }
        if query.chars().count() > 200 {
            bail!("搜索内容不能超过 200 个字符")
        }
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT seq,kind,substr(payload,max(1,harness_unicode_instr(payload,?2)-300),1600),length(payload) FROM events WHERE session_id=?1 AND kind IN ('message','activity') AND harness_unicode_instr(payload,?2)>0 ORDER BY seq DESC LIMIT 8")?;
        let rows = stmt.query_map(params![id,query], |r| Ok(serde_json::json!({"seq":r.get::<_,i64>(0)?,"kind":r.get::<_,String>(1)?,"text":r.get::<_,String>(2)?,"truncated":r.get::<_,usize>(3)?>1600})))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn append(&self, id: &str, kind: &str, payload: &Value) -> Result<i64> {
        let db = self.db.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        db.execute(
            "INSERT INTO events(session_id,kind,payload,created_at) VALUES(?1,?2,?3,?4)",
            params![id, kind, payload.to_string(), now],
        )?;
        let seq = db.last_insert_rowid();
        db.execute(
            "UPDATE sessions SET updated_at=?2 WHERE id=?1",
            params![id, now],
        )?;
        Ok(seq)
    }
    pub fn rename(&self, id: &str, title: &str) -> Result<()> {
        if title.trim().is_empty() {
            bail!("标题不能为空")
        }
        self.db.lock().unwrap().execute(
            "UPDATE sessions SET title=?2 WHERE id=?1",
            params![id, title],
        )?;
        Ok(())
    }
    pub fn events(&self, id: &str) -> Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT seq,kind,payload,created_at FROM events WHERE session_id=?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|r| { let (seq,kind,payload,created_at)=r?; Ok(serde_json::json!({"seq":seq,"kind":kind,"payload":serde_json::from_str::<Value>(&payload)?,"createdAt":created_at})) }).collect()
    }
    pub(crate) fn task_checkpoint(&self, id: &str) -> Result<Option<Value>> {
        let payload: Option<String> = self.db.lock().unwrap().query_row(
            "SELECT payload FROM events WHERE session_id=?1 AND kind='task_checkpoint' ORDER BY seq DESC LIMIT 1",
            [id], |row| row.get(0),
        ).optional()?;
        payload.map(|payload| serde_json::from_str(&payload).map_err(Into::into)).transpose()
    }
    pub(crate) fn tool_evidence(&self, id: &str, call_id: &str) -> Result<Option<Value>> {
        let payload: Option<String> = self.db.lock().unwrap().query_row(
            "SELECT payload FROM events WHERE session_id=?1 AND kind='activity' AND json_extract(payload,'$.type')='tool_completed' AND json_extract(payload,'$.callId')=?2 ORDER BY seq DESC LIMIT 1",
            params![id, call_id], |row| row.get(0),
        ).optional()?;
        payload.map(|payload| serde_json::from_str(&payload).map_err(Into::into)).transpose()
    }
    pub fn messages(&self, id: &str) -> Result<Vec<ChatMessage>> {
        // A compaction is a projection checkpoint. Do not deserialize old messages,
        // screenshots or terminal activity again on every new turn.
        let db = self.db.lock().unwrap();
        let checkpoint: Option<(i64,String)> = db.query_row(
            "SELECT seq,payload FROM events WHERE session_id=?1 AND kind='context_compacted' ORDER BY seq DESC LIMIT 1",
            [id], |r| Ok((r.get(0)?,r.get(1)?))
        ).optional()?;
        let mut messages: Vec<ChatMessage> = vec![];
        let mut after = 0;
        if let Some((seq, payload)) = checkpoint {
            after = seq;
            let mut value: Value = serde_json::from_str(&payload)?;
            messages = serde_json::from_value(value["messages"].take())?;
        }
        let mut stmt = db.prepare("SELECT payload FROM events WHERE session_id=?1 AND kind='message' AND seq>?2 ORDER BY seq")?;
        let rows = stmt.query_map(params![id, after], |r| r.get::<_, String>(0))?;
        for row in rows {
            messages.push(serde_json::from_str(&row?)?);
        }
        drop(stmt);
        drop(db);
        // Repair each call block in place, before a later user turn. Never replay it.
        let mut repaired = Vec::new();
        let mut i = 0;
        while i < messages.len() {
            let current = messages[i].clone();
            let calls = current.tool_calls.clone().unwrap_or_default();
            repaired.push(current);
            i += 1;
            if calls.is_empty() {
                continue;
            }
            let mut results = std::collections::HashMap::new();
            while i < messages.len() && messages[i].role == "tool" {
                if let Some(id) = messages[i].tool_call_id.clone() {
                    results.insert(id, messages[i].clone());
                }
                i += 1;
            }
            for call in calls {
                repaired.push(results.remove(&call.id).unwrap_or_else(||ChatMessage{
                    role:"tool".into(),content:Some(Value::String("Execution result missing / indeterminate. The operation may already have executed. Do not repeat side effects automatically; observe current state first.".into())),
                    tool_calls:None,tool_call_id:Some(call.id),name:Some(call.function.name),reasoning_content:None
                }));
            }
        }
        Ok(repaired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_test_session(id: &str, workspace: &str, device_id: &str) -> Session {
        Session {
            id: id.into(),
            title: "history".into(),
            workspace: workspace.into(),
            device_id: device_id.into(),
            connection_id: "model".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn project_registration_preserves_alias_identity_and_creation_time() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let project = store.register_project("/work/original/", "local").unwrap();
        assert_eq!(project.name, "original");
        uuid::Uuid::parse_str(&project.id).unwrap();
        let renamed = store
            .rename_project(&project.id, "  Friendly name  ")
            .unwrap();
        assert_eq!(renamed.name, "Friendly name");
        store
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE projects SET updated_at='2000-01-01T00:00:00Z' WHERE id=?1",
                [&project.id],
            )
            .unwrap();
        let registered = store.register_project("/work/original/", "local").unwrap();
        assert_eq!(registered.id, project.id);
        assert_eq!(registered.name, "Friendly name");
        assert_eq!(registered.created_at, project.created_at);
        assert_ne!(registered.updated_at, "2000-01-01T00:00:00Z");
        assert_eq!(store.projects().unwrap().len(), 1);
        assert_eq!(store.project(&project.id).unwrap().name, "Friendly name");
        assert!(store.rename_project(&project.id, " \t\n ").is_err());
        assert!(store
            .rename_project(&project.id, &"字".repeat(129))
            .is_err());
        assert_eq!(
            store
                .rename_project(&project.id, &"字".repeat(128))
                .unwrap()
                .name
                .chars()
                .count(),
            128
        );
        assert!(store.rename_project("missing", "new name").is_err());
        let json = serde_json::to_value(&registered).unwrap();
        assert_eq!(json["deviceId"], "local");
        assert!(json.get("createdAt").is_some());
    }

    #[test]
    fn projects_distinguish_device_and_recognize_remote_path_names() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        assert!(store.register_project(" \t", "local").is_err());
        assert!(store.register_project("/valid", " \t").is_err());
        let local = store.register_project("/same/path", "local").unwrap();
        let remote = store.register_project("/same/path", "remote").unwrap();
        assert_ne!(local.id, remote.id);
        assert_eq!(local.name, remote.name);
        assert_eq!(
            store
                .register_project(r"C:\work\Windows Project\", "remote")
                .unwrap()
                .name,
            "Windows Project"
        );
        assert_eq!(store.register_project("/", "local").unwrap().name, "/");
        assert_eq!(store.projects().unwrap().len(), 4);
    }

    #[test]
    fn removing_project_preserves_workspace_sessions_and_event_history() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let file = workspace.join("keep.txt");
        std::fs::write(&file, "keep this file").unwrap();
        let db_path = dir.path().join("state.db");
        let store = Store::open(&db_path).unwrap();
        let workspace = workspace.to_str().unwrap();
        let project = store.register_project(workspace, "local").unwrap();
        let session = project_test_session("durable-session", workspace, "local");
        store.create_session(&session).unwrap();
        store
            .append(
                &session.id,
                "message",
                &serde_json::json!({"role":"user","content":"keep history"}),
            )
            .unwrap();
        let before = store.events(&session.id).unwrap();
        store.remove_project(&project.id).unwrap();
        assert!(store.project(&project.id).is_err());
        assert!(store.remove_project(&project.id).is_err());
        assert!(store.projects().unwrap().is_empty());
        assert_eq!(store.session(&session.id).unwrap().workspace, workspace);
        assert_eq!(store.events(&session.id).unwrap(), before);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep this file");
        drop(store);
        let reopened = Store::open(&db_path).unwrap();
        assert_eq!(reopened.sessions().unwrap().len(), 1);
        assert_eq!(reopened.events(&session.id).unwrap(), before);
        assert_eq!(
            reopened.messages(&session.id).unwrap()[0].content,
            Some(Value::String("keep history".into()))
        );
    }

    #[test]
    fn project_migration_imports_bindings_once_without_resurrecting_removals() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("state.db");
        let store = Store::open(&db_path).unwrap();
        let existing = store.register_project("/missing/current", "local").unwrap();
        store.rename_project(&existing.id, "Current alias").unwrap();
        for session in [
            project_test_session("one", "/missing/current", "local"),
            project_test_session("duplicate", "/missing/current", "local"),
            project_test_session("remote", "/missing/current", "remote"),
            project_test_session("windows", r"Z:\missing\legacy", "remote"),
        ] {
            store.create_session(&session).unwrap();
        }
        store.migrate_projects("/missing/current", "local").unwrap();
        assert_eq!(store.projects().unwrap().len(), 3);
        assert_eq!(store.project(&existing.id).unwrap().name, "Current alias");
        assert_eq!(store.sessions().unwrap().len(), 4);
        store.remove_project(&existing.id).unwrap();
        drop(store);
        let reopened = Store::open(&db_path).unwrap();
        reopened
            .create_session(&project_test_session("later", "/later", "local"))
            .unwrap();
        reopened
            .migrate_projects("/missing/current", "local")
            .unwrap();
        assert_eq!(reopened.projects().unwrap().len(), 2);
        assert!(reopened.project(&existing.id).is_err());
        assert_eq!(reopened.sessions().unwrap().len(), 5);
        let explicit = reopened
            .register_project("/missing/current", "local")
            .unwrap();
        assert_ne!(explicit.id, existing.id);
    }

    #[test]
    fn project_migration_rolls_back_registration_and_marker_together() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        store
            .create_session(&project_test_session("legacy", "/legacy", "remote"))
            .unwrap();
        store.db.lock().unwrap().execute_batch(
            "CREATE TRIGGER reject_legacy BEFORE INSERT ON projects WHEN NEW.device_id='remote' BEGIN SELECT RAISE(FAIL,'test migration failure'); END;",
        ).unwrap();
        assert!(store.migrate_projects("/current", "local").is_err());
        assert!(store.projects().unwrap().is_empty());
        store
            .db
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_legacy")
            .unwrap();
        store.migrate_projects("/current", "local").unwrap();
        assert_eq!(store.projects().unwrap().len(), 2);
    }

    #[test]
    fn project_migration_ignores_unset_current_and_legacy_workspaces() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        store
            .create_session(&project_test_session("unset", " \t", "local"))
            .unwrap();
        store.migrate_projects("", "local").unwrap();
        assert!(store.projects().unwrap().is_empty());
        assert_eq!(store.sessions().unwrap().len(), 1);
    }

    #[test]
    fn event_history_is_durable_and_compaction_is_a_projection() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        let s = Session {
            id: "s".into(),
            title: "x".into(),
            workspace: "/tmp".into(),
            device_id: "local".into(),
            connection_id: "m".into(),
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        store.create_session(&s).unwrap();
        let m = serde_json::json!({"role":"user","content":"original"});
        store.append("s", "message", &m).unwrap();
        store
            .append(
                "s",
                "context_compacted",
                &serde_json::json!({"messages":[{"role":"user","content":"summary"}]}),
            )
            .unwrap();
        assert_eq!(
            store.messages("s").unwrap()[0].content,
            Some(Value::String("summary".into()))
        );
        assert_eq!(store.events("s").unwrap().len(), 2);
        // Older malformed projections and large activities need not be decoded.
        // A checkpoint replaces only the model view, never the durable evidence.
        store
            .append(
                "s",
                "activity",
                &serde_json::json!({"text":"irrelevant ".repeat(10000)}),
            )
            .unwrap();
        store
            .append(
                "s",
                "message",
                &serde_json::json!("obsolete invalid ChatMessage"),
            )
            .unwrap();
        store
            .append(
                "s",
                "context_compacted",
                &serde_json::json!({"messages":[{"role":"user","content":"new checkpoint"}]}),
            )
            .unwrap();
        store
            .append(
                "s",
                "message",
                &serde_json::json!({"role":"assistant","content":"after checkpoint"}),
            )
            .unwrap();
        let projected = store.messages("s").unwrap();
        assert_eq!(projected.len(), 2);
        assert_eq!(
            projected[0].content,
            Some(Value::String("new checkpoint".into()))
        );
        assert_eq!(
            store.original_goal("s").unwrap().as_deref(),
            Some("original")
        );
        store.append("s","activity",&serde_json::json!({"text":format!("{}TAIL_ERROR:中文证据 CAFÉ İSTANBUL", "prefix".repeat(4000))})).unwrap();
        let matches = store.search_events("s", "tail_error").unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0]["text"]
            .as_str()
            .unwrap()
            .contains("TAIL_ERROR:中文证据"));
        assert!(matches[0]["text"].as_str().unwrap().chars().count() <= 1600);
        assert!(store.search_events("s", "café").unwrap()[0]["text"]
            .as_str()
            .unwrap()
            .contains("CAFÉ"));
        assert!(
            store.search_events("s", "i\u{307}stanbul").unwrap()[0]["text"]
                .as_str()
                .unwrap()
                .contains("İSTANBUL")
        );
        assert!(store.search_events("s", "").is_err());
        drop(store);
        assert_eq!(
            Store::open(&dir.path().join("state.db"))
                .unwrap()
                .events("s")
                .unwrap()
                .len(),
            7
        );
    }
}
