use rusqlite::{Connection, Row, params};
use serde_json::{Value, json};
use std::{
    fs, io,
    os::{fd::AsRawFd, unix::fs::DirBuilderExt},
    path::{Path, PathBuf},
    time::Duration,
};

pub struct Memory {
    connection: Connection,
    _instance: fs::File,
}

#[derive(Clone, Copy)]
pub enum BackgroundKind {
    Analysis,
    FileApproval,
}

pub struct PendingBackground {
    pub id: i64,
    pub input: Value,
    pub awaiting_approval: bool,
}

pub struct Exchange {
    pub id: i64,
    pub project: String,
    pub prompt: String,
    pub background: bool,
    pub reply: Option<String>,
    pub error: Option<String>,
}

pub struct Decision {
    pub exchange_id: i64,
    pub action: String,
    pub approved: bool,
}

impl Memory {
    pub fn open(directory: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
        let database = directory.join("memory.sqlite3");
        // Create with private file permissions even if the directory already existed.
        use std::os::unix::fs::OpenOptionsExt as _;
        let instance = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(directory.join("instance.lock"))?;
        // Recovery assumes there is only one owner of a data directory.
        if unsafe { libc::flock(instance.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let failure = io::Error::last_os_error();
            if failure.kind() == io::ErrorKind::WouldBlock {
                return Err("이 데이터 폴더를 사용하는 프라이데이가 이미 실행 중이야.".into());
            }
            return Err(failure.into());
        }
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&database)?;
        let mut connection = Connection::open(database)?;
        connection.busy_timeout(Duration::from_secs(3))?;
        let schema =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        schema.execute_batch(
            "CREATE TABLE IF NOT EXISTS exchanges (
                id INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                prompt TEXT NOT NULL,
                reply TEXT,
                error TEXT,
                background INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                CHECK (reply IS NULL OR error IS NULL)
            );
            CREATE INDEX IF NOT EXISTS exchanges_project_id ON exchanges(project, id DESC);
            CREATE TABLE IF NOT EXISTS background_jobs (
                exchange_id INTEGER PRIMARY KEY REFERENCES exchanges(id),
                input TEXT,
                state TEXT NOT NULL CHECK (state IN ('safe', 'approval', 'protected', 'cancelled')),
                recovered INTEGER NOT NULL DEFAULT 0 CHECK (recovered IN (0, 1)),
                CHECK (state NOT IN ('safe', 'approval') OR input IS NOT NULL)
            );
            CREATE TABLE IF NOT EXISTS current_project (
                id INTEGER PRIMARY KEY CHECK (id = 1), path TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS registered_projects (
                path TEXT NOT NULL PRIMARY KEY
            );
            INSERT OR IGNORE INTO registered_projects(path)
                SELECT path FROM current_project WHERE path <> '';
            CREATE TABLE IF NOT EXISTS playback_preferences (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                volume INTEGER NOT NULL CHECK (volume BETWEEN 0 AND 200)
            );
            CREATE TABLE IF NOT EXISTS speech_preferences (
                id INTEGER PRIMARY KEY CHECK (id = 1), language TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS decisions (
                id INTEGER PRIMARY KEY, exchange_id INTEGER NOT NULL,
                action TEXT NOT NULL, approved INTEGER NOT NULL CHECK (approved IN (0, 1)),
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );",
        )?;
        let has_background: bool = schema.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('exchanges') WHERE name = 'background')",
            [],
            |row| row.get(0),
        )?;
        if !has_background {
            schema.execute(
                "ALTER TABLE exchanges ADD COLUMN background INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        schema.commit()?;
        Ok(Self {
            connection,
            _instance: instance,
        })
    }

    pub fn speech_language(&self) -> rusqlite::Result<crate::speech_language::SpeechLanguage> {
        self.connection
            .query_row(
                "SELECT COALESCE((SELECT language FROM speech_preferences WHERE id = 1), 'ko')",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(|code| crate::speech_language::SpeechLanguage::from_code(&code))
    }

    pub fn set_speech_language(
        &self,
        language: crate::speech_language::SpeechLanguage,
    ) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO speech_preferences (id, language) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET language = excluded.language",
            [language.code],
        )?;
        Ok(())
    }

