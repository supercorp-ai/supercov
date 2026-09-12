// Fixture author simulation; product commands never write acknowledgement tokens.
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';
export function acknowledgeMap(query, mapFile, map = JSON.parse(readFileSync(mapFile, 'utf8'))) {
  const write = () => writeFileSync(mapFile, JSON.stringify(map, null, 2) + '\n');
  const changes = query('report', '--view', 'changes', '--limit', '1000').items;
  if (changes.length) {
    const keys = new Set(map.assertions.flatMap(a => a.flows.map(f => `${a.id}/${f.id}`)));
    map.changeAssessments = changes.map(c => ({id:c.id,basis:null,affectedFlows:c.knownFlows.filter(k => keys.has(k)),explanation:'Fixture author inspected this source edit and the affected claims.'}));
    write();
    const validation = query('validate');
    for (const r of map.changeAssessments) r.basis = validation.changes.find(c => c.id === r.id).expectedBasis;
    write();
  }
  const validation = query('validate');
  for (const a of map.assertions) for (const f of a.flows) {
    f.basis = validation.flows.find(r => r.id === `${a.id}/${f.id}`).expectedBasis;
    assert.match(f.basis, /^scov2:[a-f0-9]{64}$/);
  }
  write();
  return map;
}
