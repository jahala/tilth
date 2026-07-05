// Unit tests for the cwd-injection PreToolUse hook. Run with:
//   node --test plugin/claude/hooks/*.test.js
// Exercises the pure `decide` core AND the real stdin→stdout process path so a
// regression in the wiring (not just the logic) is caught.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert');
const { spawnSync } = require('node:child_process');
const path = require('node:path');

const HOOK = path.join(__dirname, 'inject-cwd.js');
const { decide } = require('./inject-cwd.js');

// Drive the hook as the harness does: feed the event JSON on stdin, read stdout.
function run(event) {
  const res = spawnSync('node', [HOOK], {
    input: JSON.stringify(event),
    encoding: 'utf8',
  });
  assert.strictEqual(res.status, 0, `hook exited ${res.status}: ${res.stderr}`);
  return res.stdout;
}

test('injects the session cwd when the model did not set one', () => {
  const out = run({
    tool_name: 'mcp__tilth__tilth_search',
    tool_input: { queries: [{ query: 'x' }] },
    cwd: '/repo/checkout',
  });
  const parsed = JSON.parse(out);
  assert.strictEqual(parsed.hookSpecificOutput.hookEventName, 'PreToolUse');
  assert.strictEqual(parsed.hookSpecificOutput.updatedInput.cwd, '/repo/checkout');
  // Original input is preserved alongside the injected cwd.
  assert.deepStrictEqual(parsed.hookSpecificOutput.updatedInput.queries, [
    { query: 'x' },
  ]);
});

test('defers to a model-set cwd (emits nothing)', () => {
  const out = run({
    tool_name: 'mcp__tilth__tilth_read',
    tool_input: { paths: ['a.rs'], cwd: '/model/set' },
    cwd: '/session/cwd',
  });
  assert.strictEqual(out, '', 'must stay silent when the model already set cwd');
});

test('ignores non-tilth tools', () => {
  const out = run({
    tool_name: 'Bash',
    tool_input: { command: 'ls' },
    cwd: '/session/cwd',
  });
  assert.strictEqual(out, '', 'must stay silent for non-tilth tools');
});

test('stays silent when no session cwd is available', () => {
  const out = run({
    tool_name: 'mcp__tilth__tilth_list',
    tool_input: { patterns: ['*.rs'] },
  });
  assert.strictEqual(out, '', 'no session cwd → nothing to inject');
});

test('decide: a blank model cwd is treated as unset and injected', () => {
  const out = decide({
    tool_name: 'mcp__tilth__tilth_write',
    tool_input: { edits: '...', cwd: '' },
    cwd: '/live/cwd',
  });
  assert.strictEqual(out.hookSpecificOutput.updatedInput.cwd, '/live/cwd');
});