    pub fn playback_volume(&self) -> rusqlite::Result<u16> {
        self.connection.query_row(
            "SELECT COALESCE((SELECT volume FROM playback_preferences WHERE id = 1), 100)",
            [],
            |row| row.get(0),
        )
    }

    pub fn set_playback_volume(&self, volume: u16) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO playback_preferences (id, volume) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET volume = excluded.volume",
            [volume],
        )?;
        Ok(())
    }

    pub fn history(&self) -> rusqlite::Result<Vec<Exchange>> {
        let mut query = self.connection.prepare(
            "SELECT id, project, prompt, reply, error, background FROM exchanges
             WHERE id IN (SELECT id FROM exchanges ORDER BY id DESC LIMIT 100)
                OR id IN (SELECT exchange_id FROM background_jobs)
             ORDER BY id",
        )?;
        query.query_map([], exchange)?.collect()
    }

    pub fn context(&self, project: &str, prompt: &str) -> rusqlite::Result<Value> {
        let mut query = self.connection.prepare(
            "SELECT substr(prompt, 1, 1200), substr(reply, 1, 1200) FROM exchanges
            WHERE project = ?1 AND reply IS NOT NULL ORDER BY id DESC LIMIT 12",
        )?;
        let mut previous = query
            .query_map([project], |row| {
                Ok(json!({"user": row.get::<_, String>(0)?, "jarvis": row.get::<_, String>(1)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        previous.reverse();
        Ok(json!({"previous_exchanges": previous, "current_request": prompt}))
    }

    pub fn decisions(&self) -> rusqlite::Result<Vec<Decision>> {
        let mut query = self.connection.prepare(
            "SELECT exchange_id, action, approved FROM (
                SELECT id, exchange_id, action, approved FROM decisions ORDER BY id DESC LIMIT 200
             ) ORDER BY id",
        )?;
        query
            .query_map([], |row| {
                Ok(Decision {
                    exchange_id: row.get(0)?,
                    action: row.get(1)?,
                    approved: row.get(2)?,
                })
            })?
            .collect()
    }

    pub fn record_decision(&self, decision: &Decision) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO decisions(exchange_id, action, approved) VALUES (?1, ?2, ?3)",
            params![decision.exchange_id, decision.action, decision.approved],
        )?;
        Ok(())
    }

    pub fn current_project(&self) -> rusqlite::Result<String> {
        use rusqlite::OptionalExtension as _;
        self.connection
            .query_row("SELECT path FROM current_project WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()
            .map(Option::unwrap_or_default)
    }

    pub fn set_current_project(&mut self, path: &str) -> rusqlite::Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO current_project(id, path) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET path = excluded.path",
            [path],
        )?;
        if !path.is_empty() {
            transaction.execute(
                "INSERT OR IGNORE INTO registered_projects(path) VALUES (?1)",
                [path],
            )?;
        }
        transaction.commit()
    }

    pub fn projects(&self) -> rusqlite::Result<Vec<String>> {
        let mut query = self.connection.prepare(
            "SELECT path FROM registered_projects
             UNION SELECT project FROM exchanges WHERE project <> ''",
        )?;
        query.query_map([], |row| row.get(0))?.collect()
    }

    pub fn begin(
        &mut self,
        project: &str,
        prompt: &str,
        background: Option<(BackgroundKind, &Value)>,
    ) -> rusqlite::Result<i64> {
        let transaction = self.connection.transaction()?;
        let id = transaction.query_row(
            "INSERT INTO exchanges (project, prompt, background) VALUES (?1, ?2, ?3) RETURNING id",
            params![project, prompt, background.is_some()],
            |row| row.get(0),
        )?;
        if let Some((kind, input)) = background {
            let state = match kind {
                BackgroundKind::Analysis => "safe",
                BackgroundKind::FileApproval => "approval",
            };
            transaction.execute(
                "INSERT INTO background_jobs(exchange_id, input, state) VALUES (?1, ?2, ?3)",
                params![id, input.to_string(), state],
            )?;
        }
        transaction.commit()?;
        Ok(id)
    }

    pub fn pending_background(&mut self) -> rusqlite::Result<Vec<PendingBackground>> {
        let transaction = self.connection.transaction()?;
        transaction.execute_batch(
            "UPDATE exchanges SET error = CASE
                 WHEN (SELECT state FROM background_jobs WHERE exchange_id = id) = 'cancelled'
                      THEN '사용자가 작업을 중단했어.'
                 WHEN (SELECT state FROM background_jobs WHERE exchange_id = id) = 'protected'
                      THEN '앱 종료 전 Mac 동작 또는 파일 실행 단계에 도달했어. 기존 결과를 확인해줘.'
                 ELSE '자동 복구를 한 번 시도한 뒤 앱이 다시 종료됐어.'
             END
             WHERE reply IS NULL AND error IS NULL AND id IN (
                 SELECT exchange_id FROM background_jobs
                 WHERE state IN ('protected', 'cancelled') OR (state = 'safe' AND recovered = 1)
             );
             DELETE FROM background_jobs WHERE exchange_id IN (
                 SELECT id FROM exchanges WHERE reply IS NOT NULL OR error IS NOT NULL
             );",
        )?;
        let pending = {
            let mut query = transaction.prepare(
                "SELECT exchange_id, input, state = 'approval' FROM background_jobs
                 WHERE state IN ('safe', 'approval') ORDER BY exchange_id",
            )?;
            query
                .query_map([], |row| {
                    let source: String = row.get(1)?;
                    let input = serde_json::from_str(&source).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok(PendingBackground {
                        id: row.get(0)?,
                        input,
                        awaiting_approval: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        transaction.commit()?;
        Ok(pending)
    }

    pub fn claim_recovery(&self, id: i64) -> rusqlite::Result<bool> {
        self.connection.execute(
            "UPDATE background_jobs SET recovered = 1 WHERE exchange_id = ?1 AND state = 'safe' AND recovered = 0",
            [id],
        ).map(|count| count == 1)
    }

    pub fn approve_file_work(&self, id: i64) -> rusqlite::Result<bool> {
        self.connection.execute(
            "UPDATE background_jobs SET state = 'protected', input = NULL WHERE exchange_id = ?1 AND state = 'approval'",
            [id],
        ).map(|count| count == 1)
    }

    pub fn cancel_background(&self, id: i64) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE background_jobs SET state = 'cancelled', input = NULL WHERE exchange_id = ?1",
            [id],
        )?;
        Ok(())
    }

    pub fn cancel_unfinished_background(&self) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE background_jobs SET state = 'cancelled', input = NULL",
            [],
        )?;
        Ok(())
    }

    pub fn finish(&mut self, id: i64, outcome: &Result<String, String>) -> rusqlite::Result<()> {
        let (reply, error) = match outcome {
            Ok(reply) => (Some(reply), None),
            Err(error) => (None, Some(error)),
        };
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE exchanges SET reply = ?1, error = ?2 WHERE id = ?3",
            params![reply, error, id],
        )?;
        transaction.execute("DELETE FROM background_jobs WHERE exchange_id = ?1", [id])?;
        transaction.commit()
    }
}

