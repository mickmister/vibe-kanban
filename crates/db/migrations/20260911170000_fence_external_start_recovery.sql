ALTER TABLE execution_external_starts ADD COLUMN recovery_token TEXT;
ALTER TABLE execution_external_starts ADD COLUMN recovery_generation INTEGER NOT NULL DEFAULT 0;

PRAGMA foreign_keys = OFF;

CREATE TABLE execution_external_start_recoveries_next (
 id BLOB PRIMARY KEY,
 execution_process_id BLOB NOT NULL REFERENCES execution_processes(id) ON DELETE CASCADE,
 workspace_id BLOB NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
 operation TEXT NOT NULL CHECK(operation IN ('confirm_stopped','force_reconcile')),
 actor TEXT NOT NULL,
 created_at TEXT NOT NULL,
 recovery_token TEXT,
 recovery_generation INTEGER NOT NULL DEFAULT 0,
 UNIQUE(execution_process_id, recovery_generation, operation)
);

INSERT INTO execution_external_start_recoveries_next
 (id,execution_process_id,workspace_id,operation,actor,created_at)
SELECT id,execution_process_id,workspace_id,operation,actor,created_at
FROM execution_external_start_recoveries;

DROP TABLE execution_external_start_recoveries;
ALTER TABLE execution_external_start_recoveries_next RENAME TO execution_external_start_recoveries;

CREATE UNIQUE INDEX idx_external_start_recovery_token
 ON execution_external_starts(recovery_token)
 WHERE recovery_token IS NOT NULL;

PRAGMA foreign_keys = ON;
