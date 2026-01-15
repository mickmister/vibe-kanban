---
title: "Coolify Deployment Guide"
description: "Deploy Vibe Kanban (sqlite) to Coolify with persistent data"
---

## Overview

This setup deploys a single `vibe-kanban` service (no reverse proxy, no code-server).

## Compose file

Coolify should use `docker-compose.coolify.yml`.

Key configuration:
- Container listens on `PORT=3000` and binds to `HOST=0.0.0.0`
- Host port defaults to `3000` via `VIBE_KANBAN_PORT`
- Data persists under `./data/` (sqlite + repos)

## Notes

- The image is built from `Dockerfile.coolify` (multi-stage build: frontend + Rust server).
- If you want a different public port, set `VIBE_KANBAN_PORT` in Coolify.