pub fn protect_background_action(database: &Path, id: i64) -> rusqlite::Result<bool> {
    let connection =
        Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(Duration::from_secs(3))?;
    connection
        .execute(
            "UPDATE background_jobs SET state = 'protected', input = NULL
         WHERE exchange_id = ?1 AND state IN ('safe', 'protected')",
            [id],
        )
        .map(|count| count == 1)
}

fn exchange(row: &Row<'_>) -> rusqlite::Result<Exchange> {
    Ok(Exchange {
        id: row.get(0)?,
        project: row.get(1)?,
        prompt: row.get(2)?,
        reply: row.get(3)?,
        error: row.get(4)?,
        background: row.get(5)?,
    })
}

pub fn project(explicit_dir: Option<&Path>) -> io::Result<Option<PathBuf>> {
    let directory = match explicit_dir {
        Some(directory) => directory.canonicalize()?,
        None => std::env::current_dir()?.canonicalize()?,
    };
    if !directory.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "프로젝트 경로가 폴더가 아니야.",
        ));
    }
    Ok(project_root(&directory)
        .or(explicit_dir.map(|_| directory.as_path()))
        .map(Path::to_path_buf))
}

pub fn document_project(document: &Path, registered: &[String]) -> Option<PathBuf> {
    let path = document.canonicalize().ok()?;
    if !path.is_file() {
        return None;
    }
    registered
        .iter()
        .map(Path::new)
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .or_else(|| project_root(path.parent()?))
        .map(Path::to_path_buf)
}

