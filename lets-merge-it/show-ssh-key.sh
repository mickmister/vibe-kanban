#!/bin/bash

# Helper script to display SSH setup instructions

echo ""
echo "Displaying SSH setup instructions from code-server..."
echo ""

if docker-compose ps code-server | grep -q "Up"; then
    # Container is running, read from container
    docker-compose exec -T code-server cat /config/.ssh/SETUP_INSTRUCTIONS.txt 2>/dev/null || {
        echo "SSH key not yet generated. The container needs to start at least once."
        echo "Try running: docker-compose up -d code-server"
        exit 1
    }
elif [ -f "./config/.ssh/SETUP_INSTRUCTIONS.txt" ]; then
    # Container not running, but file exists on host
    cat "./config/.ssh/SETUP_INSTRUCTIONS.txt"
else
    echo "SSH key not yet generated. The container needs to start at least once."
    echo "Try running: docker-compose up -d code-server"
    exit 1
fi

echo ""
