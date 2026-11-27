import assert from 'node:assert/strict';
import test from 'node:test';
import { isAccepted, parseRefs, stripNonLinks } from './issue-gate.mjs';

test('parses closing and reference keywords', () => {
  assert.deepEqual(parseRefs('Fixes #1'), [1]);
  assert.deepEqual(parseRefs('Closes #2'), [2]);
  assert.deepEqual(parseRefs('Resolves #3'), [3]);
  assert.deepEqual(parseRefs('Refs #4'), [4]);
  assert.deepEqual(parseRefs('fixes: #5'), [5]);
});

test('collects multiple references on one keyword line', () => {
  assert.deepEqual(parseRefs('Fixes #1257 and #2272'), [1257, 2272]);
  assert.deepEqual(parseRefs('Fixes #1, #2, #2'), [1, 2]);
});

test('ignores text that is not a link', () => {
  assert.deepEqual(parseRefs('prefixes #1'), []);
  assert.deepEqual(parseRefs('see #1'), []);
  assert.deepEqual(parseRefs('Fixes #1\nRefs #2'), [1, 2]);
  assert.deepEqual(parseRefs('Fixes owner/repo#1'), []);
  assert.deepEqual(parseRefs('Fixes #1 and owner/repo#2'), [1]);
});

test('ignores fenced code, inline code, comments and quotes', () => {
  assert.deepEqual(parseRefs('```\nFixes #1\n```'), []);
  assert.deepEqual(parseRefs('~~~\nFixes #1\n~~~'), []);
  assert.deepEqual(parseRefs('`Fixes #1`'), []);
  assert.deepEqual(parseRefs('<!-- Fixes #1 -->'), []);
  assert.deepEqual(parseRefs('<!--\nFixes #1\n-->'), []);
  assert.deepEqual(parseRefs('> Fixes #1'), []);
});

test('keep a real link after stripping non-link contexts', () => {
  const body = '```\nFixes #1\n```\n\nRefs #2272';
  assert.deepEqual(parseRefs(body), [2272]);
  assert.ok(stripNonLinks(body).includes('Refs #2272'));
});

test('accepted means P0, P1 or P2', () => {
  assert.equal(isAccepted(['P0']), true);
  assert.equal(isAccepted(['P1', 'design']), true);
  assert.equal(isAccepted(['P2', 'P3']), true);
  assert.equal(isAccepted(['P3']), false);
  assert.equal(isAccepted(['design']), false);
  assert.equal(isAccepted([]), false);
});
