use crate::{now_ms, validate_operation_id};
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;

pub(crate) struct Journal {
    connection: Connection,
}

impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let schema_version: u32 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if schema_version > 2 {
            bail!("remote journal was written by a newer version; refusing to downgrade it");
        }
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS pairings (
               id TEXT PRIMARY KEY, token_hash TEXT UNIQUE NOT NULL,
               created_at INTEGER NOT NULL, revoked_at INTEGER);
             CREATE TABLE IF NOT EXISTS operations (
               id TEXT PRIMARY KEY, request_hash TEXT NOT NULL, owner TEXT NOT NULL,
               workspace TEXT NOT NULL, name TEXT NOT NULL, state TEXT NOT NULL,
               result TEXT, error TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS operation_events (
               seq INTEGER PRIMARY KEY AUTOINCREMENT, operation_id TEXT NOT NULL,
               owner TEXT NOT NULL, state TEXT NOT NULL, created_at INTEGER NOT NULL,payload TEXT);
             CREATE INDEX IF NOT EXISTS operation_events_owner_seq ON operation_events(owner, seq);
             CREATE INDEX IF NOT EXISTS operation_events_owner_operation_seq ON operation_events(owner,operation_id,seq);",
        )?;
        let has_payload = {
            let mut statement = connection.prepare("PRAGMA table_info(operation_events)")?;
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            columns.iter().any(|column| column == "payload")
        };
        if !has_payload {
            connection.execute_batch("ALTER TABLE operation_events ADD COLUMN payload TEXT;")?;
        }
        connection.pragma_update(None, "user_version", 2u32)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self { connection })
    }

    /// Never replay a request whose side effects may have begun before a crash.
    pub fn recover(&mut self) -> Result<()> {
        let unresolved: Vec<(String, String)> = {
            let mut statement = self.connection.prepare(
                "SELECT id, owner FROM operations WHERE state IN ('accepted','dispatching')",
            )?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let transaction = self.connection.transaction()?;
        for (id, owner) in unresolved {
            transaction.execute("UPDATE operations SET state='indeterminate',error=?2,updated_at=?3 WHERE id=?1", params![id, "Executor restarted before this operation settled. Side effects may have occurred; inspect state before issuing a new operation.", now_ms()])?;
            transaction.execute("INSERT INTO operation_events(operation_id,owner,state,created_at) VALUES(?1,?2,'indeterminate',?3)", params![id,owner,now_ms()])?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn add_pairing(&self, id: &str, token_hash: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO pairings(id,token_hash,created_at) VALUES(?1,?2,?3)",
            params![id, token_hash, now_ms()],
        )?;
        Ok(())
    }

    pub fn authenticate(&self, token_hash: &str) -> Result<Option<String>> {
        Ok(self
            .connection
            .query_row(
                "SELECT id FROM pairings WHERE token_hash=?1 AND revoked_at IS NULL",
                [token_hash],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn pairing_active(&self, id: &str) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pairings WHERE id=?1 AND revoked_at IS NULL)",
            [id],
            |row| row.get(0),
        )?)
    }

    pub fn revoke(&self, id: &str) -> Result<()> {
        if self.connection.execute(
            "UPDATE pairings SET revoked_at=?2 WHERE id=?1 AND revoked_at IS NULL",
            params![id, now_ms()],
        )? == 0
        {
            bail!("pairing is missing or already revoked");
        }
        Ok(())
    }

    pub fn pairings(&self) -> Result<Value> {
        let mut statement = self
            .connection
            .prepare("SELECT id,created_at,revoked_at FROM pairings ORDER BY created_at DESC")?;
        let rows = statement.query_map([], |row| Ok(json!({"id":row.get::<_,String>(0)?,"createdAt":row.get::<_,u64>(1)?,"revokedAt":row.get::<_,Option<u64>>(2)?})))?;
        Ok(Value::Array(
            rows.collect::<std::result::Result<Vec<_>, _>>()?,
        ))
    }

    /// Retrieve a matching settled/in-flight request without admitting work.
    pub fn existing(&self, id: &str, request_hash: &str, owner: &str) -> Result<Option<Value>> {
        let existing: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT request_hash,owner FROM operations WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((stored_hash, stored_owner)) = existing {
            if stored_owner != owner {
                bail!("operation belongs to a different pairing");
            }
            if stored_hash != request_hash {
                bail!("operation id already exists with different arguments or workspace");
            }
            return Ok(Some(self.operation(id, Some(owner))?));
        }
        Ok(None)
    }

    /// Returns true only for a newly admitted request. A duplicate never dispatches.
    pub fn accept(
        &mut self,
        id: &str,
        request_hash: &str,
        owner: &str,
        workspace: &str,
        name: &str,
    ) -> Result<bool> {
        validate_operation_id(id)?;
        let transaction = self.connection.transaction()?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT request_hash,owner FROM operations WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((stored_hash, stored_owner)) = existing {
            if stored_owner != owner {
                bail!("operation belongs to a different pairing");
            }
            if stored_hash != request_hash {
                bail!("operation id already exists with different arguments or workspace");
            }
            return Ok(false);
        }
        transaction.execute("INSERT INTO operations(id,request_hash,owner,workspace,name,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,'accepted',?6,?6)", params![id,request_hash,owner,workspace,name,now_ms()])?;
        transaction.execute("INSERT INTO operation_events(operation_id,owner,state,created_at) VALUES(?1,?2,'accepted',?3)", params![id,owner,now_ms()])?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn dispatch(&mut self, id: &str) -> Result<()> {
        let transaction = self.connection.transaction()?;
        let owner: String = transaction
            .query_row(
                "SELECT owner FROM operations WHERE id=?1 AND state='accepted'",
                [id],
                |row| row.get(0),
            )
            .context("operation is no longer accepted")?;
        transaction.execute(
            "UPDATE operations SET state='dispatching',updated_at=?2 WHERE id=?1",
            params![id, now_ms()],
        )?;
        transaction.execute("INSERT INTO operation_events(operation_id,owner,state,created_at) VALUES(?1,?2,'dispatching',?3)", params![id,owner,now_ms()])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn finish(
        &mut self,
        id: &str,
        state: &str,
        result: Option<&Value>,
        error: Option<&str>,
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;
        let owner: String =
            transaction.query_row("SELECT owner FROM operations WHERE id=?1", [id], |row| {
                row.get(0)
            })?;
        transaction.execute("UPDATE operations SET state=?2,result=?3,error=?4,updated_at=?5 WHERE id=?1 AND state IN ('accepted','dispatching')", params![id,state,result.map(serde_json::to_string).transpose()?,error,now_ms()])?;
        transaction.execute(
            "INSERT INTO operation_events(operation_id,owner,state,created_at) VALUES(?1,?2,?3,?4)",
            params![id, owner, state, now_ms()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn record_cancel(&self, id: &str, owner: &str) -> Result<()> {
        self.operation(id, Some(owner))?;
        self.connection.execute("INSERT INTO operation_events(operation_id,owner,state,created_at) VALUES(?1,?2,'cancellationRequested',?3)", params![id,owner,now_ms()])?;
        Ok(())
    }

    pub fn record_output(&self, id: &str, payload: &Value) -> Result<()> {
        // Stream ownership comes from the admitted operation, never from the
        // child process or an untrusted payload field.
        let owner: String = self.connection.query_row(
            "SELECT owner FROM operations WHERE id=?1 AND state='dispatching'",
            [id],
            |row| row.get(0),
        )?;
        self.connection.execute("INSERT INTO operation_events(operation_id,owner,state,created_at,payload) VALUES(?1,?2,'tool_output',?3,?4)",params![id,owner,now_ms(),payload.to_string()])?;
        Ok(())
    }

    pub fn operation(&self, id: &str, owner: Option<&str>) -> Result<Value> {
        self.connection.query_row("SELECT owner,workspace,name,state,result,error,created_at,updated_at FROM operations WHERE id=?1", [id], |row| {
            Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?,row.get::<_,u64>(6)?,row.get::<_,u64>(7)?))
        }).optional()?.map(|(stored_owner,workspace,name,state,result,error,created_at,updated_at)| {
            if owner.is_some_and(|value| value != stored_owner) { bail!("operation belongs to a different pairing"); }
            Ok(json!({"id":id,"workspace":workspace,"name":name,"state":state,"result":result.map(|r|serde_json::from_str::<Value>(&r)).transpose()?,"error":error,"createdAt":created_at,"updatedAt":updated_at}))
        }).transpose()?.context("unknown operation id")
    }

    #[cfg(test)]
    pub fn events(&self, owner: &str, after: u64) -> Result<Value> {
        self.events_for_operation(owner, after, None)
    }

    pub fn events_for_operation(
        &self,
        owner: &str,
        after: u64,
        operation_id: Option<&str>,
    ) -> Result<Value> {
        if let Some(id) = operation_id {
            let stored_owner: Option<String> = self
                .connection
                .query_row("SELECT owner FROM operations WHERE id=?1", [id], |row| {
                    row.get(0)
                })
                .optional()?;
            if stored_owner.as_deref() != Some(owner) {
                bail!("operation is missing or belongs to a different pairing");
            }
        }
        let filter = if operation_id.is_some() {
            "operation_id=?3"
        } else {
            "?3 IS NULL"
        };
        let mut statement=self.connection.prepare(&format!("SELECT seq,operation_id,state,created_at,payload FROM operation_events WHERE owner=?1 AND seq>?2 AND {filter} ORDER BY seq LIMIT 256"))?;
        let rows = statement.query_map(params![owner, after, operation_id], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, u64>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        let events = rows
            .map(|row| {
                let (cursor, id, state, created_at, payload) = row?;
                let mut value =
                    json!({"cursor":cursor,"operationId":id,"state":state,"createdAt":created_at});
                if let Some(payload) = payload {
                    value["payload"] = serde_json::from_str(&payload)?;
                }
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()?;
        let cursor = events
            .last()
            .and_then(|e| e["cursor"].as_u64())
            .unwrap_or(after);
        Ok(json!({"events":events,"cursor":cursor}))
    }

    pub fn recent(&self) -> Result<Value> {
        // Companion status polls must not deserialize and retransmit every old
        // screenshot or terminal artifact. Full results are fetched by op ID.
        let mut statement=self.connection.prepare("SELECT id,workspace,name,state,result IS NOT NULL,error,created_at,updated_at FROM operations ORDER BY updated_at DESC LIMIT 30")?;
        let rows=statement.query_map([],|row|Ok(json!({"id":row.get::<_,String>(0)?,"workspace":row.get::<_,String>(1)?,"name":row.get::<_,String>(2)?,"state":row.get::<_,String>(3)?,"hasResult":row.get::<_,bool>(4)?,"error":row.get::<_,Option<String>>(5)?,"createdAt":row.get::<_,u64>(6)?,"updatedAt":row.get::<_,u64>(7)?})))?;
        Ok(Value::Array(
            rows.collect::<std::result::Result<Vec<_>, _>>()?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_journal_migrates_and_operation_output_keeps_owner_and_cursor() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("remote.sqlite");
        let legacy = Connection::open(&path)?;
        legacy.execute_batch("CREATE TABLE operation_events(seq INTEGER PRIMARY KEY AUTOINCREMENT,operation_id TEXT NOT NULL,owner TEXT NOT NULL,state TEXT NOT NULL,created_at INTEGER NOT NULL); INSERT INTO operation_events(operation_id,owner,state,created_at) VALUES('legacy','owner','completed',1); PRAGMA user_version=1;")?;
        drop(legacy);
        let mut journal = Journal::open(&path)?;
        assert_eq!(
            journal
                .connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))?,
            2
        );
        journal.accept("stream", "hash", "owner", "/workspace", "terminal_run")?;
        journal.dispatch("stream")?;
        journal.record_output(
            "stream",
            &json!({"type":"tool_output","stream":"stdout","text":"live"}),
        )?;
        let events = journal.events_for_operation("owner", 0, Some("stream"))?;
        assert_eq!(events["events"].as_array().unwrap().len(), 3);
        assert_eq!(events["events"][2]["payload"]["text"], "live");
        let cursor = events["cursor"].as_u64().unwrap();
        assert!(journal
            .events_for_operation("stranger", 0, Some("stream"))
            .is_err());
        assert!(
            journal.events_for_operation("owner", cursor, Some("stream"))?["events"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        drop(journal);
        let reopened = Journal::open(&path)?;
        assert_eq!(
            reopened.events_for_operation("owner", 0, Some("stream"))?["events"][2]["payload"]
                ["text"],
            "live"
        );
        assert!(reopened.events("owner", 0)?["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["operationId"] == "legacy"));
        Ok(())
    }

    #[test]
    fn journal_deduplicates_and_recovers_crash_marker() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("remote.sqlite");
        let mut journal = Journal::open(&path)?;
        assert!(journal.accept("op-1", "hash-a", "owner", "/workspace", "shell")?);
        journal.dispatch("op-1")?;
        assert!(!journal.accept("op-1", "hash-a", "owner", "/workspace", "shell")?);
        assert!(journal
            .accept("op-1", "hash-b", "owner", "/workspace", "shell")
            .is_err());
        drop(journal);
        let mut recovered = Journal::open(&path)?;
        recovered.recover()?;
        assert_eq!(
            recovered.operation("op-1", Some("owner"))?["state"],
            "indeterminate"
        );
        assert!(!recovered.accept("op-1", "hash-a", "owner", "/workspace", "shell")?);
        let events = recovered.events("owner", 0)?;
        let states: Vec<_> = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["state"].as_str().unwrap())
            .collect();
        assert_eq!(states, vec!["accepted", "dispatching", "indeterminate"]);
        Ok(())
    }
}
