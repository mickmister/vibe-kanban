PRAGMA foreign_keys = OFF;

CREATE TABLE execution_external_starts_next (
 execution_process_id BLOB PRIMARY KEY REFERENCES execution_processes(id) ON DELETE CASCADE,
 start_key TEXT NOT NULL UNIQUE,
 state TEXT NOT NULL CHECK(state IN ('authorized','claiming','spawned','blocked','failed')),
 claim_fence INTEGER NOT NULL DEFAULT 0,
 claim_expires_at TEXT,
 external_process_id TEXT,
 created_at TEXT NOT NULL,
 updated_at TEXT NOT NULL,
 spawn_requested_at TEXT,
 external_process_started_at TEXT,
 blocked_reason TEXT
);

INSERT INTO execution_external_starts_next
 (execution_process_id,start_key,state,claim_fence,claim_expires_at,external_process_id,
  created_at,updated_at,spawn_requested_at,external_process_started_at)
SELECT execution_process_id,start_key,state,claim_fence,claim_expires_at,external_process_id,
       created_at,updated_at,spawn_requested_at,external_process_started_at
FROM execution_external_starts;

DROP TABLE execution_external_starts;
ALTER TABLE execution_external_starts_next RENAME TO execution_external_starts;
CREATE INDEX idx_execution_external_start_state ON execution_external_starts(state,claim_expires_at);

CREATE TABLE execution_external_start_recoveries (
 id BLOB PRIMARY KEY,
 execution_process_id BLOB NOT NULL REFERENCES execution_processes(id) ON DELETE CASCADE,
 workspace_id BLOB NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
 operation TEXT NOT NULL CHECK(operation IN ('confirm_stopped','force_reconcile')),
 actor TEXT NOT NULL,
 created_at TEXT NOT NULL,
 UNIQUE(execution_process_id, operation)
);

PRAGMA foreign_keys = ON;
