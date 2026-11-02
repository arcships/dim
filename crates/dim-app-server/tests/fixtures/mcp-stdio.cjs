// Opt-in interoperability fixture: uses the workspace's official MCP SDK.
const { createRequire } = require('node:module');
const { resolve } = require('node:path');
const { writeFileSync } = require('node:fs');
const sdk = createRequire(resolve(__dirname, '../../../../runtime/package.json'));
const { McpServer } = sdk('@modelcontextprotocol/sdk/server/mcp.js');
const { StdioServerTransport } = sdk('@modelcontextprotocol/sdk/server/stdio.js');
const { z } = sdk('zod');

const WIDGET_URI = 'ui://widget/counter.html';

async function main() {
  const mode = process.argv[2];
  writeFileSync(process.env.MCP_PID_FILE, String(process.pid));
  const keepAlive = setInterval(() => {}, 1000);
  if (mode === 'silent') {
    process.stdin.resume();
    return;
  }
  if (mode === 'invalid') {
    process.stdout.write('not json\n');
    process.stdin.resume();
    return;
  }
  if (mode !== 'stubborn') {
    process.stdin.on('end', () => clearInterval(keepAlive));
  }
  const server = new McpServer({ name: 'stdio-interop', version: '1' });
  server.registerTool('inspect', { inputSchema: { a: z.number(), b: z.number() } }, async ({ a, b }) => ({
    content: [{ type: 'text', text: JSON.stringify({ value: a * 100 + b, cwd: process.cwd(), argument: process.argv[3], env: process.env.MCP_TEST_VALUE }) }],
  }));
  server.registerTool('slow', { inputSchema: {} }, async (_, { signal }) => {
    writeFileSync(process.env.MCP_PID_FILE + '.called', '1');
    await new Promise(resolve => signal.addEventListener('abort', resolve, { once: true }));
    writeFileSync(process.env.MCP_PID_FILE + '.cancelled', '1');
    return { content: [{ type: 'text', text: 'cancelled' }] };
  });
  // MCP Apps: a widget tool, an app-only tool and a model-only tool, declared
  // with the specification's `_meta.ui`, and the `ui://` document it mounts.
  if (process.env.MCP_WIDGETS === '1') {
    const ui = { csp: { connectDomains: ['https://api.example'] }, prefersBorder: true };
    server.registerTool('chart', {
      description: 'Render a counter',
      inputSchema: { step: z.number() },
      _meta: { ui: { resourceUri: WIDGET_URI } },
    }, async ({ step }) => ({
      content: [{ type: 'text', text: `chart ${step}` }],
      structuredContent: { step },
      _meta: { widget: 'chart' },
    }));
    server.registerTool('refresh', {
      description: 'Refresh the view',
      inputSchema: {},
      _meta: { ui: { visibility: ['app'] } },
    }, async () => ({ content: [{ type: 'text', text: 'refreshed' }] }));
    server.registerTool('summary', {
      description: 'Summarize',
      inputSchema: {},
      _meta: { ui: { visibility: ['model'] } },
    }, async () => ({ content: [{ type: 'text', text: 'summary' }] }));
    server.registerResource('widget', WIDGET_URI, {
      mimeType: 'text/html;profile=mcp-app',
      _meta: { ui },
    }, async uri => ({
      contents: [{
        uri: uri.href,
        mimeType: 'text/html;profile=mcp-app',
        text: '<p>counter</p>',
        _meta: { ui },
      }],
    }));
  }
  await server.connect(new StdioServerTransport());
}
main().catch(() => process.exit(1));
