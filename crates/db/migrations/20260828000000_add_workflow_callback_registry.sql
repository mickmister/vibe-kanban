CREATE TABLE workflow_callback_registry (
    id                  BLOB PRIMARY KEY,
    callback_key        TEXT NOT NULL UNIQUE,
    workspace_id        BLOB NOT NULL,
    target_session_id   BLOB NOT NULL,
    kind                TEXT NOT NULL CHECK (kind IN ('workflow_completion')),
    status              TEXT NOT NULL CHECK (status IN ('pending','delivered','failed','superseded')),
    workflow_run_id     TEXT NOT NULL,
    workflow_name       TEXT,
    workflow_design_id  TEXT,
    workflow_version    INTEGER,
    delivered_ref       TEXT,
    error_message       TEXT,
    created_at          TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at          TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE,
    FOREIGN KEY (target_session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE INDEX idx_workflow_callback_registry_workspace_status
    ON workflow_callback_registry(workspace_id, status, updated_at DESC);
CREATE INDEX idx_workflow_callback_registry_session_status
    ON workflow_callback_registry(target_session_id, status, updated_at DESC);
CREATE INDEX idx_workflow_callback_registry_workflow_run
    ON workflow_callback_registry(workflow_run_id, target_session_id, kind);
