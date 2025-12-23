---
title: "Coolify Deployment Guide"
description: "Deploy Vibe Kanban to Coolify using Docker Compose with persistent data, code-server access, and production configuration"
---

## Overview

This guide explains how to deploy Vibe Kanban to Coolify using the production-ready `docker-compose.coolify.yml` configuration. The deployment includes:

- Production-optimised Vibe Kanban application
- Web-based VSCode (code-server) for remote debugging
- Caddy reverse proxy for intelligent routing
- Persistent volumes for data and configuration

## Architecture

The Coolify deployment consists of three services:

<CardGroup cols={3}>
<Card title="Vibe Kanban" icon="rocket">
  Main application serving both frontend and backend on port 3000
</Card>

<Card title="code-server" icon="code">
  Web-based VSCode for debugging and administration
</Card>

<Card title="Caddy" icon="globe">
  Reverse proxy routing traffic to appropriate services
</Card>
</CardGroup>

### Service Flow

```
User Request → Caddy (Port 3001)
                ├─→ VSCode queries/paths → code-server (8443)
                └─→ All other requests → vibe-kanban (3000)
```

## Prerequisites

Before deploying to Coolify, ensure you have:

- Coolify instance set up and accessible (v4.0.0-beta.450 or later recommended)
- GitHub repository containing your Vibe Kanban code
- Required environment variables (detailed below)

<Tip>
The deployment uses `Dockerfile.coolify` which includes BuildKit cache mounts for faster rebuilds (5-8 minute time savings on subsequent builds)
</Tip>

## Environment Variables

Configure these variables in Coolify's environment settings:

<AccordionGroup>
<Accordion title="Required Variables">

<ParamField path="CODE_PASSWORD" type="string" required>
Password for accessing code-server (VSCode). Use a strong password for production.
</ParamField>

</Accordion>

<Accordion title="Optional Variables">

<ParamField path="NODE_ENV" type="string" default="production">
Node.js environment mode. Use `production` for Coolify deployments.
</ParamField>

<ParamField path="RUST_LOG" type="string" default="info">
Rust logging level. Options: `error`, `warn`, `info`, `debug`, `trace`
</ParamField>

<ParamField path="TZ" type="string" default="UTC">
Timezone for code-server and log timestamps
</ParamField>

<ParamField path="VIBE_PORT" type="string" default="3000">
External port for Vibe Kanban application. Coolify will map this to your domain.
</ParamField>

<ParamField path="CADDY_PORT" type="string" default="3001">
Port for Caddy reverse proxy. Coolify will map this to external port.
</ParamField>

<ParamField path="CADDY_CONFIG_PATH" type="string" default="./lets-merge-it/caddy_config">
Path to Caddy configuration directory. Can be absolute or relative to repository root.
</ParamField>

<ParamField path="SUDO_PASSWORD" type="string">
Optional sudo password for code-server root access
</ParamField>

<ParamField path="POSTHOG_API_KEY" type="string">
PostHog analytics API key for telemetry (optional)
</ParamField>

<ParamField path="POSTHOG_API_ENDPOINT" type="string">
PostHog API endpoint URL (optional)
</ParamField>

</Accordion>
</AccordionGroup>

## Deployment Steps

<Steps>
<Step title="Connect repository to Coolify">
  In Coolify dashboard:

  1. Navigate to **Projects** → **New Resource**
  2. Select **Docker Compose**
  3. Connect your GitHub repository
  4. Select the branch you want to deploy
</Step>

<Step title="Configure Docker Compose file">
  In the repository settings:

  1. Set **Docker Compose File Path** to `docker-compose.coolify.yml`
  2. Coolify will automatically detect the services

  <Check>
  Verify that Coolify shows 3 services: vibe-kanban, code-server, and caddy
  </Check>
</Step>