fn project_root(directory: &Path) -> Option<&Path> {
    directory
        .ancestors()
        .find(|path| path.join(".git").exists())
        .or_else(|| {
            directory.ancestors().find(|path| {
                [
                    "Cargo.toml",
                    "package.json",
                    "pyproject.toml",
                    "go.mod",
                    "Package.swift",
                ]
                .iter()
                .any(|marker| path.join(marker).is_file())
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_language_defaults_to_korean_and_survives_reopening() {
        use crate::speech_language::SpeechLanguage;

        let path =
            std::env::temp_dir().join(format!("friday-speech-language-{}", std::process::id()));
        let memory = Memory::open(&path).unwrap();
        assert_eq!(memory.speech_language().unwrap(), SpeechLanguage::KOREAN);
        let spanish = SpeechLanguage::from_code("es");
        memory.set_speech_language(spanish).unwrap();
        drop(memory);

        let memory = Memory::open(&path).unwrap();
        assert_eq!(memory.speech_language().unwrap(), spanish);
        drop(memory);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn open_document_uses_a_registered_or_detected_project_only() {
        let scratch =
            std::env::temp_dir().join(format!("jarvis-document-project-{}", std::process::id()));
        let repo = scratch.join("repo");
        let nested = repo.join("src");
        fs::create_dir_all(&nested).unwrap();
        fs::write(repo.join("Cargo.toml"), "").unwrap();
        let document = nested.join("notes.md");
        fs::write(&document, "").unwrap();
        let unrelated = scratch.join("notes.md");
        fs::write(&unrelated, "").unwrap();
        let repo = repo.canonicalize().unwrap();
        let nested = nested.canonicalize().unwrap();
        assert_eq!(document_project(&document, &[]), Some(repo.clone()));
        assert_eq!(
            document_project(
                &document,
                &[
                    repo.to_string_lossy().into_owned(),
                    nested.to_string_lossy().into_owned()
                ]
            ),
            Some(nested)
        );
        assert_eq!(
            document_project(&unrelated, &[repo.to_string_lossy().into_owned()]),
            None
        );
        fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn only_untouched_work_can_recover_once_and_file_approval_survives_without_running() {
        let path = std::env::temp_dir().join(format!(
            "jarvis-recovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let request = json!({"current_request":"검증"});
        let mut memory = Memory::open(&path).unwrap();
        assert!(Memory::open(&path).is_err());

        let safe = memory
            .begin(
                "project",
                "safe",
                Some((BackgroundKind::Analysis, &request)),
            )
            .unwrap();
        let interacted = memory
            .begin("project", "mac", Some((BackgroundKind::Analysis, &request)))
            .unwrap();
        let waiting = memory
            .begin(
                "project",
                "file",
                Some((BackgroundKind::FileApproval, &request)),
            )
            .unwrap();
        let approved = memory
            .begin(
                "project",
                "approved",
                Some((BackgroundKind::FileApproval, &request)),
            )
            .unwrap();
        let cancelled = memory
            .begin(
                "project",
                "cancelled",
                Some((BackgroundKind::Analysis, &request)),
            )
            .unwrap();
        assert!(protect_background_action(&path.join("memory.sqlite3"), interacted).unwrap());
        assert!(memory.approve_file_work(approved).unwrap());
        assert!(!memory.approve_file_work(approved).unwrap());
        memory.cancel_background(cancelled).unwrap();
        assert!(!protect_background_action(&path.join("memory.sqlite3"), cancelled).unwrap());
        drop(memory);

        let mut memory = Memory::open(&path).unwrap();
        let pending = memory.pending_background().unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|job| (job.id, job.awaiting_approval))
                .collect::<Vec<_>>(),
            [(safe, false), (waiting, true)]
        );
        assert_eq!(pending[0].input, request);
        assert!(memory.claim_recovery(safe).unwrap());
        assert!(!memory.claim_recovery(safe).unwrap());
        drop(memory);

        let mut memory = Memory::open(&path).unwrap();
        assert_eq!(memory.pending_background().unwrap()[0].id, waiting);
        let history = memory.history().unwrap();
        assert!(
            history
                .iter()
                .filter(|exchange| exchange.id != waiting)
                .all(|exchange| exchange.error.is_some())
        );
        memory.finish(waiting, &Err("거절".into())).unwrap();
        assert!(memory.pending_background().unwrap().is_empty());
        drop(memory);
        fs::remove_dir_all(path).unwrap();
    }
}
