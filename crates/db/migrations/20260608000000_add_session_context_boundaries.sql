ALTER TABLE sessions ADD COLUMN context_reset_at TEXT;
ALTER TABLE sessions ADD COLUMN forked_from_session_id BLOB;
ALTER TABLE sessions ADD COLUMN resume_agent_session_id TEXT;
ALTER TABLE sessions ADD COLUMN resume_agent_message_id TEXT;

CREATE INDEX IF NOT EXISTS idx_sessions_forked_from_session_id
ON sessions (forked_from_session_id);
