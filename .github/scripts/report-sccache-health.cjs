#!/usr/bin/env node

const fs = require('fs');

function sumCounts(counts) {
  return Object.values(counts ?? {}).reduce(
    (total, value) => total + Number(value ?? 0),
    0
  );
}

function summarizeSccacheStats(stats, target) {
  const values = stats.stats ?? {};
  const hits = sumCounts(values.cache_hits?.counts);
  const misses = sumCounts(values.cache_misses?.counts);
  const writes = Number(values.cache_writes ?? 0);
  const writeErrors = Number(values.cache_write_errors ?? 0);
  const readErrors = Number(values.cache_read_errors ?? 0);
  const total = hits + misses;
  const hitRate = total === 0 ? 0 : (hits / total) * 100;

  return {
    hits,
    misses,
    writes,
    writeErrors,
    readErrors,
    hitRate,
    warning: writeErrors > 0 || readErrors > 0,
    markdown: [
      `### sccache health: \`${target}\``,
      '',
      '| Metric | Value |',
      '| --- | ---: |',
      `| Cache hits | ${hits} |`,
      `| Cache misses | ${misses} |`,
      `| Hit rate | ${hitRate.toFixed(2)}% |`,
      `| Cache writes | ${writes} |`,
      `| Cache write errors | ${writeErrors} |`,
      `| Cache read errors | ${readErrors} |`,
      `| Cache location | ${stats.cache_location ?? 'unknown'} |`,
      '',
    ].join('\n'),
  };
}

function main(argv) {
  const [statsPath, target, summaryPath] = argv;
  if (!statsPath || !target || !summaryPath) {
    throw new Error(
      'usage: report-sccache-health.cjs <stats-json> <target> <summary-file>'
    );
  }

  const stats = JSON.parse(fs.readFileSync(statsPath, 'utf8'));
  const summary = summarizeSccacheStats(stats, target);
  fs.appendFileSync(summaryPath, summary.markdown);

  if (summary.warning) {
    console.log(
      `::warning title=sccache backend errors::${target}: ` +
        `${summary.writeErrors} write errors and ${summary.readErrors} read errors`
    );
  }
}

if (require.main === module) {
  main(process.argv.slice(2));
}

module.exports = { summarizeSccacheStats };
