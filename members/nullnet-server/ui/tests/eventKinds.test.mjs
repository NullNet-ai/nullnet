import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import ts from 'typescript';

const source = await readFile(new URL('../src/lib/eventKinds.ts', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } });
const { EVENT_KIND_GROUPS } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);

test('event kinds are unique and alphabetical within each scope', () => {
  const kinds = EVENT_KIND_GROUPS.flatMap(group => group.kinds);
  assert.equal(new Set(kinds).size, kinds.length);

  for (const group of EVENT_KIND_GROUPS) {
    assert.deepEqual(group.kinds, group.kinds.toSorted());
  }
});
