import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EVENT_KIND_GROUPS } from '../src/lib/eventKinds.ts';

test('event kinds are unique and alphabetical within each scope', () => {
  const kinds = EVENT_KIND_GROUPS.flatMap(group => group.kinds);
  assert.equal(new Set(kinds).size, kinds.length);

  for (const group of EVENT_KIND_GROUPS) {
    assert.deepEqual(group.kinds, group.kinds.toSorted());
  }
});