<Step title="Set environment variables">
  In the **Environment** tab, add the required variables:

  ```bash
  CODE_PASSWORD=your_secure_password_here
  NODE_ENV=production
  RUST_LOG=info
  TZ=UTC
  ```

  <Warning>
  Never use weak passwords for CODE_PASSWORD in production environments
  </Warning>
</Step>

<Step title="Configure port mappings">
  Coolify automatically maps ports from the compose file:

  - **Port 3001** (Caddy) → Your chosen external port/domain
  - Internal ports (3000, 8443) remain internal to Docker network

  <Tip>
  Configure your domain in Coolify to automatically handle SSL with Let's Encrypt
  </Tip>
</Step>

<Step title="Deploy the application">
  Click **Deploy** in Coolify dashboard

  Coolify will:
  1. Clone your repository
  2. Build the Dockerfile
  3. Start all services
  4. Run health checks
  5. Map domains/ports

  <Check>
  Monitor deployment logs to ensure all services start successfully
  </Check>
</Step>

<Step title="Verify deployment">
  Access your application:

  - **Main app**: `https://your-domain.com`
  - **VSCode**: `https://your-domain.com?folder=/repos`

  Test the health check endpoint:

  ```bash
  curl https://your-domain.com
  ```

  <ResponseExample>
  ```
  HTTP/2 200 OK
  ```
  </ResponseExample>
</Step>
</Steps>

## Persistent Data

The deployment uses bind mounts to persist critical data in the `./data/` directory:

### Data Directory Structure

All persistent data is stored in `./data/` relative to the repository root:

```
./data/
├── db/                      # SQLite database and app data
├── repos/                   # Git repositories and workspaces
├── code-server-config/      # VSCode configuration and extensions
├── caddy-data/              # Caddy SSL certificates
└── caddy-config/            # Caddy internal configuration
```

<Tabs>
<Tab title="db">
  **Host Path**: `./data/db`

  **Container Path**: `/home/appuser/.local/share/vibe-kanban`

  **Contains**:
  - SQLite database (`db.sqlite`)
  - Application configuration
  - User preferences

  <Warning>
  This directory contains your database. Ensure regular backups.
  </Warning>
</Tab>

<Tab title="repos">
  **Host Path**: `./data/repos`

  **Container Path**: `/repos`

  **Contains**:
  - Git repositories created by the application
  - Workspace data
  - Task-related files
</Tab>

<Tab title="code-server-config">
  **Host Path**: `./data/code-server-config`

  **Container Path**: `/config`

  **Contains**:
  - VSCode extensions
  - User settings
  - SSH keys (if configured)
</Tab>

<Tab title="caddy-data & caddy-config">
  **Host Path**: `./data/caddy-data` and `./data/caddy-config`

  **Container Path**: `/data` and `/config`

  **Contains**:
  - Caddy certificates
  - Internal routing configuration
</Tab>
</Tabs>

## Accessing code-server

To access the web-based VSCode:

1. Navigate to your domain with the `folder` query parameter:
   ```
   https://your-domain.com?folder=/repos
   ```

2. Enter the `CODE_PASSWORD` you configured

3. You'll have full VSCode access to the `/repos` directory

<Tip>
Use code-server for debugging production issues or making quick configuration changes
</Tip>

## Health Checks

All services include health checks for reliability:

<CardGroup cols={3}>
<Card title="vibe-kanban" icon="heart-pulse">
  Checks HTTP endpoint every 30s
  - 10s start period
  - 3 retries before unhealthy
</Card>

<Card title="code-server" icon="heart-pulse">
  Checks `/healthz` endpoint every 30s
  - 10s start period
  - 3 retries before unhealthy
</Card>

<Card title="caddy" icon="heart-pulse">
  Checks proxy endpoint every 30s
  - 5s start period
  - 3 retries before unhealthy
</Card>
</CardGroup>

Coolify uses these health checks to:
- Determine when services are ready
- Restart unhealthy containers
- Show service status in dashboard

## Troubleshooting

