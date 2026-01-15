# Docker Setup (Local + Coolify)

This repo includes a minimal Docker Compose setup for running the sqlite deployment in a container.

## Local Docker Compose

1. Create an env file:
   ```bash
   cp .env.example .env
   ```

2. Build and run:
   ```bash
   docker compose up --build
   ```

3. Open:
   - http://localhost:3000 (or `VIBE_KANBAN_PORT`)

Persistent data is stored under `./data/`:
- `./data/db` → sqlite DB + app state
- `./data/repos` → repos/worktrees created by the app

## Coolify

Use `docker-compose.coolify.yml` as the Compose file path in Coolify.
Ensure `./data/` is treated as persistent storage (Coolify persistent volumes) so sqlite state survives redeploys.
