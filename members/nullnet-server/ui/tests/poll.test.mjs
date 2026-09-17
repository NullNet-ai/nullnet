import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import ts from 'typescript';

const source = await readFile(new URL('../src/lib/poll.ts', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } });
const { poll } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);
const flush = async () => { await Promise.resolve(); await Promise.resolve(); };

test('slow requests complete before another poll starts', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const pending = [];
  let completed = 0;
  const { stop } = poll(async () => {
    await new Promise(resolve => pending.push(resolve));
    completed++;
  }, 5000);
  t.after(stop);
  for (let i = 0; i < 4; i++) {
    t.mock.timers.tick(6000);
    assert.equal(pending.length, i + 1);
    pending[i]();
    await flush();
    assert.equal(completed, i + 1);
    t.mock.timers.tick(4999);
    assert.equal(pending.length, i + 1);
    t.mock.timers.tick(1);
  }
});

test('stopping cancels an in-flight request and prevents another poll', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  let signal, finish, calls = 0;
  const { stop } = poll(async requestSignal => {
    signal = requestSignal;
    calls++;
    await new Promise(resolve => { finish = resolve; });
  }, 5000);
  stop();
  assert.equal(signal.aborted, true);
  finish();
  await flush();
  t.mock.timers.tick(20000);
  assert.equal(calls, 1);
});

test('stopping cancels a scheduled poll', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  let calls = 0;
  const { stop } = poll(async () => { calls++; }, 5000);
  await flush();
  stop();
  t.mock.timers.tick(20000);
  assert.equal(calls, 1);
});

test('without an interval the request runs once', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  let calls = 0;
  const { stop } = poll(async () => { calls++; });
  t.after(stop);
  await flush();
  t.mock.timers.tick(20000);
  assert.equal(calls, 1);
});

test('manual refresh cancels the old request and remains awaitable', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const requests = [];
  const polling = poll(signal => new Promise(resolve => requests.push({ signal, resolve })), 5000);
  t.after(polling.stop);
  let refreshed = false;
  const refresh = polling.refresh().then(() => { refreshed = true; });
  assert.equal(requests[0].signal.aborted, true);
  requests[0].resolve();
  await flush();
  t.mock.timers.tick(20000);
  assert.equal(requests.length, 2);
  assert.equal(refreshed, false);
  requests[1].resolve();
  await refresh;
  assert.equal(refreshed, true);
  t.mock.timers.tick(5000);
  assert.equal(requests.length, 3);
});
