#!/usr/bin/with-contenv bash

# This script runs as part of the container initialization
# It sets up SSH keys for connecting to the host

SSH_DIR="/config/.ssh"
SSH_KEY="${SSH_DIR}/id_ed25519"
SETUP_MARKER="${SSH_DIR}/.ssh_setup_complete"

# Exit if already set up
if [ -f "${SETUP_MARKER}" ]; then
    exit 0
fi

echo "Setting up SSH for host connection..."

# Ensure .ssh directory has correct permissions
chmod 700 "${SSH_DIR}"

# Generate SSH key if it doesn't exist
if [ ! -f "${SSH_KEY}" ]; then
    echo "Generating SSH key..."
    ssh-keygen -t ed25519 -f "${SSH_KEY}" -N "" -C "code-server-to-host"
    chmod 600 "${SSH_KEY}"
    chmod 644 "${SSH_KEY}.pub"
fi

# Ensure SSH files are owned by the user (not root)
chown -R ${PUID:-1000}:${PGID:-1000} "${SSH_DIR}"

# Set correct permissions on SSH config
if [ -f "${SSH_DIR}/config" ]; then
    chmod 600 "${SSH_DIR}/config"
fi

# Write instructions to a file for easy access
INSTRUCTIONS_FILE="${SSH_DIR}/SETUP_INSTRUCTIONS.txt"
cat > "${INSTRUCTIONS_FILE}" << EOF
==========================================
SSH Setup Instructions
==========================================

To enable SSH access from code-server to the host, run this command on your HOST machine:

cat >> ~/.ssh/authorized_keys << 'SSHKEY'
$(cat "${SSH_KEY}.pub")
SSHKEY

Then set proper permissions:
chmod 600 ~/.ssh/authorized_keys

==========================================

After adding the key, you can test the connection from inside code-server:
ssh mickmister@host.docker.internal

The VS Code terminal will automatically use SSH to connect to the host.
EOF

chmod 644 "${INSTRUCTIONS_FILE}"

# Also output to logs (will be visible in docker-compose logs)
echo ""
echo "=========================================="
echo "SSH Setup Complete"
echo "=========================================="
echo ""
echo "SSH key generated. To view setup instructions, run:"
echo ""
echo "  docker-compose exec code-server cat /config/.ssh/SETUP_INSTRUCTIONS.txt"
echo ""
echo "Or view the file at: ./lets-merge-it/config/.ssh/SETUP_INSTRUCTIONS.txt"
echo ""
echo "=========================================="
echo ""

# Mark setup as complete
touch "${SETUP_MARKER}"
