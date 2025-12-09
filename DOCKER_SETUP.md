# Docker Setup Guide for Vibe Kanban

This guide explains how to run Vibe Kanban in Docker with an integrated VSCode web interface.

## Architecture

The Docker setup includes three services:

1. **vibe-kanban** - Vibe Kanban application (frontend on port 3000, backend on port 8080)
2. **code-server** - VSCode in the browser for editing code (port 8443 internally)
3. **caddy** - Reverse proxy serving everything on port 3001

### How It Works

Caddy intelligently routes requests based on URL patterns:
- Requests with `?folder=*` query parameter → code-server
- Requests to `/stable-*` paths (VSCode assets) → code-server
- All other requests → vibe-kanban frontend

This allows you to:
- Access Vibe Kanban at: `http://localhost:3001`
- Access VSCode at: `http://localhost:3001/?folder=/config/workspace`
- Develop and debug with full hot-reload support

## Prerequisites

- Docker and Docker Compose installed
- 2+ GB of free disk space for Docker images
- For Apple Silicon (M1/M2/M3): Set `CODE_SERVER_PLATFORM=linux/arm64` in .env
- For Intel/AMD: Set `CODE_SERVER_PLATFORM=linux/amd64` in .env

## Quick Start

1. **Create your environment file:**
   ```bash
   cp .env.example .env
   # Edit .env with your configuration
   ```

2. **Build the Docker images:**
   ```bash
   docker compose build
   ```

3. **Start all services:**
   ```bash
   docker compose up -d
   ```

4. **Access the services:**
   - Vibe Kanban (via Caddy): http://localhost:3001
   - Vibe Kanban Frontend (direct): http://localhost:3000
   - Vibe Kanban Backend (direct): http://localhost:8080
   - VSCode: http://localhost:3001/?folder=/config/workspace
   - VSCode Password: Use the `CODE_PASSWORD` from your `.env` file

## Environment Variables

The `.env.example` file contains all configurable options. Key variables:

```bash
# Required for code-server
CODE_PASSWORD=your_secure_password_here
CODE_SERVER_PLATFORM=linux/arm64  # or linux/amd64 for Intel

# Development settings
NODE_ENV=development
RUST_LOG=debug
FRONTEND_PORT=3000
BACKEND_PORT=8080

# GitHub OAuth (optional, for GitHub integration)
GITHUB_CLIENT_ID=your_github_client_id
GITHUB_CLIENT_SECRET=your_github_client_secret

# Public URL
PUBLIC_SITE_URL=http://localhost:3001
```

## Common Commands

### Starting Services
```bash
# Start all services in background
docker compose up -d

# Start all services with logs
docker compose up

# Start specific service
docker compose up -d vibe-kanban
```

### Viewing Logs
```bash
# All services
docker compose logs -f

# Specific service
docker compose logs -f vibe-kanban
docker compose logs -f code-server
docker compose logs -f caddy
```

### Stopping Services
```bash
# Stop all services
docker compose down

# Stop and remove volumes
docker compose down -v
```

### Rebuilding After Changes
```bash
# Rebuild all images
docker compose build --no-cache

# Rebuild specific service
docker compose build --no-cache vibe-kanban
```

### Accessing Container Shell
```bash
# Vibe Kanban container
docker compose exec vibe-kanban sh

# Code-server container
docker compose exec code-server bash
```

## Development Workflow

### Hot Reload Development

The Docker setup includes hot-reload for both frontend and backend:

1. Source code is mounted as a volume, so changes on your host are immediately reflected
2. `pnpm run dev` runs inside the container with hot-reload enabled
3. Frontend uses Vite HMR (Hot Module Replacement)
4. Backend uses cargo-watch for automatic recompilation

### Making Code Changes

1. Edit files on your host machine using your preferred IDE
2. Changes are synced to the Docker container via volume mounts
3. Services automatically reload
4. View changes at http://localhost:3001

### Using VSCode in Browser

1. Navigate to http://localhost:3001/?folder=/config/workspace
2. Enter your `CODE_PASSWORD`
3. Open the `/app` folder in the VSCode interface
4. Edit code directly in the browser with full VSCode features

## Troubleshooting

### Docker Build Fails with Hash Mismatch

If you see Debian mirror hash mismatch errors, the Alpine-based Dockerfile (`Dockerfile.dev.alpine`) should resolve this. Make sure `docker-compose.yml` points to this file:

```yaml
vibe-kanban:
  build:
    dockerfile: Dockerfile.dev.alpine
```

### Port Already in Use

If ports 3000, 3001, or 8080 are already in use:

1. Stop the service using that port, or
2. Edit `.env` to change the port mappings:
   ```bash
   FRONTEND_PORT=3050  # Change from 3000
   ```

### Container Won't Start

Check logs for the failing service:
```bash
docker compose logs vibe-kanban
```

Common issues:
- Missing environment variables (check `.env` file)
- Insufficient memory (Docker needs at least 2GB RAM)
- File permission issues (ensure workspace path is readable)

### Code-Server Password Not Working

1. Verify `CODE_PASSWORD` is set in `.env`
2. Restart the code-server container:
   ```bash
   docker compose restart code-server
   ```

### Changes Not Reflected

1. Verify volume mounts are working:
   ```bash
   docker compose exec vibe-kanban ls -la /app
   ```
2. Check if hot-reload is running:
   ```bash
   docker compose logs -f vibe-kanban | grep -i "reload\|watching"
   ```

## Architecture Details

### Dockerfile.dev.alpine

Uses Alpine Linux as the base image for:
- Smaller image size (~1GB vs 2-3GB for Debian)
- More reliable package repositories (no hash mismatch issues)
- Faster build times with layer caching

Includes:
- Node.js 20 with pnpm
- Rust nightly toolchain
- Build dependencies (clang, llvm, sqlite, openssl)
- Development tools (git, bash)

### Volume Mounts

The docker-compose.yml mounts several volumes:
- `.:/app` - Source code (for hot-reload)
- `/app/node_modules` - Node dependencies (cached in container)
- `/app/target` - Rust build artifacts (cached in container)

### Networking

All services communicate via the `app-network` Docker bridge network:
- Services reference each other by service name (e.g., `vibe-kanban:3000`)
- Host machine accessible via `host.docker.internal`

## Additional Resources

- [Docker Compose Documentation](https://docs.docker.com/compose/)
- [Caddy Documentation](https://caddyserver.com/docs/)
- [code-server Documentation](https://coder.com/docs/code-server)
- Vibe Kanban main README: `README.md`
- Development guide: `CLAUDE.md`
