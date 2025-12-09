# SSH Terminal Setup for Code-Server

This document explains how the code-server container is configured to use SSH for terminal access to the host machine, allowing you to run terminal commands on your host system from within the VSCode web interface.

## Overview

The code-server container is configured with:
1. **SSH keys mounted** from the host machine (read-only)
2. **VSCode terminal profile** that uses SSH to connect to `host.docker.internal`
3. **Host gateway mapping** to allow container-to-host networking

## Architecture

```
┌─────────────────────────────────────┐
│  Code-Server Container              │
│  (VSCode Web Interface)             │
│                                     │
│  Terminal Profile: "ssh-host"      │
│  ├─ Uses /usr/bin/ssh              │
│  ├─ SSH Keys: /config/.ssh         │
│  └─ Target: abc@host.docker.internal │
└──────────────┬──────────────────────┘
               │ SSH Connection
               ↓
┌─────────────────────────────────────┐
│  Host Machine (macOS/Linux)         │
│  User: abc                          │
│  Working Directory: ${workspaceFolder}│
│  Shell: zsh -l                      │
└─────────────────────────────────────┘
```

## Configuration Files

### 1. VSCode Settings (`lets-merge-it/config/data/User/settings.json`)

```json
{
    "workbench.colorTheme": "Default Dark Modern",
    "terminal.integrated.defaultProfile.linux": "ssh-host",
    "terminal.integrated.profiles.linux": {
        "ssh-host": {
            "path": "/usr/bin/ssh",
            "args": [
                "-t",
                "abc@host.docker.internal",
                "cd ${workspaceFolder} && exec zsh -l"
            ]
        },
        "bash": {
            "path": "bash",
            "icon": "terminal-bash"
        }
    }
}
```

**Key Points:**
- `ssh-host` is the default terminal profile
- Connects to user `abc` on the host via `host.docker.internal`
- Changes to `${workspaceFolder}` (the current workspace path)
- Starts a login shell with `zsh -l`
- Falls back to local `bash` profile if needed

### 2. Docker Compose Configuration

```yaml
code-server:
  image: lscr.io/linuxserver/code-server:4.105.1
  environment:
    - PUID=1000
    - PGID=1000
  volumes:
    # SSH keys (read-only)
    - ${SSH_KEY_PATH:-~/.ssh}:/config/.ssh:ro
    # Other volumes...
  extra_hosts:
    - "host.docker.internal:host-gateway"
```

**Key Points:**
- SSH keys mounted at `/config/.ssh` (read-only for security)
- `host.docker.internal` resolves to the host's IP address
- `PUID/PGID` match the host user for proper file permissions

### 3. Environment Variables (`.env`)

```bash
# SSH Configuration
SSH_KEY_PATH=~/.ssh
```

## Setup Instructions

### Step 1: Ensure SSH is Configured on Host

1. **Check if SSH server is running** on your host machine:
   ```bash
   # macOS
   sudo systemsetup -getremotelogin

   # Linux
   sudo systemctl status ssh
   ```

2. **Enable SSH if needed**:
   ```bash
   # macOS
   sudo systemsetup -setremotelogin on

   # Linux (Ubuntu/Debian)
   sudo systemctl enable --now ssh
   ```

### Step 2: Set Up SSH Key Authentication

