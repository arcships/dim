import assert from 'node:assert/strict';
import {EventEmitter, once} from 'node:events';
import {rpc} from './native-rpc.mjs';
import fs from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import {createInterface} from 'node:readline';
import {setTimeout as delay} from 'node:timers/promises';

export async function draftDelivery(tui) {
  await tui.setup({respond: request => ({
    text: request.messages.some(m => m.role === 'user' && JSON.stringify(m.content).includes('edited after refusal'))
      ? 'Edited draft accepted.' : 'Receipt test answered.',
  })});
  const receiver = path.join(tui.root, 'receiver');
  const target = path.join(receiver, 'rust', 'daemon.sock');
  const socket = path.join(tui.root, 'receipt.sock');
  const workspace = path.join(tui.root, 'workspace');
  fs.writeFileSync(path.join(workspace, 'notes.txt'), 'notes');
  const serverArgs = ['--home', receiver, '--model-config', tui.modelFile];
  tui.runCommand([...serverArgs, 'mcp', 'list']);
  const env = {DIMCODE_TERMINAL_PROXY: socket, DIMCODE_TERMINAL_ID: 'fixture',
    DIMCODE_TERMINAL_NAME: 'Receipt fixture', DIMCODE_TERMINAL_ADDRESS: '127.0.0.1:1'};
  const command = [tui.binary, '--home', tui.home];
  const calls = [], sockets = new Set();
  const observed = new EventEmitter();
  let faults = [], refuseModel = false, connections = 0;
  const proxy = net.createServer(front => {
    connections++;
    const back = net.connect(target);
    const lost = new Map();
    for (const stream of [front, back]) {
      sockets.add(stream);
      stream.on('error', () => stream.destroy());
      stream.on('close', () => {
        sockets.delete(stream);
        front.destroy();
        back.destroy();
      });
    }
    const reply = (id, appError, message) => front.write(JSON.stringify({
      jsonrpc: '2.0', id, error: {code: -32000, message, data: {appError}},
    }) + '\n');
    createInterface({input: front}).on('line', line => {
      const request = JSON.parse(line);
      calls.push(request);
      if (request.method === 'session.input_lookup') observed.emit('lookup', request);
      if (request.method === 'session.model.get' && refuseModel) {
        refuseModel = false;
        return reply(request.id, 'service_reloading', 'model temporarily unavailable');
      }
      if (request.method === 'session.submit') {
        const fault = faults.shift();
        if (fault === 'refuse') return reply(request.id, 'input_blocked', 'fixture refusal');
        if (fault === 'before-edit') return reply(request.id, 'submission_unknown', 'edit receipt missing');
        if (fault === 'reject-edit') return reply(request.id, 'input_blocked', 'retry refused at target');
        if (fault === 'before') return reply(request.id, 'submission_unknown', 'receipt missing');
        if (fault) lost.set(request.id, fault);
      }
      back.write(line + '\n');
    });
    createInterface({input: back}).on('line', line => {
      const response = JSON.parse(line);
      if (response.result?.features) {
        response.result.features = response.result.features.filter(feature => feature !== 'ui.leaf.2');
        return front.write(JSON.stringify(response) + '\n');
      }
      if (lost.has(response.id) && response.result) {
        const fault = lost.get(response.id);
        lost.delete(response.id);
        if (fault === 'disconnect') return front.destroy();
        return reply(response.id, 'submission_unknown', 'receipt missing');
      }
      front.write(line + '\n');
    });
  });
  try {
    proxy.listen(socket);
    await once(proxy, 'listening');
    fs.chmodSync(socket, 0o600);
    for (const mode of ['scrollback', 'fullscreen', 'line']) {
      const args = mode === 'fullscreen' ? ['--fullscreen'] : mode === 'line' ? ['--no-tui'] : [];
      const owner = await rpc(target);
      const {result: session} = await owner.call('session.create', {cwd: workspace});
      owner.stream.destroy();
      const terminal = await tui.launch([...args, '--resume', session.sessionId, '--cwd', workspace], {command, env, cols: 132, rows: 32});
      await terminal.waitFor(mode === 'line' ? 'Enter: send' : 'Ask anything');
      if (mode === 'line') {
        const start = calls.length;
        const opened = connections;
        for (const key of '@note') {
          terminal.send(key);
          await delay(5);
        }
        await terminal.waitFor('* notes.txt');
        terminal.key('Down', 'Up', 'End');
        await delay(200);
        assert.deepEqual(calls.slice(start).filter(r => r.method === 'workspace.files.complete')
          .map(r => r.params.query), ['note']);
        assert.equal(connections - opened, 1, 'one connection for the completed query');
        terminal.key('C-c');
        const image = path.join(tui.root, 'pixel.png');
        fs.writeFileSync(image, Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC', 'base64'));
        terminal.paste(image);
        await terminal.waitFor('[Image 1:');
        refuseModel = true;
        terminal.key('Enter');
        await terminal.waitFor('model temporarily unavailable');
        terminal.key('C-c');
      }
      const start = calls.length;
      faults = mode === 'fullscreen' ? ['refuse', 'before'] : [mode === 'line' ? 'disconnect' : 'after'];
      terminal.send(mode + ' ');
      terminal.paste('x'.repeat(16 * 1024 + 1));
      await terminal.waitFor(mode === 'line' ? '[Paste 1:' : 'chars');
      terminal.key('Enter');
      if (mode === 'fullscreen') {
        await terminal.waitFor('fixture refusal');
        terminal.key('Enter');
      }
      await terminal.waitFor('Delivery unconfirmed');
      const checked = once(observed, 'lookup');
      terminal.key('Enter');
      await checked;
      await terminal.waitFor(mode === 'line' ? 'The original submission was accepted' : 'Receipt test answered.');
      if (mode !== 'line') await terminal.waitForAbsent('Enter on an empty draft to check and retry');
      const relevant = calls.slice(start);
      const attempts = relevant.filter(r => r.method === 'session.submit').map(r => r.params);
      const expected = attempts[mode === 'fullscreen' ? 1 : 0];
      const lookup = relevant.find(r => r.method === 'session.input_lookup');
      assert.equal(lookup?.params.submissionId, expected.submissionId);
      assert.equal(lookup.params.sessionId, expected.sessionId);
      assert.equal(lookup.params.branchId, expected.branchId);
      assert.equal(attempts.length, mode === 'fullscreen' ? 3 : 1);
      if (mode === 'fullscreen') assert.deepEqual(attempts[2], expected);
      assert.equal(relevant.filter(r => r.method === 'attachment.put').length, 1);
      assert.ok(expected.text.startsWith(mode + ' [Pasted text:'), expected.text);
      if (mode !== 'scrollback') {
        const mark = calls.length;
        faults = ['before-edit', 'reject-edit'];
        terminal.send('editable ');
        terminal.paste('y'.repeat(16 * 1024 + 1));
        await terminal.waitFor(mode === 'line' ? '[Paste 1:' : 'chars');
        terminal.key('Enter');
        await terminal.waitFor('edit receipt missing');
        terminal.key('Enter');
        await terminal.waitFor('retry refused at target');
        terminal.key('End');
        terminal.send(' edited after refusal');
        terminal.key('Enter');
        await terminal.waitFor('Edited draft accepted.');
        const history = calls.slice(mark);
        const deliveries = history.filter(r => r.method === 'session.submit' || r.method === 'session.input_lookup');
        assert.deepEqual(deliveries.map(r => r.method), ['session.submit', 'session.input_lookup', 'session.submit', 'session.submit']);
        const [first, check, retry, edited] = deliveries;
        assert.equal(check.params.submissionId, first.params.submissionId);
        assert.deepEqual(retry.params, first.params);
        assert.notEqual(edited.params.submissionId, first.params.submissionId);
        assert.equal(edited.params.text, first.params.text + ' edited after refusal');
        assert.equal(history.filter(r => r.method === 'attachment.put').length, 1);
      }
      terminal.send('/exit');
      terminal.key('Enter');
      assert.equal((await terminal.waitForExit()).status, 0);
    }
  } finally {
    for (const stream of sockets) stream.destroy();
    if (proxy.listening) await new Promise(resolve => proxy.close(resolve));
    tui.runCommand([...serverArgs, 'daemon', 'stop']);
  }
}
