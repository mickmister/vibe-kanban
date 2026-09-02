# Deterministic scripted QA executor Docker smoke

Workflow E2E tests should run in Docker, not directly on the local host. The
first harness slice is intentionally narrow: it builds the VK Docker builder
stage with `qa-mode`, starts a container, and runs deterministic scripted QA
executor checks via `docker exec`.

Preview the commands without building:

```bash
scripts/qa-scripted-docker-smoke.sh --plan
```

Run the smoke harness:

```bash
scripts/qa-scripted-docker-smoke.sh
```

The scripted executor is enabled only under `qa-mode`. Set either of these for
manual QA-mode server runs:

- `VK_QA_SCRIPTED_OUTCOME` with inline JSON.
- `VK_QA_SCRIPTED_OUTCOME_FILE` with a JSON file path.

Example outcome:

```json
{
  "outcome": "completed",
  "final_message": "Deterministic final message for workflow tests.",
  "session_id": "qa-scripted-session",
  "message_id": "qa-scripted-message",
  "delay_ms": 0
}
```

Supported v1 outcomes:

- `completed`
- `failed`
- `wait_callback` marker
- `wait_ci` marker
- `stall` marker with no final assistant end-turn message
- `structured_command`
- `long_response`
- `killed` marker; true killed status is still owned by container stop paths

Future workflow E2E tests should extend this Docker harness and keep using
`docker exec` for test commands so host state does not become part of the test
contract.
