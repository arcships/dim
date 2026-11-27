import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {test} from 'node:test';
import {plan} from './ci-test-concurrency.mjs';

test('the plan scales by physical cores', () => {
  assert.deepEqual(plan('linux', 32, 16, 32), {jobs: 5, threads: 4}, 'sixteen physical cores');
  assert.deepEqual(plan('linux', 8, 8, 8), {jobs: 3, threads: 2}, 'no SMT siblings');
  assert.deepEqual(plan('linux', 64, 32, 64), {jobs: 8, threads: 6}, 'the upper clamps');
});

test('the plan falls back to the CPU count without a topology', () => {
  assert.deepEqual(plan('linux', 16, 0, 0), {jobs: 5, threads: 4});
  assert.deepEqual(plan('linux', 4, 0, 0), {jobs: 3, threads: 2});
});

test('the plan holds the macOS settings on other platforms', () => {
  assert.deepEqual(plan('darwin', 32, 16, 32), {jobs: 3, threads: 3});
});

test('the command writes the outputs the job hands to the test runner', () => {
  const run = spawnSync(process.execPath, ['scripts/ci-test-concurrency.mjs'], {encoding: 'utf8'});
  assert.equal(run.status, 0, run.stderr);
  const lines = run.stdout.trim().split('\n');
  assert.deepEqual(
    lines.map((line) => line.split('=')[0]),
    ['jobs', 'threads'],
    'the step writes exactly the two outputs it hands on',
  );
  const [, jobs] = lines[0].match(/^jobs=(\d+)$/) ?? [];
  const [, threads] = lines[1].match(/^threads=(\d+)$/) ?? [];
  assert.ok(Number(jobs) >= 3 && Number(jobs) <= 8, lines[0]);
  assert.ok(Number(threads) >= 2 && Number(threads) <= 6, lines[1]);
});
