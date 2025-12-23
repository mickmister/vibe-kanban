---
title: "Coolify Docker Compose Changes Summary"
description: "Summary of changes made to adapt the development docker-compose.yml for production Coolify deployment"
---

## Files Created/Modified

### 1. `Dockerfile.coolify` (NEW)

Optimized production Dockerfile with BuildKit cache mount support for faster builds on Coolify.

**BuildKit Optimizations:**
- `# syntax=docker/dockerfile:1` - Enables BuildKit features
- Cache mounts for Alpine packages (`/var/cache/apk`)
- Cache mounts for pnpm store (`/root/.local/share/pnpm/store`)
- Cache mounts for Cargo registry, git, and build artifacts
- `--frozen-lockfile` for deterministic builds

**Build Dependencies:**
- Added `pkgconfig` - Required for OpenSSL detection
- Added `openssl-dev` - Required for Rust openssl-sys crate
- Added `sqlite-dev` - Required for SQLx SQLite support

**Expected Build Times:**
- First build: ~8-12 minutes
- Subsequent builds (code changes): ~2-4 minutes (5-8 min savings!)

### 2. `docker-compose.coolify.yml` (NEW)

Production-ready Docker Compose configuration for Coolify deployment.

Uses `Dockerfile.coolify` which includes BuildKit cache mount optimizations.

**Key Changes:**

#### Volume Strategy
- ❌ Removed: Named Docker volumes
- ✅ Added: Bind mounts to `./data/` directory
  - `./data/db` - SQLite database
  - `./data/repos` - Git repositories
  - `./data/code-server-config` - VSCode configuration
  - `./data/caddy-data` - Caddy SSL certificates
  - `./data/caddy-config` - Caddy internal config

**Why bind mounts?**
- Transparency: All data visible in file system
- Easy backups: `tar czf backup.tar.gz ./data`
- No namespace conflicts between deployments
- Simple inspection and management

#### Port Configuration
All ports now configurable via environment variables:

```yaml
vibe-kanban:
  ports:
    - "${VIBE_PORT:-3000}:3000"

caddy:
  ports:
    - "${CADDY_PORT:-3001}:3001"
```

**Benefits:**
- Multiple deployments on same server (different ports)
- Flexibility for different environments
- No hardcoded values

#### Path Configuration
Caddy config path is now configurable:

```yaml
volumes:
  - ${CADDY_CONFIG_PATH:-./lets-merge-it/caddy_config}:/etc/caddy:ro
```

**Use cases:**
- Default: Uses config in repository
- Override: Point to shared config location
- Absolute path: `/etc/vibe-kanban/caddy-config`

### 3. `.gitignore`

**Added:**
```
# Docker compose persistent data
data/
```

**Reason:** Prevents committing user data (database, repos) to version control.

### 4. Documentation

**Created:**
- `docs/coolify-deployment.md` - Comprehensive deployment guide
- `docs/coolify-deployment-quickstart.md` - Quick start reference
- `docs/coolify-docker-compose-analysis.md` - Technical analysis

**Updated:**
All documentation to reflect bind mount strategy and configurable ports.

## Environment Variables

### Required
```bash
CODE_PASSWORD=your_secure_password
```

### Optional (Ports)
```bash
VIBE_PORT=3000              # Default: 3000
CADDY_PORT=3001             # Default: 3001
```

### Optional (Paths)
```bash
CADDY_CONFIG_PATH=./lets-merge-it/caddy_config  # Default
```

### Optional (Configuration)
```bash
NODE_ENV=production         # Default: production
RUST_LOG=info              # Default: info
TZ=UTC                     # Default: UTC
SUDO_PASSWORD=             # Optional for code-server
POSTHOG_API_KEY=           # Optional analytics
POSTHOG_API_ENDPOINT=      # Optional analytics
```

## Multi-Deployment Support

You can now run multiple deployments on the same server:

### Example: Production + Staging

**Production deployment:**
```bash
# /srv/vibe-kanban-prod/
# .env:
CODE_PASSWORD=prod_password
VIBE_PORT=3000
CADDY_PORT=3001
```

**Staging deployment:**
```bash
# /srv/vibe-kanban-staging/
# .env:
CODE_PASSWORD=staging_password
VIBE_PORT=4000
CADDY_PORT=4001
```

Each deployment has its own:
- `./data/` directory (isolated data)
- Port mappings (no conflicts)
- Network namespace
- Caddy configuration

## Data Directory Structure

