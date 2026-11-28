import {once} from 'node:events';
import {connect} from 'node:net';
import {createInterface} from 'node:readline';

export async function rpc(socket) {
  const stream = connect(socket);
  await once(stream, 'connect');
  const lines = createInterface({ input: stream, crlfDelay: Infinity })[Symbol.asyncIterator]();
  let id = 0;
  async function receive(matches) {
    for (;;) {
      const { value, done } = await lines.next();
      if (done) throw Error('App Server stream ended');
      const reply = JSON.parse(value);
      if (matches(reply)) return reply;
    }
  }
  function call(method, params) {
    const request = ++id;
    stream.write(JSON.stringify({ jsonrpc: '2.0', id: request, method, params }) + '\n');
    return receive(reply => reply.id === request);
  }
  const { result: hello } = await call('hello', { protocolVersion: 4, clientKind: 'cli', clientName: 'sharing-test' });
  return { stream, call, hello, receive };
}
