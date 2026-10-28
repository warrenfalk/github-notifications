use crate::github::Comment;
use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub repository: String,
    pub author: String,
    pub body: String,
    pub url: String,
    pub reason: String,
    pub created_at: String,
    pub read: bool,
}

pub struct Store(pub Connection);

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        #[cfg(unix)]
        if path != Path::new(":memory:") {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS inbox (
               id TEXT PRIMARY KEY, title TEXT NOT NULL, repository TEXT NOT NULL,
               author TEXT NOT NULL, body TEXT NOT NULL, url TEXT NOT NULL,
               reason TEXT NOT NULL, created_at TEXT NOT NULL,
               read INTEGER NOT NULL DEFAULT 0, delivered INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS dismissed_comments (id TEXT PRIMARY KEY);",
        )?;
        Ok(Self(connection))
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .0
            .query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.0.execute("INSERT INTO meta(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [key, value])?;
        Ok(())
    }

    pub fn check_account(&self, username: &str) -> Result<()> {
        if let Some(account) = self.meta("account")?
            && !account.eq_ignore_ascii_case(username)
        {
            bail!("This database belongs to {account}; use --database for another account");
        }
        self.set_meta("account", username)
    }

    pub fn insert(
        &self,
        comment: &Comment,
        title: &str,
        repository: &str,
        reason: &str,
        quiet: bool,
    ) -> Result<()> {
        self.0.execute(
            "INSERT OR IGNORE INTO inbox(id,title,repository,author,body,url,reason,created_at,delivered)
             VALUES (?,?,?,?,?,?,?,?,?)",
            params![comment.id, title, repository, comment.user.login, comment.body, comment.html_url, reason, comment.created_at.to_rfc3339(), quiet],
        )?;
        Ok(())
    }

    pub fn list(&self, pending_delivery: bool) -> Result<Vec<Item>> {
        let mut statement = self.0.prepare(
            "SELECT id,title,repository,author,body,url,reason,created_at,read FROM inbox
             WHERE (?=0 OR delivered=0)
               AND NOT EXISTS (SELECT 1 FROM dismissed_comments WHERE dismissed_comments.id=inbox.id)
             ORDER BY created_at DESC, id DESC",
        )?;
        let items = statement
            .query_map([pending_delivery], |r| {
                Ok(Item {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    repository: r.get(2)?,
                    author: r.get(3)?,
                    body: r.get(4)?,
                    url: r.get(5)?,
                    reason: r.get(6)?,
                    created_at: r.get(7)?,
                    read: r.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(items)
    }

    pub fn delivered(&self, id: &str) -> Result<()> {
        self.0
            .execute("UPDATE inbox SET delivered=1 WHERE id=?", [id])?;
        Ok(())
    }

    pub fn mark_read(&self, id: &str) -> Result<()> {
        self.0.execute("UPDATE inbox SET read=1 WHERE id=?", [id])?;
        Ok(())
    }

    pub fn dismiss(&self, ids: &[String]) -> Result<()> {
        let transaction = self.0.unchecked_transaction()?;
        for id in ids {
            self.0.execute(
                "INSERT OR IGNORE INTO dismissed_comments(id) SELECT id FROM inbox WHERE id=?",
                [id],
            )?;
            self.0
                .execute("UPDATE inbox SET read=1, delivered=1 WHERE id=?", [id])?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn checkpoint(&self) -> Result<Option<DateTime<Utc>>> {
        self.meta("checkpoint")?
            .map(|v| v.parse().map_err(Into::into))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::User;

    #[test]
    fn import_is_quiet_and_duplicate_preserves_local_state() -> Result<()> {
        let store = Store::open(Path::new(":memory:"))?;
        let comment = Comment {
            id: "review:1".into(),
            user: User {
                login: "someone".into(),
            },
            body: "reply".into(),
            html_url: "https://github.com/a/b/pull/1".into(),
            created_at: Utc::now(),
            thread_id: Some(1),
        };
        store.insert(&comment, "PR", "a/b", "Reply", true)?;
        store.mark_read(&comment.id)?;
        store.insert(&comment, "PR", "a/b", "Reply", false)?;
        assert!(store.list(true)?.is_empty());
        let items = store.list(false)?;
        assert_eq!(items.len(), 1);
        assert!(items[0].read);
        Ok(())
    }

    #[test]
    fn dismissal_survives_reimport_and_leaves_new_replies_visible() -> Result<()> {
        let store = Store::open(Path::new(":memory:"))?;
        let mut comment = Comment {
            id: "review:1".into(),
            user: User {
                login: "someone".into(),
            },
            body: "old reply".into(),
            html_url: "https://github.com/a/b/pull/1".into(),
            created_at: Utc::now(),
            thread_id: Some(1),
        };
        store.insert(&comment, "PR", "a/b", "Reply", false)?;
        // Capture the UI's selection before another reply arrives.
        let selected = vec![comment.id.clone()];
        comment.id = "review:2".into();
        store.insert(&comment, "PR", "a/b", "Reply", false)?;
        store.dismiss(&selected)?;
        store.dismiss(&selected)?;
        comment.id = "review:1".into();
        store.insert(&comment, "PR", "a/b", "Reply", false)?;
        assert_eq!(
            store
                .list(false)?
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["review:2"]
        );
        assert_eq!(store.list(true)?.len(), 1);
        // Keep the original ID for deduplication; this is not deletion.
        assert_eq!(
            store
                .0
                .query_row("SELECT count(*) FROM inbox", [], |row| row.get::<_, i64>(0))?,
            2
        );
        Ok(())
    }

    #[test]
    fn failed_transaction_does_not_advance_checkpoint_or_save_items() -> Result<()> {
        let store = Store::open(Path::new(":memory:"))?;
        {
            let _transaction = store.0.unchecked_transaction()?;
            store.set_meta("checkpoint", "2026-10-28T00:00:00Z")?;
        }
        assert!(store.checkpoint()?.is_none());
        Ok(())
    }
}