After first deployment, the `./data/` directory will be created:

```
./data/
├── db/
│   └── db.sqlite                    # Your application database
├── repos/
│   └── [workspace-directories]/     # Git repositories for tasks
├── code-server-config/
│   ├── data/                        # VSCode extensions
│   └── User/                        # VSCode settings
├── caddy-data/
│   └── caddy/
│       └── certificates/            # Auto-generated SSL certs
└── caddy-config/
    └── [caddy-state]/               # Caddy internal state
```

## Permissions

The application runs as `uid 1001`, so the `./data` directory and all subdirectories should be owned by `1001:1001`.

**If you encounter permission issues:**
```bash
sudo chown -R 1001:1001 ./data
```

**Docker will auto-create directories** with root ownership initially. The application will attempt to use them, but you may need to fix permissions if there are issues.

## Backup Strategy

### Simple Backup
```bash
tar czf backups/vibe-kanban-$(date +%Y%m%d-%H%M%S).tar.gz ./data
```

### Critical Data Only
```bash
tar czf backups/critical-$(date +%Y%m%d).tar.gz ./data/db ./data/repos
```

### Restore
```bash
# Stop services
docker compose -f docker-compose.coolify.yml down

# Restore
tar xzf backups/vibe-kanban-YYYYMMDD-HHMMSS.tar.gz

# Fix permissions
sudo chown -R 1001:1001 ./data

# Start services
docker compose -f docker-compose.coolify.yml up -d
```

## Comparison: Development vs Coolify

| Aspect | Development | Coolify (Production) |
|--------|-------------|---------------------|
| **Dockerfile** | `Dockerfile.dev.alpine` | `Dockerfile` |
| **Source mount** | `.:/app` (live reload) | None (compiled binary) |
| **Volumes** | Named volumes | Bind mounts to `./data/` |
| **Host paths** | Bridges to host filesystem | No host dependencies |
| **Ports** | Hardcoded | Env var configurable |
| **Build** | Incremental dev build | Full production build |
| **User** | May run as root | Runs as uid 1001 |
| **Health checks** | Optional | Required |
| **Hot reload** | Enabled | Disabled |

## Migration from Development

If you have an existing development setup and want to migrate data:

1. **Export from named volumes** (if you have existing dev data):
   ```bash
   docker run --rm \
     -v vibe-kanban_app-data:/source \
     -v $(pwd)/data/db:/target \
     alpine cp -a /source/. /target/
   ```

2. **Fix permissions**:
   ```bash
   sudo chown -R 1001:1001 ./data
   ```

3. **Deploy with new compose file**:
   ```bash
   docker compose -f docker-compose.coolify.yml up -d
   ```

## Testing Locally

Before deploying to Coolify, test locally:

```bash
# Set required environment variables
export CODE_PASSWORD=test_password

# Start services
docker compose -f docker-compose.coolify.yml up -d

# Check logs
docker compose -f docker-compose.coolify.yml logs -f

# Verify health
docker compose -f docker-compose.coolify.yml ps

# Test application
curl http://localhost:3001

# Test code-server
open http://localhost:3001?folder=/repos

# Stop when done
docker compose -f docker-compose.coolify.yml down
```

## Coolify Deployment Checklist

- [ ] Set `CODE_PASSWORD` environment variable in Coolify
- [ ] Set `docker-compose.coolify.yml` as compose file path
- [ ] Configure domain in Coolify (optional)
- [ ] Set additional environment variables (optional)
- [ ] Deploy and wait for health checks
- [ ] Verify `./data/` directory was created
- [ ] Access application via Coolify URL
- [ ] Set up backup strategy
- [ ] Document any custom configuration

## Troubleshooting

### Permission Denied Errors
```bash
sudo chown -R 1001:1001 ./data
```

### Data Not Persisting
- Check `./data` directory exists
- Verify mount paths in logs
- Ensure no `volumes:` section at bottom of compose file

### Port Conflicts
- Check other services using same ports: `netstat -tuln | grep 3000`
- Change `VIBE_PORT` or `CADDY_PORT` environment variables

### Health Checks Failing
- Check service logs: `docker compose -f docker-compose.coolify.yml logs`
- Verify services can reach each other on Docker network
- Ensure sufficient resources (memory/CPU)

## Next Steps

1. Review `docs/coolify-deployment.md` for detailed deployment guide
2. Test deployment locally with `docker-compose.coolify.yml`
3. Deploy to Coolify
4. Set up automated backups
5. Configure monitoring/alerting
