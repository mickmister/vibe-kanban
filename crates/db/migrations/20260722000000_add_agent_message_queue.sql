CREATE TABLE agent_message_queue (
    id                           BLOB PRIMARY KEY,
    session_id                   BLOB NOT NULL,
    workspace_id                 BLOB NOT NULL,
    status                       TEXT NOT NULL DEFAULT 'queued'
                                 CHECK (status IN ('queued','leased','starting','running','completed','failed','cancelled')),
    source                       TEXT NOT NULL DEFAULT 'agent'
                                 CHECK (source IN ('from_user','workflow','agent','system')),
    priority                     INTEGER NOT NULL DEFAULT 50,
    data                         TEXT NOT NULL,
    started_execution_process_id BLOB,
    lease_owner                  TEXT,
    lease_expires_at             TEXT,
    attempt_count                INTEGER NOT NULL DEFAULT 0,
    last_error                   TEXT,
    queued_at                    TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    created_at                   TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at                   TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
    FOREIGN KEY (started_execution_process_id) REFERENCES execution_processes(id) ON DELETE SET NULL
);

CREATE INDEX idx_agent_message_queue_status_priority
    ON agent_message_queue(status, priority DESC, queued_at ASC);
CREATE INDEX idx_agent_message_queue_session_status
    ON agent_message_queue(session_id, status, queued_at ASC);
CREATE INDEX idx_agent_message_queue_workspace_status
    ON agent_message_queue(workspace_id, status, queued_at ASC);
CREATE INDEX idx_agent_message_queue_execution_process
    ON agent_message_queue(started_execution_process_id)
    WHERE started_execution_process_id IS NOT NULL;
