---
title: "Coolify Deployment Quick Start"
description: "Deploy Vibe Kanban (sqlite) to Coolify using docker-compose.coolify.yml"
---

## Steps

1. In Coolify, create a new application from this repository.
2. Set **Docker Compose File Path** to `docker-compose.coolify.yml`.
3. Deploy.

## Recommended environment variables

In Coolify → Environment:

```bash
VIBE_KANBAN_PORT=3000
RUST_LOG=info
```

If you use PostHog:

```bash
POSTHOG_API_KEY=...
POSTHOG_API_ENDPOINT=...
```

## Persistent storage

Keep these mounts persistent so sqlite data and repos survive redeploys:
- `./data/db` (sqlite + app data)
- `./data/repos` (repos/worktrees)
