# Lets Merge It - Dockerized Development Environment

A complete Dockerized development environment that serves both a Node.js application and VSCode (code-server) through a single port using Caddy as a reverse proxy.

## Architecture

This setup includes two Docker services plus your app running on the host:

1. **App (Host Machine)** - Your Node.js/Springboard application running on host port 1340
2. **Code-Server (Docker)** - VSCode in the browser for editing code (port 8443 internally)
3. **Caddy (Docker)** - Reverse proxy serving both services on port 3001

### How It Works

Caddy running in Docker intelligently routes requests:
- Requests with `?folder=*` query parameter → code-server (in Docker)
- Requests to `/stable-*` paths (VSCode assets) → code-server (in Docker)
- All other requests → your application (on host via `host.docker.internal:1340`)

This allows you to:
- Run your app normally on your host machine (port 1340)
- Access your app through Caddy at: `http://localhost:3001`
- Access VSCode at: `http://localhost:3001/?folder=/config/workspace`
- Both services share the same origin, avoiding CORS/SameSite cookie issues
- Develop and debug your app with full access to host resources while code-server runs isolated in Docker

## Prerequisites

- Docker and Docker Compose installed
- pnpm (if developing locally)
- Your application source code in the project directory

## Quick Start

1. **Clone and setup environment:**
   ```bash
   cp .env.example .env
   # Edit .env with your configuration
   ```

2. **Start your application on the host:**
   ```bash
   # In terminal 1 - Start your Node.js app
   pnpm dev
   # Or however you normally start your app on port 1340
   ```

3. **Start Docker services (Caddy + code-server):**
   ```bash
   # In terminal 2 - Start Docker services
   docker-compose up -d
   # or: make up
   ```

4. **Access the services:**
   - Application (via Caddy): http://localhost:3001
   - Application (direct): http://localhost:1340
   - VSCode: http://localhost:3001/?folder=/config/workspace
   - Password: Use the `CODE_PASSWORD` from your `.env` file

## Environment Variables

Copy `.env.example` to `.env` and configure:

- `CODE_PASSWORD` - Password for VSCode access
- `GITHUB_CLIENT_ID` - GitHub OAuth client ID (if using GitHub integration)
- `GITHUB_CLIENT_SECRET` - GitHub OAuth client secret
- `PUBLIC_SITE_URL` - Public URL for the application (default: http://localhost:3001)

## Development Workflow

### Recommended Setup

1. **Start your app on the host (terminal 1):**
   ```bash
   # Your app runs normally on your machine with hot reload
   pnpm dev
   ```

2. **Start Docker services (terminal 2):**
   ```bash
   docker-compose up -d
   ```

3. **View logs:**
   ```bash
   # App logs - check your terminal where you ran pnpm dev
   docker-compose logs -f code-server # VSCode logs
   docker-compose logs -f caddy       # Proxy logs
   ```

4. **Make changes:**
   - Edit code normally on your host machine
   - Your app's hot reload will work as usual
   - Access everything through http://localhost:3001

5. **Access shell in containers:**
   ```bash
   docker-compose exec code-server bash # VSCode container
   docker-compose exec caddy sh         # Caddy container
   ```

### Why This Setup?

Running the app on the host while containerizing code-server and Caddy gives you:
- ✅ Full development experience with hot reload
- ✅ Direct access to host file system and development tools
- ✅ Easier debugging (can attach debugger normally)
- ✅ Code-server isolated in Docker for security
- ✅ Single port access to everything via Caddy
- ✅ No container rebuild needed for code changes

## File Structure

```
.
├── Dockerfile.code-server   # VSCode server container
├── Caddyfile               # Reverse proxy configuration (routes to host + containers)
├── docker-compose.yaml     # Service orchestration (code-server + Caddy)
├── Makefile                # Convenient Docker commands
├── .env                    # Environment variables (create from .env.example)
├── .env.example           # Example environment configuration
├── src/                   # Application source code (runs on host)
├── config/                # Code-server configuration
├── projects/              # Code-server workspace
└── data/                  # Application data

Note: Dockerfile.app exists but is not used in this setup since the app runs on the host.
```

## Docker Networking

The Docker services communicate through the `app-network` bridge network:
- **code-server** service accessible at `code-server:8443` (within Docker network)
- **caddy** exposes port 3001 to the host
- **app** runs on host machine, accessible from Caddy via `host.docker.internal:1340`

Caddy uses:
- Docker service names (e.g., `code-server:8443`) for containers
- `host.docker.internal:1340` to reach the app running on your host machine

The `extra_hosts` configuration in docker-compose.yaml ensures `host.docker.internal` works on all platforms (macOS, Windows, and Linux).

## Volumes

Persistent data is stored in Docker volumes:
- `caddy-data` - Caddy data (SSL certificates, etc.)
- `caddy-config` - Caddy configuration cache

Bind mounts for code-server:
- `./config` → `/config` (code-server settings and data)
- `./projects` → `/config/workspace` (code-server workspace)

Note: The `springboard-data` volume is defined but only used if you run the app in Docker. For the default host-based setup, your app's data is stored normally on your host machine.

## Customization

### Changing Ports

1. **External port (default 3001):**
   Edit `docker-compose.yaml`:
   ```yaml
   caddy:
     ports:
       - "YOUR_PORT:3001"
   ```

2. **Internal app port (default 1340):**
   Edit both:
   - `docker-compose.yaml` (environment `PORT` variable)
   - `Caddyfile` (reverse_proxy address)

### Adding New Routes to Caddy

Edit `Caddyfile` to add custom routing:
```caddy
@custom_route {
    path /api/*
}

handle @custom_route {
    reverse_proxy another-service:8080
}
```

## Troubleshooting

### Services won't start
```bash
docker-compose down
docker-compose up -d --build
```

### Can't access VSCode
- Verify code-server is running: `docker-compose ps`
- Check logs: `docker-compose logs code-server`
- Ensure password is correct in `.env`

### App returns 502 Bad Gateway
- Check app logs: `docker-compose logs app`
- Verify app is listening on port 1340
- Check Caddy logs: `docker-compose logs caddy`

### WebSocket connections failing
- Check Caddy configuration has proper WebSocket headers
- Verify firewall/proxy settings

## Production Considerations

For production deployment:

1. **Use production image builds:**
   - Remove development volume mounts
   - Set `NODE_ENV=production`
   - Consider multi-stage builds for smaller images

2. **Secure code-server:**
   - Use strong passwords
   - Consider disabling in production or restricting access
   - Use HTTPS (Caddy can auto-provision with Let's Encrypt)

3. **Configure Caddy for HTTPS:**
   ```caddy
   yourdomain.com {
       # Your existing configuration
   }
   ```

4. **Resource limits:**
   Add to docker-compose.yaml:
   ```yaml
   app:
     deploy:
       resources:
         limits:
           cpus: '1'
           memory: 1G
   ```

## Maintenance

### Update containers:
```bash
docker-compose pull
docker-compose up -d --build
```

### Clean up:
```bash
docker-compose down -v  # WARNING: Removes volumes!
docker system prune
```

### Backup data:
```bash
# Backup volumes
docker run --rm -v lets-merge-it_springboard-data:/data -v $(pwd):/backup alpine tar czf /backup/springboard-backup.tar.gz -C /data .
```

## License

MIT
