import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import ts from 'typescript';

const source = await readFile(new URL('../src/lib/pressureChart.ts', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } });
const module = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);
const { recentPressureWindows, timedLinePoints } = module;

const windowAt = (age_ms, arrivals_per_sec = 1, duration_ms = 1000) => ({ age_ms, duration_ms, arrivals_per_sec });

test('the chart retains zero samples but never data older than 60 seconds', () => {
  const recentZero = windowAt(10_000, 0);
  const boundary = windowAt(60_000, 2);
  const old = windowAt(60_001, 9);

  assert.deepEqual(recentPressureWindows([recentZero, old, boundary]), [boundary, recentZero]);
});

test('points use actual intervals and draw idle gaps as zero', () => {
  const windows = [windowAt(40_000, 2, 20_000), windowAt(10_000, 5, 10_000)];

  assert.equal(
    timedLinePoints(windows, window => window.arrivals_per_sec, 300, 100, 10),
    '0,100 100,80 200,100 250,50 300,100',
  );
});

test('a fully idle range is an explicit zero line', () => {
  assert.equal(timedLinePoints([], () => 0, 300, 100, 1), '0,100 300,100');
});
