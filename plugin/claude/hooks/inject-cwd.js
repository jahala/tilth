#!/usr/bin/env node
// PreToolUse hook: inject the live Claude Code session cwd into every
// `mcp__tilth__*` tool call. tilth's MCP server freezes its own process cwd at
// spawn, so it cannot see the agent's live working directory; this hook hands it
// the per-event cwd as the `cwd` parameter. A cwd the model set explicitly wins
// (the hook stays silent). Non-tilth tools and events with no cwd are no-ops.
'use strict';

function decide(event) {
  const tool = (event && event.tool_name) || '';
  if (!tool.startsWith('mcp__tilth__')) return null; // not our tool
  const input = (event && event.tool_input) || {};
  // Model-set cwd wins: only inject when the model left it unset/blank.
  if (input.cwd !== undefined && input.cwd !== null && input.cwd !== '') return null;
  const cwd = event && event.cwd;
  if (!cwd) return null; // nothing to inject
  return {
    hookSpecificOutput: {
      hookEventName: 'PreToolUse',
      updatedInput: Object.assign({}, input, { cwd }),
    },
  };
}

function main(raw) {
  let event;
  try {
    event = JSON.parse(raw);
  } catch {
    process.exit(0); // malformed event: stay silent
  }
  const out = decide(event);
  if (out) process.stdout.write(JSON.stringify(out));
  process.exit(0);
}

if (require.main === module) {
  let data = '';
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', (c) => {
    data += c;
  });
  process.stdin.on('end', () => main(data));
}

module.exports = { decide };
