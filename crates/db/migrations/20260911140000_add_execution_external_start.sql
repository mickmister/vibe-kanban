CREATE TABLE execution_external_starts (
 execution_process_id BLOB PRIMARY KEY REFERENCES execution_processes(id) ON DELETE CASCADE,
 start_key TEXT NOT NULL UNIQUE,
 state TEXT NOT NULL CHECK(state IN ('authorized','claiming','spawned','failed')),
 claim_fence INTEGER NOT NULL DEFAULT 0,
 claim_expires_at TEXT,
 external_process_id TEXT,
 created_at TEXT NOT NULL,
 updated_at TEXT NOT NULL
);
CREATE INDEX idx_execution_external_start_state ON execution_external_starts(state,claim_expires_at);
