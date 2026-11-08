// The smallest stdio MCP server: the handshake and one tool. A test starts it
// through `fixturetool`, a shell script only a runtime's bin directory holds.
const readline = require('node:readline');

const rl = readline.createInterface({ input: process.stdin });
const send = (message) => process.stdout.write(JSON.stringify(message) + '\n');

rl.on('line', (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return;
  }
  if (message.id === undefined) {
    return;
  }
  if (message.method === 'initialize') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        protocolVersion: '2025-03-26',
        capabilities: { tools: {} },
        serverInfo: { name: 'fixture', version: '1' },
      },
    });
  } else if (message.method === 'tools/list') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        tools: [
          {
            name: 'fixture_tool',
            description: 'A tool only the toolchain directory provides.',
            inputSchema: { type: 'object', properties: {} },
          },
        ],
      },
    });
  } else if (message.method === 'ping') {
    send({ jsonrpc: '2.0', id: message.id, result: {} });
  } else {
    send({
      jsonrpc: '2.0',
      id: message.id,
      error: { code: -32601, message: 'unknown method' },
    });
  }
});
