#!/usr/bin/env bash
set -euo pipefail

# Docker-only smoke harness for deterministic QA executor tests.
# This intentionally runs test commands through `docker exec`; do not port
# workflow E2E tests to run directly on the host.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image="${VK_QA_E2E_IMAGE:-vibe-kanban-qa-scripted-e2e:local}"
container="${VK_QA_E2E_CONTAINER:-vk-qa-scripted-e2e-$$}"

print_plan() {
  cat <<PLAN
Docker scripted QA smoke plan:
1. docker build --target builder --build-arg VK_CARGO_FEATURES=qa-mode -t ${image} -f ${repo_root}/Dockerfile ${repo_root}
2. docker run -d --name ${container} --entrypoint sleep ${image} infinity
3. docker exec ${container} env CARGO_BUILD_JOBS=1 cargo test -p executors qa_mock --features qa-mode
4. docker exec ${container} env CARGO_BUILD_JOBS=1 cargo check -p server --features qa-mode
5. docker rm -f ${container}
PLAN
}

if [[ "${1:-}" == "--plan" || "${DRY_RUN:-}" == "1" ]]; then
  print_plan
  exit 0
fi

cleanup() {
  docker rm -f "${container}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

print_plan

docker build \
  --target builder \
  --build-arg VK_CARGO_FEATURES=qa-mode \
  -t "${image}" \
  -f "${repo_root}/Dockerfile" \
  "${repo_root}"

docker run -d --name "${container}" --entrypoint sleep "${image}" infinity >/dev/null

docker exec "${container}" env CARGO_BUILD_JOBS=1 cargo test -p executors qa_mock --features qa-mode
docker exec "${container}" env CARGO_BUILD_JOBS=1 cargo check -p server --features qa-mode
