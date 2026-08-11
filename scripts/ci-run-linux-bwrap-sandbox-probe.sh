#!/usr/bin/env bash
set -euo pipefail

# Linux/bwrap acceptance probe for the agent sandbox MVP.
#
# From macOS or another non-Linux host with Docker/OrbStack, run:
#   scripts/ci-run-linux-bwrap-sandbox-probe.sh
#
# The wrapper starts an Ubuntu Linux container with bubblewrap installed, then
# runs the qa-mode sandbox probe test that exercises the prepared/sandboxed child
# process: workspace writes allowed, readonly node_modules writes denied, and
# network=none denied/unreachable.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="${VK_LINUX_BWRAP_ACCEPTANCE_IMAGE:-ubuntu:24.04}"

if [[ "${VK_LINUX_BWRAP_ACCEPTANCE_IN_CONTAINER:-}" != "1" ]]; then
  if ! command -v docker >/dev/null 2>&1; then
    echo "error: docker is required to run Linux bwrap acceptance from $(uname -s)" >&2
    exit 1
  fi

  exec docker run --rm \
    --privileged \
    --security-opt seccomp=unconfined \
    --network bridge \
    -e VK_LINUX_BWRAP_ACCEPTANCE_IN_CONTAINER=1 \
    -e CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}" \
    -e CARGO_TARGET_DIR=/tmp/vk-target \
    -v "$REPO_ROOT:/workspace/vibe-kanban:rw" \
    -w /workspace/vibe-kanban \
    "$IMAGE" \
    bash scripts/ci-run-linux-bwrap-sandbox-probe.sh
fi

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "error: Linux bwrap acceptance must run on Linux; got $(uname -s)" >&2
  exit 1
fi

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends \
  bubblewrap \
  ca-certificates \
  curl \
  git \
  build-essential \
  pkg-config \
  libssl-dev \
  cmake \
  clang

if ! command -v bwrap >/dev/null 2>&1; then
  echo "error: bubblewrap installed but bwrap is not on PATH" >&2
  exit 1
fi
bwrap --version

if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
    | sh -s -- -y --profile default --default-toolchain none
  # shellcheck source=/dev/null
  source "$HOME/.cargo/env"
fi

rustup toolchain install nightly-2025-12-04 --profile default
rustup default nightly-2025-12-04

cargo test -p executors bwrap_ -- --nocapture
cargo test -p executors --features qa-mode qa_sandbox_probe_runs_inside_prepared_sandbox -- --nocapture