<AccordionGroup>
<Accordion title="Service fails health check">
  **Symptoms**: Coolify shows service as unhealthy or constantly restarting

  **Solutions**:
  1. Check service logs in Coolify dashboard
  2. Verify environment variables are set correctly
  3. Ensure sufficient resources (memory/CPU)
  4. Check if volumes are mounted correctly

  ```bash
  # View logs for specific service
  docker compose -f docker-compose.coolify.yml logs vibe-kanban
  ```
</Accordion>

<Accordion title="Cannot access code-server">
  **Symptoms**: VSCode doesn't load or password doesn't work

  **Solutions**:
  1. Verify `CODE_PASSWORD` environment variable is set
  2. Check Caddy routing configuration
  3. Ensure URL includes `?folder=/repos` parameter
  4. Review code-server logs for errors

  <Tip>
  Try accessing code-server directly via internal port for testing
  </Tip>
</Accordion>

<Accordion title="Database data lost after restart">
  **Symptoms**: Tasks/projects disappear after redeployment

  **Solutions**:
  1. Verify `./data/db` directory exists and is mounted
  2. Check directory permissions (should be owned by uid 1001)
  3. Ensure data directory wasn't accidentally deleted or moved

  ```bash
  # Check data directory exists
  ls -la ./data/db

  # Check ownership
  ls -ldn ./data/db

  # Fix permissions if needed
  sudo chown -R 1001:1001 ./data/db
  ```

  <Warning>
  Always backup ./data directory before major deployments or migrations
  </Warning>
</Accordion>

<Accordion title="Build fails during deployment">
  **Symptoms**: Deployment fails during Docker build phase

  **Common causes**:
  - Missing build dependencies
  - OpenSSL compilation errors (`openssl-sys` crate)
  - Rust compilation errors
  - Frontend build failures
  - Network timeouts during dependency installation

  **Solutions**:
  1. Review build logs for specific error
  2. Ensure adequate build resources in Coolify (2GB+ RAM recommended)
  3. Verify using `Dockerfile.coolify` (not `Dockerfile`)
  4. Check BuildKit is enabled (default in Coolify v4+)
  5. Try local build: `DOCKER_BUILDKIT=1 docker build -f Dockerfile.coolify .`

  **If seeing OpenSSL errors:**
  - Ensure `Dockerfile.coolify` includes `openssl-dev`, `pkgconfig`, and `sqlite-dev`
  - These dependencies are required for Rust compilation
</Accordion>

<Accordion title="Caddy routing not working correctly">
  **Symptoms**: Wrong service responds to requests

  **Solutions**:
  1. Verify Caddyfile configuration in `lets-merge-it/caddy_config/`
  2. Check service dependencies are healthy
  3. Review Caddy logs for routing decisions
  4. Ensure internal DNS resolves service names correctly

  ```bash
  # Test internal service resolution from Caddy container
  docker exec <caddy-container> nslookup vibe-kanban
  ```
</Accordion>
</AccordionGroup>

## Backup and Restore

### Creating Backups

<Steps>
<Step title="Stop the services (optional)">
  For consistent backups, stop services first:

  ```bash
  docker compose -f docker-compose.coolify.yml down
  ```

  <Tip>
  You can backup while running, but stopping ensures data consistency
  </Tip>
</Step>

<Step title="Backup data directory">
  ```bash
  # Create backup directory
  mkdir -p backups

  # Backup all persistent data
  tar czf backups/vibe-kanban-data-$(date +%Y%m%d-%H%M%S).tar.gz ./data

  # Or backup individual directories
  tar czf backups/db-$(date +%Y%m%d).tar.gz ./data/db
  tar czf backups/repos-$(date +%Y%m%d).tar.gz ./data/repos
  ```

  <Check>
  Verify backup file was created and has reasonable size
  </Check>
</Step>

<Step title="Restart services (if stopped)">
  ```bash
  docker compose -f docker-compose.coolify.yml up -d
  ```
