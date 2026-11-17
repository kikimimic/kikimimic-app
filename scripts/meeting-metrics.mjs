#!/usr/bin/env node
// Offline adapter input, not native events.jsonl. See docs/evaluation/meeting-quality.md.
import {readFileSync} from 'node:fs';
import assert from 'node:assert/strict';
export function summarize(rows) {
  const ids = new Set();
  const metrics = {first_partial_ms: [], finalization_ms: [], translation_ms: [], peak_rss_mib: []};
  const outcomes = {success: 0, failed: 0, dropped: 0, unmeasured: 0};
  for (const r of rows) {
    if (!r.id || ids.has(r.id)) throw Error('missing or duplicate id');
    ids.add(r.id);
    if (!Object.hasOwn(outcomes, r.outcome)) throw Error('invalid outcome');
    outcomes[r.outcome]++;
    for (const [key, values] of Object.entries(metrics)) {
      if (r[key] == null) continue;
      if (!Number.isFinite(r[key]) || r[key] < 0) throw Error(`invalid ${key}`);
      values.push(r[key]);
    }
    if (r.outcome !== 'success' && r.translation_ms != null) throw Error('unsuccessful translation has latency');
  }
  return {utterances: rows.length, outcomes, metrics: Object.fromEntries(Object.entries(metrics).map(([key, values]) => {
    values.sort((a,b) => a-b);
    const percentile = p => values.length ? values[Math.ceil(values.length*p)-1] : null;
    return [key, {n: values.length, missing: rows.length-values.length, mean: values.length ? values.reduce((a,b)=>a+b,0)/values.length : null, p50: percentile(.5), p95: percentile(.95), max: values.at(-1) ?? null}];
  }))};
}
if (process.argv[2] === '--self-test') {
  const report = summarize([{id:'a',outcome:'success',translation_ms:10},{id:'b',outcome:'dropped'},{id:'c',outcome:'success',translation_ms:30}]);
  assert.equal(report.metrics.translation_ms.p50,10);
  assert.equal(report.metrics.translation_ms.p95,30);
  assert.equal(report.metrics.translation_ms.missing,1);
  assert.equal(summarize([]).metrics.translation_ms.p95,null);
  assert.throws(()=>summarize([{id:'a',outcome:'success',finalization_ms:-1}]));
  assert.throws(()=>summarize([{id:'a',outcome:'dropped',translation_ms:0}]));
  assert.throws(()=>summarize([{id:'a',outcome:'success'},{id:'a',outcome:'success'}]));
  console.log('7 assertions passed (synthetic data only)');
} else {
  try {
    const rows = readFileSync(process.argv[2], 'utf8').split('\n').filter(x=>x.trim()).map(JSON.parse);
    console.log(JSON.stringify(summarize(rows),null,2));
  } catch(error) { console.error(error.message); process.exitCode=1; }
}
