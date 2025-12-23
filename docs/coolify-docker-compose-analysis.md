# Docker Compose Analysis for Coolify Deployment

## Overview
The current `docker-compose.yml` is configured for local development with host-specific paths and live-reload capabilities. For Coolify deployment, significant changes are needed to create a production-ready, self-contained deployment.

## Current Issues for Coolify

### 1. Host-Specific Path Mounts (Critical)
**Lines 31-32, 57-58** - Hard-coded paths specific to developer's macOS machine:
```yaml
- /private/var/folders/1j/j9tl30f930d27ck98gv5jpg80000gn:/private/var/folders/1j/j9tl30f930d27ck98gv5jpg80000gn
- /Users/mickmister/code:/Users/mickmister/code
```

**Impact**: These paths won't exist on Coolify servers and will cause deployment failures.

**Solution**: Remove entirely for Coolify. These were only needed for git worktree bridging between host and container.

### 2. Development-Oriented Service Configuration

#### vibe-kanban Service
**Issues**:
- Uses `Dockerfile.dev.alpine` (line 21) - development build
- Mounts entire source directory (line 26: `.:/app`) for live reload
- Excludes node_modules/target with anonymous volumes (lines 27-30)
- Development environment variables (lines 36-37)
- `DISABLE_WORKTREE_ORPHAN_CLEANUP=1` (line 40) - dev-specific

**Solution**:
- Switch to production Dockerfile
- Remove source code volume mount
- Build application into container image
- Use production environment variables
- Remove worktree-related settings if not needed in production

### 3. code-server Service (Lines 46-68)

**Purpose**: Provides web-based VSCode for development

**Issues for Coolify**:
- Entire service is development-focused
- Requires SSH keys mount (line 60)
- Uses host-specific paths
- Adds security surface area
- `extra_hosts` for host.docker.internal (lines 63-64)

**Solution**:
- **Option A**: Remove entirely if not needed in production
- **Option B**: Make optional/conditional if needed for debugging
- **Option C**: Separate deployment for development environments only

### 4. Caddy Reverse Proxy (Lines 70-84)

**Current Setup**:
- Listens on port 3001
- Routes to code-server
- Uses local config directory

**Coolify Consideration**:
- Coolify typically handles reverse proxy/SSL termination
- May be redundant with Coolify's built-in proxy
- Local config mount (line 75: `./lets-merge-it/caddy_config`) won't work

**Solution**:
- **Option A**: Remove - let Coolify handle routing
- **Option B**: Reconfigure if internal routing needed between services
- **Option C**: Use for specific internal proxy needs only

### 5. Port Configuration

**Current**:
```yaml
ports:
  - "3000:3000"  # Frontend
  - "8080:8080"  # Backend
```

**Coolify Consideration**:
- Coolify manages port mapping
- May only need to expose one port if backend serves frontend
- Or use Coolify's multi-port service features

**Solution**: Review architecture - does backend serve frontend bundle or are they separate?

### 6. Data Persistence

**Current Volumes**:
- `caddy-data`, `caddy-config` - May not be needed
- `vibe-kanban-worktrees` - Development feature for git worktrees

**Missing**:
- No volume for application data (unless in `./data` from commented service)
- No volume for SQLite/database if applicable
- No volume for user uploads or generated assets

**Solution**: Define proper persistent volumes for:
- Application database
- User-generated content
- Configuration (if not in env vars)

### 7. Environment Variables

**Current**: Hard-coded in docker-compose
- `NODE_ENV=development`
- `RUST_LOG=debug`
- Fixed ports

**Coolify Best Practice**:
- Use Coolify's environment variable management
- Remove from docker-compose, make configurable
- Use production defaults

## Recommended Changes for Coolify

### Phase 1: Minimal Production Setup

1. **Remove development-specific services**
   - Remove or make optional: `code-server`, `caddy`

2. **Update vibe-kanban service**
   ```yaml
   vibe-kanban:
     build:
       context: .
       dockerfile: Dockerfile  # Production dockerfile
     ports:
       - "${PORT:-8080}:8080"
     volumes:
       - app-data:/app/data  # Persistent application data
     environment:
       - NODE_ENV=production
       - RUST_LOG=${RUST_LOG:-info}
       - FRONTEND_PORT=${FRONTEND_PORT:-3000}
       - BACKEND_PORT=${BACKEND_PORT:-8080}
     restart: unless-stopped
   ```

3. **Define necessary volumes only**
   ```yaml
   volumes:
     app-data:  # For application database/assets
   ```

### Phase 2: Production Considerations

1. **Health Checks**: Add health check endpoints and configuration
2. **Resource Limits**: Define memory/CPU limits
3. **Security**: Run as non-root user, minimal permissions
4. **Logging**: Configure proper log drivers
5. **Secrets Management**: Use Coolify's secrets for sensitive data

### Phase 3: Optional Services

If you need code-server or internal routing:
1. Create separate docker-compose files
2. Use Coolify's service dependencies
3. Gate behind authentication/VPN

## Questions to Answer

1. **Architecture**: Does the backend serve the frontend, or are they separate services?
2. **Data Persistence**: What data needs to persist? Database location?
3. **Worktrees**: Are git worktrees needed in production, or just development?
4. **Code Server**: Is web-based VSCode needed in production/staging?
5. **Assets**: Where are static assets stored? In image or persistent volume?
6. **Database**: Using SQLite (needs volume) or external database?

## Next Steps

1. Review application architecture to answer questions above
2. Check if production Dockerfile exists or needs creation
3. Identify all persistent data requirements
4. Create new `docker-compose.coolify.yml` with production settings
5. Test locally with production-like settings
6. Document Coolify-specific environment variables needed
