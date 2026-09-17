DROP INDEX IF EXISTS idx_agent_turn_admission_active_workspace;
DROP INDEX IF EXISTS idx_agent_turn_admission_status_expiry;
ALTER TABLE agent_turn_admissions RENAME TO agent_turn_admissions_intermediate;

CREATE TABLE agent_turn_admissions (
    token_id BLOB PRIMARY KEY,
    operation_key TEXT NOT NULL UNIQUE,
    queue_item_id BLOB,
    intended_process_id BLOB,
    workspace_id BLOB NOT NULL,
    fence INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL CHECK (status IN ('reserved','starting','started','released','expired')),
    expires_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (queue_item_id) REFERENCES agent_message_queue(id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

INSERT INTO agent_turn_admissions
    (token_id, operation_key, queue_item_id, intended_process_id, workspace_id,
     fence, status, expires_at, created_at, updated_at)
SELECT token_id, operation_key, queue_item_id, intended_process_id, workspace_id,
       fence, status, expires_at, created_at, updated_at
FROM agent_turn_admissions_intermediate;
DROP TABLE agent_turn_admissions_intermediate;

CREATE UNIQUE INDEX idx_agent_turn_admission_active_workspace
    ON agent_turn_admissions(workspace_id)
    WHERE status IN ('reserved','starting');
CREATE INDEX idx_agent_turn_admission_status_expiry
    ON agent_turn_admissions(status, expires_at);
CREATE UNIQUE INDEX idx_agent_turn_admission_intended_process
    ON agent_turn_admissions(intended_process_id)
    WHERE intended_process_id IS NOT NULL;
