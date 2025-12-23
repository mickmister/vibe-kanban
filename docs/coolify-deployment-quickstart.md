---
title: "Coolify Deployment Quick Start"
description: "Quick reference for deploying Vibe Kanban to Coolify - get up and running in 5 minutes"
---

## Quick Deploy

Deploy Vibe Kanban to Coolify in 3 steps:

<Steps>
<Step title="Set required environment variables">
  In Coolify's Environment tab:

  ```bash
  CODE_PASSWORD=your_secure_password
  ```
</Step>

<Step title="Configure Docker Compose">
  - Set **Docker Compose File Path**: `docker-compose.coolify.yml`
  - Coolify auto-detects 3 services
</Step>

<Step title="Deploy">
  Click **Deploy** and wait for health checks to pass
</Step>
</Steps>

<Check>
Access your app at the Coolify-provided URL
</Check>

## Key Changes from Development

The production `docker-compose.coolify.yml` differs from the development `docker-compose.yml`:

### Removed ❌

- Host-specific path mounts (`/Users/mickmister/code`, `/private/var/folders/...`)
- Development source code mounting (`.:/app`)
- Development environment variables
- Worktree-specific configurations
- Host bridging features

### Added ✅

- Production Dockerfile build
- Bind mount data persistence to `./data/`
- Health checks for all services
- Service dependencies
- Proper container restart policies
- Security: non-root user (uid 1001)
- Configurable ports via environment variables

### Modified 🔄

- **Ports**: Single entry point (3001) instead of multiple
- **Volumes**: Named volumes instead of host paths
- **Environment**: Production-ready defaults
- **Build**: Full production build, not incremental dev build

## Service Endpoints

When deployed to Coolify:

| Service | Internal Port | Access Via |
|---------|--------------|------------|
| Vibe Kanban | 3000 | `https://your-domain.com` |
| code-server | 8443 | `https://your-domain.com?folder=/repos` |
| Caddy | 3001 | Coolify maps to external port/domain |

## Essential Environment Variables

<Tabs>
<Tab title="Minimal">
  Required for deployment:

  ```bash
  CODE_PASSWORD=strong_password_here
  ```
</Tab>

<Tab title="Recommended">
  Production-ready configuration:

  ```bash
  CODE_PASSWORD=strong_password_here
  NODE_ENV=production
  RUST_LOG=info
  TZ=America/New_York
  VIBE_PORT=3000
  CADDY_PORT=3001
  ```
</Tab>

<Tab title="Full">
  All available options:

  ```bash
  CODE_PASSWORD=strong_password_here
  SUDO_PASSWORD=admin_password_here
  NODE_ENV=production
  RUST_LOG=info
  TZ=America/New_York
  VIBE_PORT=3000
  CADDY_PORT=3001
  CADDY_CONFIG_PATH=./lets-merge-it/caddy_config
  POSTHOG_API_KEY=phc_...
  POSTHOG_API_ENDPOINT=https://app.posthog.com
  ```
</Tab>
</Tabs>

## Data Persistence

Your data is stored in the `./data/` directory using bind mounts:

| Directory | Contains | Backup Priority |
|-----------|----------|-----------------|
| `./data/db` | SQLite database | 🔴 Critical |
| `./data/repos` | Git repositories | 🔴 Critical |
| `./data/code-server-config` | VSCode settings | 🟡 Optional |
| `./data/caddy-data` | SSL certificates | 🟡 Optional |

<Warning>
Always backup `./data/db` and `./data/repos` before major changes
</Warning>

## Quick Backup

```bash
# Backup all persistent data
mkdir -p backups
tar czf backups/vibe-kanban-backup-$(date +%Y%m%d-%H%M%S).tar.gz ./data

# Or just critical data
tar czf backups/critical-$(date +%Y%m%d).tar.gz ./data/db ./data/repos
```

## Common Issues

<AccordionGroup>
<Accordion title="Build fails">
  **Cause**: Missing environment variables or insufficient resources

  **Fix**:
  - Ensure Coolify has adequate build resources (2GB+ RAM)
  - Check build logs for specific errors
</Accordion>

<Accordion title="Can't access code-server">
  **Cause**: Missing `folder` query parameter or wrong password

  **Fix**:
  - Use URL: `https://your-domain.com?folder=/repos`
  - Verify `CODE_PASSWORD` is set correctly
</Accordion>

<Accordion title="Data lost after restart">
  **Cause**: Data directory not properly mounted or permissions issue

  **Fix**:
  - Verify `./data/` directory exists: `ls -la ./data`
  - Check permissions: `sudo chown -R 1001:1001 ./data`
  - Restore from backup if needed
</Accordion>
</AccordionGroup>

## Health Check Endpoints

Services expose health check endpoints:

```bash
# vibe-kanban
curl http://localhost:3000

# caddy
curl http://localhost:3001
```

<Tip>
Coolify automatically monitors these endpoints and restarts unhealthy services
</Tip>

## Next Steps

<CardGroup cols={2}>
<Card title="Full deployment guide" icon="book" href="/coolify-deployment">
  Detailed step-by-step deployment instructions
</Card>

<Card title="Analysis document" icon="magnifying-glass" href="/coolify-docker-compose-analysis">
  Technical analysis of required changes
</Card>
</CardGroup>

## Support

If you encounter issues:

1. Check Coolify deployment logs
2. Review [troubleshooting section](/coolify-deployment#troubleshooting)
3. Verify environment variables
4. Test local build: `docker build -f Dockerfile .`