</Step>
</Steps>

### Restoring from Backup

<Steps>
<Step title="Stop services">
  ```bash
  docker compose -f docker-compose.coolify.yml down
  ```

  <Warning>
  This will stop all services. Ensure you're ready for downtime.
  </Warning>
</Step>

<Step title="Remove existing data (optional)">
  If you want to completely replace existing data:

  ```bash
  # CAUTION: This deletes all existing data
  rm -rf ./data
  ```

  Or backup existing data first:
  ```bash
  mv ./data ./data.backup.$(date +%Y%m%d-%H%M%S)
  ```
</Step>

<Step title="Restore from backup">
  ```bash
  # Extract full backup
  tar xzf backups/vibe-kanban-data-YYYYMMDD-HHMMSS.tar.gz

  # Or restore individual directories
  mkdir -p ./data
  tar xzf backups/db-YYYYMMDD.tar.gz
  tar xzf backups/repos-YYYYMMDD.tar.gz
  ```

  <Tip>
  Ensure the extracted files have correct ownership (uid 1001)
  </Tip>
</Step>

<Step title="Fix permissions">
  ```bash
  # Ensure correct ownership for the app user
  sudo chown -R 1001:1001 ./data
  ```
</Step>

<Step title="Start services">
  ```bash
  docker compose -f docker-compose.coolify.yml up -d
  ```

  <Check>
  Verify data was restored correctly by checking the application
  </Check>
</Step>
</Steps>

## Security Considerations

<Warning>
Follow these security best practices for production deployments:
</Warning>

1. **Strong passwords**: Use complex passwords for `CODE_PASSWORD` and `SUDO_PASSWORD`
2. **Access control**: Restrict code-server access via firewall or VPN
3. **Regular updates**: Keep Docker images updated for security patches
4. **Volume permissions**: Ensure volumes have appropriate ownership (uid 1001)
5. **Secrets management**: Use Coolify's secrets feature for sensitive variables
6. **Network isolation**: Services communicate via internal Docker network only
7. **SSL/TLS**: Use Coolify's automatic HTTPS with Let's Encrypt

## Performance Tuning

### Resource Limits

For production workloads, consider adding resource limits to `docker-compose.coolify.yml`:

```yaml
services:
  vibe-kanban:
    deploy:
      resources:
        limits:
          cpus: '2.0'
          memory: 2G
        reservations:
          cpus: '1.0'
          memory: 1G
```

### Database Optimisation

The SQLite database is optimised for concurrent access, but for high-traffic deployments:

1. Monitor database file size in `app-data` volume
2. Consider periodic vacuum operations
3. Implement regular backups
4. Monitor query performance via `RUST_LOG=debug`

## Differences from Development Setup

The Coolify deployment differs from local development (`docker-compose.yml`):

| Aspect | Development | Production (Coolify) |
|--------|-------------|----------------------|
| Dockerfile | `Dockerfile.dev.alpine` | `Dockerfile` |
| Source mounting | Full source mounted | Compiled binary only |
| Hot reload | Enabled | Disabled |
| Host paths | Bridges to host worktrees | No host dependencies |
| Build time | Fast (incremental) | Slower (full build) |
| Image size | Larger (includes dev tools) | Minimal (Alpine runtime) |
| Security | Runs as root | Runs as appuser (1001) |
| Health checks | Optional | Required |

## Next Steps

<CardGroup cols={2}>
<Card title="Monitor performance" icon="chart-line" href="#performance-tuning">
  Set up monitoring and configure resource limits
</Card>

<Card title="Configure backups" icon="database" href="#backup-and-restore">
  Implement automated backup strategy
</Card>

<Card title="Scale deployment" icon="arrows-maximize">
  Consider horizontal scaling for high traffic
</Card>

<Card title="Custom domain" icon="globe">
  Configure custom domain and SSL in Coolify
</Card>
</CardGroup>
