#!/usr/bin/env node
import { execFileSync, spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';

const repoRoot = path.resolve(import.meta.dirname, '..', '..');

const sourcePaths = {
  codex: path.join('crates', 'executors', 'src', 'executors', 'codex.rs'),
  claude: path.join('crates', 'executors', 'src', 'executors', 'claude.rs'),
  executorsCargoToml: path.join('crates', 'executors', 'Cargo.toml'),
  cargoLock: 'Cargo.lock',
};

const fixtures = [
  {
    cli: 'codex',
    package: '@openai/codex',
    currentVersion: readPackageVersion(sourcePaths.codex, '@openai/codex@'),
    previousVersion: derivePreviousPackageVersion(
      sourcePaths.codex,
      '@openai/codex@',
      readPackageVersion(sourcePaths.codex, '@openai/codex@')
    ),
  },
  {
    cli: 'claude',
    package: '@anthropic-ai/claude-code',
    currentVersion: readPackageVersion(
      sourcePaths.claude,
      '@anthropic-ai/claude-code@'
    ),
    previousVersion: derivePreviousPackageVersion(
      sourcePaths.claude,
      '@anthropic-ai/claude-code@',
      readPackageVersion(sourcePaths.claude, '@anthropic-ai/claude-code@')
    ),
  },
];

runNormalizeCheck();
checkCodexProtocolVersions(fixtures[0].currentVersion);

for (const fixture of fixtures) {
  checkMetadata(fixture, 'current', fixture.currentVersion);
  checkMetadata(fixture, 'previous', fixture.previousVersion);
}

console.log('agent fixture metadata and JSONL checks passed');

function readRepoFile(relativePath) {
  return readFileSync(path.join(repoRoot, relativePath), 'utf8');
}

function readPackageVersion(relativePath, packagePrefix, content = readRepoFile(relativePath)) {
  const marker = packagePrefix;
  const start = content.indexOf(marker);
  if (start === -1) {
    throw new Error(`${relativePath} does not contain ${packagePrefix}`);
  }
  return content
    .slice(start + marker.length)
    .split(/["\s]/)[0]
    .trim();
}

function derivePreviousPackageVersion(relativePath, packagePrefix, currentVersion) {
  const refs = ['origin/main', 'main', 'HEAD^', 'HEAD~2'];
  for (const ref of refs) {
    try {
      const content = execFileSync('git', ['show', `${ref}:${relativePath}`], {
        cwd: repoRoot,
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
      });
      const version = readPackageVersion(relativePath, packagePrefix, content);
      if (version && version !== currentVersion) {
        return version;
      }
    } catch {
      // Best-effort local derivation only; try the next available ref.
    }
  }
  throw new Error(
    `could not derive previous ${packagePrefix} version from local git history`
  );
}

function checkCodexProtocolVersions(codexVersion) {
  const expectedTag = `rust-v${codexVersion}`;
  const cargoToml = readRepoFile(sourcePaths.executorsCargoToml);
  assertDependencyTag(cargoToml, 'codex-protocol', expectedTag);
  assertDependencyTag(cargoToml, 'codex-app-server-protocol', expectedTag);

  const cargoLock = readRepoFile(sourcePaths.cargoLock);
  assertLockPackage(cargoLock, 'codex-protocol', codexVersion, expectedTag);
  assertLockPackage(
    cargoLock,
    'codex-app-server-protocol',
    codexVersion,
    expectedTag
  );
}

function assertDependencyTag(cargoToml, dependency, expectedTag) {
  const line = cargoToml
    .split(/\r?\n/)
    .find((candidate) => candidate.startsWith(`${dependency} = `));
  if (!line || !line.includes(`tag = "${expectedTag}"`)) {
    throw new Error(`${dependency} must use tag ${expectedTag}`);
  }
}

function assertLockPackage(lock, packageName, expectedVersion, expectedTag) {
  const block = lock
    .split('[[package]]')
    .find((candidate) => candidate.includes(`name = "${packageName}"`));
  if (!block) {
    throw new Error(`Cargo.lock is missing ${packageName}`);
  }
  if (!block.includes(`version = "${expectedVersion}"`)) {
    throw new Error(`${packageName} Cargo.lock version must be ${expectedVersion}`);
  }
  if (!block.includes(`tag=${expectedTag}`)) {
    throw new Error(`${packageName} Cargo.lock source must use ${expectedTag}`);
  }
}

function checkMetadata(fixture, versionRole, expectedVersion) {
  const metadataPath = path.join(
    repoRoot,
    'crates',
    'executors',
    'tests',
    'fixtures',
    'agent-cli',
    fixture.cli,
    versionRole,
    'metadata.json'
  );
  const metadata = JSON.parse(readFileSync(metadataPath, 'utf8'));
  const expected = {
    cli: fixture.cli,
    package: fixture.package,
    version_role: versionRole,
    version: expectedVersion,
    captured_from: 'committed_raw_fixture',
    raw_fixture: 'stdout.jsonl',
    compatibility_scope: 'normalization_no_migration',
  };

  for (const [key, value] of Object.entries(expected)) {
    if (metadata[key] !== value) {
      throw new Error(
        `${path.relative(repoRoot, metadataPath)} ${key} must be ${JSON.stringify(
          value
        )}, got ${JSON.stringify(metadata[key])}`
      );
    }
  }
}

function runNormalizeCheck() {
  const result = spawnSync(
    process.execPath,
    [path.join(repoRoot, 'scripts', 'agent-fixtures', 'normalize.mjs'), '--check'],
    {
      cwd: repoRoot,
      encoding: 'utf8',
      stdio: 'inherit',
    }
  );
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}
