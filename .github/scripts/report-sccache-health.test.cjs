const assert = require('node:assert/strict');
const test = require('node:test');

const { summarizeSccacheStats } = require('./report-sccache-health.cjs');

test('summarizes cache hits across compiler languages', () => {
  const summary = summarizeSccacheStats(
    {
      stats: {
        cache_hits: { counts: { Rust: 30, 'C/C++': 10 } },
        cache_misses: { counts: { Rust: 10 } },
        cache_writes: 9,
        cache_write_errors: 0,
        cache_read_errors: 0,
      },
      cache_location: 'ghac, name: test',
    },
    'x86_64-unknown-linux-musl'
  );

  assert.equal(summary.hits, 40);
  assert.equal(summary.misses, 10);
  assert.equal(summary.hitRate, 80);
  assert.equal(summary.warning, false);
  assert.match(summary.markdown, /\| Hit rate \| 80\.00% \|/);
  assert.match(summary.markdown, /ghac, name: test/);
});

test('warns when the cache backend reports errors', () => {
  const summary = summarizeSccacheStats(
    {
      stats: {
        cache_hits: { counts: {} },
        cache_misses: { counts: { Rust: 12 } },
        cache_writes: 0,
        cache_write_errors: 12,
        cache_read_errors: 2,
      },
    },
    'aarch64-apple-darwin'
  );

  assert.equal(summary.hitRate, 0);
  assert.equal(summary.warning, true);
  assert.match(summary.markdown, /\| Cache write errors \| 12 \|/);
  assert.match(summary.markdown, /\| Cache read errors \| 2 \|/);
});