1. **Generate SSH key pair** (if you don't have one):
   ```bash
   ssh-keygen -t ed25519 -C "code-server@docker"
   ```

2. **Add public key to authorized_keys** on the host:
   ```bash
   cat ~/.ssh/id_ed25519.pub >> ~/.ssh/authorized_keys
   chmod 600 ~/.ssh/authorized_keys
   ```

3. **Test SSH connection** from code-server container:
   ```bash
   docker compose exec code-server ssh abc@host.docker.internal
   ```

### Step 3: Configure Environment Variables

1. **Copy `.env.example` to `.env`**:
   ```bash
   cp .env.example .env
   ```

2. **Set SSH_KEY_PATH** (if different from default):
   ```bash
   # .env
   SSH_KEY_PATH=/path/to/your/.ssh
   ```

3. **Update username** in `settings.json` if needed:
   - Replace `abc` with your actual host username
   - Location: `lets-merge-it/config/data/User/settings.json`

### Step 4: Restart Code-Server

```bash
docker compose restart code-server
```

## Verification

### 1. Check SSH Keys are Mounted

```bash
docker compose exec code-server ls -la /config/.ssh
```

You should see your SSH keys listed.

### 2. Test SSH Connection

```bash
docker compose exec code-server ssh -T abc@host.docker.internal whoami
```

Should return your host username.

### 3. Open VSCode Terminal

1. Navigate to `http://localhost:3001/?folder=/path/to/workspace`
2. Open integrated terminal (`Ctrl+` `` or Terminal → New Terminal)
3. Terminal should automatically connect via SSH to the host
4. Verify by running: `hostname` (should show your host machine's hostname)

## Troubleshooting

### Issue: "Permission denied (publickey)"

**Cause:** SSH keys not properly configured or permissions incorrect.

**Solution:**
1. Check key permissions on host:
   ```bash
   chmod 700 ~/.ssh
   chmod 600 ~/.ssh/id_ed25519
   chmod 644 ~/.ssh/id_ed25519.pub
   chmod 600 ~/.ssh/authorized_keys
   ```

2. Ensure public key is in `authorized_keys`:
   ```bash
   cat ~/.ssh/id_ed25519.pub >> ~/.ssh/authorized_keys
   ```

### Issue: "Connection refused" or "No route to host"

**Cause:** SSH server not running on host or firewall blocking connection.

**Solution:**
1. Enable SSH on host (see Step 1 above)
2. Check firewall settings:
   ```bash
   # macOS
   sudo /usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate

   # Linux
   sudo ufw status
   ```

### Issue: "Host key verification failed"

**Cause:** SSH host key not in `known_hosts`.

**Solution:**
1. Accept host key manually:
   ```bash
   docker compose exec code-server ssh-keyscan host.docker.internal >> /config/.ssh/known_hosts
   ```

### Issue: Terminal opens but shows wrong directory

**Cause:** `${workspaceFolder}` not resolving correctly.

**Solution:**
1. Ensure workspace paths match between container and host
2. Check volume mounts in `docker-compose.yml`:
   ```yaml
   volumes:
     - vibe-kanban-worktrees:/var/tmp/vibe-kanban-dev/worktrees
     - /Users/mickmister/code:/Users/mickmister/code
   ```

### Issue: "abc@host.docker.internal: command not found: zsh"

**Cause:** `zsh` not installed on host or user's default shell is different.

**Solution:**
Change terminal profile in `settings.json`:
```json
"args": [
    "-t",
    "abc@host.docker.internal",
    "cd ${workspaceFolder} && exec bash -l"  # Use bash instead
]
```

## Advanced Configuration

### Using Different Shell

Edit `lets-merge-it/config/data/User/settings.json`:

```json
"ssh-host": {
    "path": "/usr/bin/ssh",
    "args": [
        "-t",
        "abc@host.docker.internal",
        "cd ${workspaceFolder} && exec fish -l"  // or any other shell
    ]
}
```

### Adding SSH Config Options

Create `lets-merge-it/config/.ssh/config`:

```
Host host.docker.internal
    User abc
    IdentityFile ~/.ssh/id_ed25519
    StrictHostKeyChecking no
    UserKnownHostsFile /dev/null
    ServerAliveInterval 60
```

Then mount it in `docker-compose.yml`:
```yaml
volumes:
  - ./lets-merge-it/config/.ssh/config:/config/.ssh/config:ro
```

### Using SSH Agent

Mount SSH agent socket:

```yaml
volumes:
  - ${SSH_AUTH_SOCK}:/ssh-agent
environment:
  - SSH_AUTH_SOCK=/ssh-agent
```

## Security Considerations

1. **Read-only SSH keys**: Keys are mounted as read-only (`:ro`) to prevent modification
2. **Limited access**: SSH connection uses specific user account on host
3. **Host firewall**: Consider restricting SSH access to localhost only
4. **Key management**: Use separate SSH keys for code-server vs. personal use
5. **Audit logs**: Monitor SSH access logs on host:
   ```bash
   # macOS
   log show --predicate 'process == "sshd"' --last 1h

   # Linux
   sudo tail -f /var/log/auth.log
   ```

## How It Works

1. **User opens VSCode** in browser at `http://localhost:3001/?folder=/path/to/workspace`
2. **User opens integrated terminal** in VSCode
3. **VSCode executes** the `ssh-host` profile command:
   ```bash
   /usr/bin/ssh -t abc@host.docker.internal "cd ${workspaceFolder} && exec zsh -l"
   ```
4. **Docker resolves** `host.docker.internal` to the host machine's IP
5. **SSH authenticates** using keys from `/config/.ssh`
6. **Host opens shell** in the workspace directory
7. **User runs commands** on the host machine through the VSCode terminal

## Benefits

- ✅ **Host environment**: Access to host tools, PATH, and environment variables
- ✅ **Git integration**: Use host git configuration and credentials
- ✅ **Build tools**: Access to host-installed compilers, SDKs, and build tools
- ✅ **Performance**: Commands run directly on host (no container overhead)
- ✅ **Consistency**: Same environment whether using VSCode locally or via code-server

## Related Documentation

- [DOCKER_SETUP.md](../DOCKER_SETUP.md) - Complete Docker setup guide
- [lets-merge-it/README.md](./README.md) - Architecture and customization
- [Code-Server Documentation](https://github.com/coder/code-server)
- [Docker Host Networking](https://docs.docker.com/network/drivers/host/)
