import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import {rpc} from './native-rpc.mjs';
import {draftDelivery} from './draft-delivery.mjs';

const ANSWER = 'The fixture answer is deterministic.';

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function quote(value) {
  return `'${String(value).replace(/'/g, "'\\''")}'`;
}

function lastMessage(request) {
  return (request.messages ?? []).at(-1) ?? {};
}

function userText(request) {
  const last = lastMessage(request);
  return typeof last.content === 'string' ? last.content : JSON.stringify(last.content ?? '');
}

/// Requests the fixture answered for a real turn: a prompt-suggestion or title
/// request carries no tool declarations, a turn request does.
function turnRequests(model) {
  return model.requests.filter((entry) => (entry.body.tools ?? []).length > 0);
}

function lastTurnText(model) {
  const turn = turnRequests(model).at(-1);
  assert(turn, 'the fixture model answered a turn request');
  return userText(turn.body);
}

async function waitForRequest(model, predicate, {timeout = 20_000, describe = 'a fixture request'} = {}) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const found = model.requests.find((entry) => predicate(entry.body));
    if (found) return found;
    if (Date.now() >= deadline) {
      throw new Error(
        `waited ${timeout}ms for ${describe}; ${model.requests.length} request(s): ${model.requests
          .map((entry) => JSON.stringify(userText(entry.body).slice(0, 40)))
          .join(', ')}`,
      );
    }
    await sleep(100);
  }
}

/// A rejected gesture has no event of its own, so the evidence is the window
/// in which nothing reached the fixture.
async function settleWithout(model, predicate, {timeout = 1500, after = 0} = {}) {
  const matching = () => model.requests.filter((entry) => predicate(entry.body)).length;
  const deadline = Date.now() + timeout;
  for (;;) {
    if (matching() > after) return false;
    if (Date.now() >= deadline) return true;
    await sleep(100);
  }
}

async function exitWith(terminal, expected = 0) {
  terminal.send('/exit');
  terminal.key('Enter');
  await terminal.waitFor('Bye!');
  const status = await terminal.waitForExit();
  assert.equal(status.status, expected, `the CLI exits with ${expected}`);
  return status;
}

/// The exit path's own output: everything the process wrote after its goodbye
/// panel, so a cursor or screen assertion cannot be satisfied by an earlier
/// frame or an earlier mode switch.
function exitTail(raw) {
  const at = raw.lastIndexOf('Bye!');
  return at === -1 ? '' : raw.slice(at);
}

/// The final hand-back. The restore sequence starts with the status record's
/// clear, so everything after it is the process leaving the terminal, whatever
/// ended the interface.
function restoreTail(raw) {
  const marker = '\u001b]7501;state=clear';
  const at = raw.lastIndexOf(marker);
  return at === -1 ? '' : raw.slice(at);
}

/// The reports written after the newest `state=working`, so a settled wait
/// cannot be satisfied by a report from before the run started.
function afterLastWorking(raw) {
  const marker = 'state=working';
  const at = raw.lastIndexOf(marker);
  return at === -1 ? '' : raw.slice(at + marker.length);
}

/// The notice a TUI of one build shows when the daemon it reached is another
/// build of the same version.
const OTHER_BUILD_NOTICE = 'still using a different build of';

/// Runs as the older build, so the daemon it starts is stamped as that build
/// and reports one Session for the resumes that follow.
function startDaemonAsOlderBuild(tui) {
  return tui
    .runCommandAsync(
      ['--home', tui.home, '--model-config', tui.modelFile, 'exec', '--json', '-p', 'seed'],
      {env: {DIM_TEST_BUILD_ID: 'build-2026-11-16'}},
    )
    .then((output) => {
      const session = output
        .split('\n')
        .map((line) => line.trim())
        .filter(Boolean)
        .map((line) => JSON.parse(line).params?.sessionId)
        .find(Boolean);
      assert(session, `exec named the Session it created: ${output}`);
      return session;
    });
}

/// One TUI mode's view of a kept daemon of another build: the notice and the
/// command that stops it, once on Home and once on a direct resume, and
/// nothing when the stamps match. Returns the matching terminal for the caller
/// to close or extend.
async function otherBuildNotice(tui, {fullscreen = false} = {}) {
  const mode = fullscreen ? ['--fullscreen'] : [];
  // The renderer breaks a long row at its width, inside a word when it must, so
  // the command is compared without the whitespace the break leaves behind.
  const squeeze = (text) => text.replace(/\s+/g, '');
  const command = squeeze(`${tui.binary} --home ${tui.home} daemon stop`);
  const nextStep = (screen) => squeeze(screen).includes(command);
  await tui.setup({respond: () => ({text: ANSWER})});
  const session = await startDaemonAsOlderBuild(tui);

  const home = await tui.launch(mode, {waitForComposer: true, env: {DIM_TEST_BUILD_ID: 'build-2026-11-18'}});
  const screen = await home.waitFor(OTHER_BUILD_NOTICE);
  assert(nextStep(screen), `the notice names the command that switches builds: ${screen}`);
  await exitWith(home);

  const resumed = await tui.launch([...mode, '--resume', session], {
    waitForComposer: true,
    env: {DIM_TEST_BUILD_ID: 'build-2026-11-18'},
  });
  await resumed.waitFor('seed', {history: true, describe: 'the resumed transcript'});
  const resumedScreen = await resumed.waitFor(OTHER_BUILD_NOTICE);
  assert(nextStep(resumedScreen), `a direct resume names the same next step: ${resumedScreen}`);
  await exitWith(resumed);

  const matched = await tui.launch([...mode, '--resume', session], {
    waitForComposer: true,
    env: {DIM_TEST_BUILD_ID: 'build-2026-11-16'},
  });
  await matched.waitFor('seed', {history: true, describe: 'the resumed transcript'});
  assert(
    !matched.capture({history: true}).includes(OTHER_BUILD_NOTICE),
    'a daemon of this same build is reused without a notice',
  );
  return matched;
}

/// A daemon whose build left no stamp cannot be verified by a client that has
/// one: the client says that before any command can surface a missing method.
async function unverifiedBuildNotice(tui) {
  const squeeze = (text) => text.replace(/\s+/g, '');
  tui.stopDaemon();
  await tui.runCommandAsync(
    ['--home', tui.home, '--model-config', tui.modelFile, 'exec', '--json', '-p', 'seed'],
    {env: {DIM_TEST_BUILD_ID: ''}},
  );
  const terminal = await tui.launch([], {
    waitForComposer: true,
    env: {DIM_TEST_BUILD_ID: 'build-2026-11-18'},
  });
  const screen = await terminal.waitFor('could not be verified as this client');
  assert(
    squeeze(screen).includes(squeeze(`${tui.binary} --home ${tui.home} daemon stop`)),
    `the unverified notice names the command that switches builds: ${screen}`,
  );
  await exitWith(terminal);
}

async function checkDeviceDraftDestination(terminal, devicesOverDraft) {
  const switchBack = async () => {
    await devicesOverDraft();
    terminal.key('Home', 'Down', 'Enter');
    await terminal.waitFor('OfficeFixture · Active sessions');
    terminal.key('Home', 'Enter');
    await terminal.waitFor('OfficeFixture:');
    await devicesOverDraft();
    terminal.key('Home', 'Enter');
    await terminal.waitForAbsent('OfficeFixture:');
  };
  terminal.send('request fixture approval');
  terminal.key('Enter');
  await terminal.waitFor('Permission required');
  await switchBack();
  await terminal.waitFor('Permission required');
  await terminal.waitFor('echo decision-fixture');
  terminal.key('y');
  await terminal.waitFor('Ran 1 command');

  terminal.send('/branch Side task');
  terminal.key('Enter');
  await terminal.waitFor('Side task');
  terminal.send('/lines');
  terminal.key('Enter');
  await terminal.waitFor('Enter show');
  await terminal.waitFor('Side task');
  terminal.key('Home', 'Down', 'Enter');
  await terminal.waitFor(/⑂.*Side task/);
  terminal.send('Draft for the side task');
  await switchBack();
  await terminal.waitFor(/⑂.*Side task/);
  await terminal.waitFor('Draft for the side task');
  terminal.key('Enter');
  await terminal.waitFor('Side draft delivered');

  terminal.send('Typed before history');
  terminal.key('Home', 'Up');
  await terminal.waitFor('Draft for the side task');
  await switchBack();
  terminal.key('Down');
  await terminal.waitFor('Typed before history');
  terminal.key('C-c');
  await terminal.waitForAbsent('Typed before history');

  terminal.send('/timeline');
  terminal.key('Enter');
  await terminal.waitFor('Timeline');
  terminal.key('b');
  await terminal.waitFor(/new line from .*Side task/);
  terminal.key('C-u');
  terminal.send('Nested task draft');
  await switchBack();
  await terminal.waitFor(/new line from .*Side task/);
  await terminal.waitFor('Nested task draft');
  terminal.key('Enter');
  await terminal.waitForAbsent('new line from');
  terminal.send('/lines');
  terminal.key('Enter');
  await terminal.waitFor('Enter show');
  await terminal.waitFor(/Nested task draft.*[0-9a-f]{8}/);
  terminal.key('Escape');
  await terminal.waitForAbsent('Enter show');
}

const scenarios = [
  {
    name: 'earlier_reveals_resumed_history_in_both_modes',
    area: 'terminal-modes',
    title: 'Redraw native scrollback oldest first and reveal the preceding page at the fullscreen join',
    async run(tui) {
      const thought = 'Check the first turn.';
      await tui.setup({respond: (request) => userText(request).includes('history-turn-01')
        ? {thinking: thought, text: ANSWER} : {text: ANSWER}});
      let session;
      for (let turn = 1; turn <= 16; turn++) {
        const output = await tui.runCommandAsync([
          '--home', tui.home, '--model-config', tui.modelFile, 'exec', ...(session ? ['resume', session] : []),
          '--json', '-p', `history-turn-${String(turn).padStart(2, '0')}`,
        ]);
        session ??= output.trim().split('\n').map((line) => JSON.parse(line).params?.sessionId).find(Boolean);
        assert(session, 'all turns belong to one Session');
      }
      const sentinel = 'pre-Dim shell output';
      const warning = 'Clears terminal output from before Dim started';
      for (const fullscreen of [true, false]) {
        const terminal = await tui.launch(['--resume', session, ...(fullscreen ? ['--fullscreen'] : [])], {
          rows: 40,
          command: fullscreen ? undefined : ['/bin/sh', '-c', 'printf "%s" "$0"; exec "$@"',
            sentinel + '\n'.repeat(41), tui.binary, '--home', tui.home, '--model-config', tui.modelFile],
        });
        const initial = await terminal.waitFor(/resumed .* messages/, {history: true});
        assert(initial.includes('history-turn-16') && !initial.includes('history-turn-10'), initial);
        const hint = 'Earlier messages · /earlier';
        assert(initial.includes(hint) && initial.indexOf(hint) < initial.search(/┃ history-turn-/), initial);
        assert.equal(initial.includes(warning), !fullscreen, initial);
        if (!fullscreen) assert(initial.includes(sentinel), initial);
        assert(!/earlier|\/timeline/i.test(initial.slice(initial.indexOf('resumed '))), initial);
        if (fullscreen) {
          terminal.send('/earlier');
        } else {
          terminal.send('draft stays here');
          terminal.key('C-o');
          await terminal.waitFor('Suggestions');
          terminal.send('earlier');
        }
        terminal.key('Enter');
        if (!fullscreen) {
          const turns = (text) => [...text.matchAll(/┃ history-turn-(\d+)/g)].map((m) => Number(m[1]));
          let oldest = turns(initial)[0];
          while (oldest > 1) {
            const last = String(oldest - 1).padStart(2, '0');
            const page = await terminal.waitFor(new RegExp(`┃ history-turn-${last}[\\s\\S]*┃ history-turn-16[\\s\\S]*draft stays here`), {history: true});
            const first = turns(page)[0];
            assert(!page.includes(sentinel), page);
            assert(first >= 1 && first < oldest, page);
            assert.deepEqual(page.match(/┃ history-turn-\d+|The fixture answer is deterministic\./g),
              Array.from({length: 17 - first}, (_, i) => [`┃ history-turn-${String(first + i).padStart(2, '0')}`, ANSWER]).flat());
            assert.equal(page.includes(hint), first > 1);
            oldest = first;
            terminal.key('C-o');
            await terminal.waitFor('Suggestions');
            terminal.send('earlier');
            terminal.key('Enter');
          }
          const beginning = await terminal.waitFor('Beginning of conversation', {history: true});
          assert.equal(beginning.split(thought).length, 2, beginning);
          assert(!/\x1b\[\?(?:47|1047|1049)h/.test(terminal.raw()), 'no alternate screen');
          terminal.key('Enter');
          const live = await terminal.waitFor(/┃ draft stays here[\s\S]*The fixture answer is deterministic\./, {history: true});
          assert.deepEqual(turns(live), Array.from({length: 16}, (_, i) => i + 1));
          assert.equal(live.split('┃ draft stays here').length, 2);
          await exitWith(terminal);
          continue;
        }
        const join = await terminal.waitFor('history-turn-10');
        assert(!join.includes('history-turn-01'), `the viewport lands at the join: ${join}`);
        terminal.key('Home');
        const oldest = await terminal.waitFor('history-turn-01');
        assert(oldest.includes('Thought for ') && oldest.includes(thought), oldest);
        terminal.key('End');
        const newest = await terminal.waitFor('history-turn-16');
        for (const page of [oldest, join, newest]) {
          const turns = [...page.matchAll(/┃ history-turn-(\d+)/g)].map((m) => Number(m[1]));
          assert(turns.length >= 2 && turns.every((n, i) => !i || n === turns[i - 1] + 1), page);
        }
        terminal.key('Home');
        await terminal.waitFor('history-turn-01');
        terminal.key('PPage');
        await terminal.waitFor('Beginning of conversation');
        await exitWith(terminal);
      }
    },
  },
  {
    name: 'scrollback_streams_and_exits_cleanly',
    area: 'terminal-modes',
    title: 'The default scrollback mode streams an answer and exits into the same transcript',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const terminal = await tui.launch([], {waitForComposer: true});

      await terminal.waitFor('Welcome to DIM');
      await terminal.waitFor('Workspace write (shift+tab to cycle)');
      const startup = terminal.raw();
      assert(!startup.includes('\u001b[?1049h'), 'scrollback must not take the alternate screen');

      await terminal.sendPrompt('hello fixture');
      await terminal.waitFor(ANSWER);
      await terminal.waitFor('┃ hello fixture');
      assert.equal(lastTurnText(tui.model), 'hello fixture');

      await exitWith(terminal);

      const transcript = terminal.screen({history: true});
      assert(transcript.includes('hello fixture'), 'the scrollback keeps the message');
      assert(transcript.includes(ANSWER), 'the scrollback keeps the answer');

      const raw = terminal.raw();
      assert(!raw.includes('\u001b[?1049h'), 'a scrollback run never enters the alternate screen');
      const tail = exitTail(raw);
      assert(tail.includes('\u001b[?25h'), 'the cursor is shown again on exit');
      assert(tail.includes('state=clear'), 'leaving clears the status record');
      assert(raw.includes('\u001b]7501;state=working'), 'a run reports working through OSC 7501');
    },
  },
  {
    name: 'fullscreen_switches_screens_and_restores_the_terminal',
    area: 'terminal-modes',
    title: 'Fullscreen enters and leaves the alternate screen with a balanced keyboard protocol',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const terminal = await tui.launch(['--fullscreen'], {waitForComposer: true});

      await terminal.waitForRaw((raw) => raw.includes('\u001b[?1049h'), {describe: 'the alternate screen'});
      await terminal.waitFor('Welcome to DIM');

      await terminal.sendPrompt('hello fullscreen');
      await terminal.waitFor(ANSWER);

      await terminal.send('/tui');
      terminal.key('Enter');
      await terminal.waitForRaw((raw) => raw.includes('\u001b[?1049l'), {describe: 'the caller screen'});
      await terminal.untilComposer();
      assert(
        terminal.raw().indexOf('\u001b[?1049l') > terminal.raw().indexOf('\u001b[?1049h'),
        'leaving fullscreen returns to the caller screen',
      );

      await terminal.send('/tui');
      terminal.key('Enter');
      await terminal.waitForRaw((raw) => (raw.match(/\u001b\[\?1049h/g) ?? []).length >= 2, {
        describe: 'fullscreen again',
      });
      await terminal.untilComposer();

      await exitWith(terminal);

      const raw = terminal.raw();
      // tmux answers the kitty probe on the outer terminal's behalf without
      // negotiating it, so the app keeps xterm's ModifyOtherKeys: the enable
      // and reset pair must appear, and must balance.
      const enables = (raw.match(/\u001b\[>4;2m/g) ?? []).length;
      const resets = (raw.match(/\u001b\[>4;0m/g) ?? []).length;
      assert(enables > 0, 'the multiplexer kept ModifyOtherKeys enabled');
      assert.equal(resets, enables, `every ModifyOtherKeys enable is reset (${enables} enables, ${resets} resets)`);
      assert(!raw.includes('\u001b[>5u'), 'tmux does not negotiate the kitty protocol');
      assert(restoreTail(raw).includes('\u001b[?25h'), 'the cursor is restored on exit');
      assert(
        raw.lastIndexOf('\u001b[?1049l') > raw.lastIndexOf('\u001b[?1049h'),
        'the last screen switch leaves the alternate screen',
      );
    },
  },
  {
    name: 'fullscreen_tool_body_click_collapses_only_its_node',
    area: 'terminal-modes',
    title: 'In fullscreen, clicking an expanded tool output row collapses only that call, cancels on drag/double-click/paste/resize, and keeps the collapsed header in view when scrolled',
    async run(tui) {
      const reply = [
        'First reply paragraph.',
        'Second reply paragraph.',
        'Third reply paragraph.',
        'Fourth reply paragraph.',
        'Fifth reply paragraph.',
      ].join('\n\n');
      await tui.setup({
        respond: (request) => {
          if ((request.tools ?? []).length === 0) return {text: ANSWER};
          if (lastMessage(request).role === 'tool') return {text: reply};
          return {toolCalls: [{name: 'exec', arguments: {command: 'seq 1 15'}}]};
        },
      });
      const terminal = await tui.launch(['--fullscreen'], {waitForComposer: true});
      const cellOf = (needle, text = needle) => {
        const lines = terminal.capture({join: false}).split('\n');
        const index = lines.findIndex((line) => (needle instanceof RegExp ? needle.test(line) : line.includes(needle)));
        assert(index !== -1, `pane missing ${needle}\n${lines.join('\n')}`);
        const col = lines[index].indexOf(text);
        assert(col !== -1, `row ${index + 1} missing ${text}: ${lines[index]}`);
        return {col: col + 1, row: index + 1};
      };
      const mouse = (button, col, row, release = false) =>
        terminal.bytes(Buffer.from(`\u001b[<${button};${col};${row}${release ? 'm' : 'M'}`, 'utf8'));
      const click = ({col, row}) => {
        mouse(0, col, row, false);
        mouse(0, col, row, true);
      };

      terminal.send('count to fifteen');
      terminal.key('Enter');
      await terminal.waitFor('Permission required');
      terminal.key('y');
      await terminal.waitFor('Fifth reply paragraph.');
      await terminal.waitFor('▸ ✓ Ran 1 command');
      await terminal.untilComposer();

      click(cellOf('▸ ✓ Ran 1 command', 'Ran 1 command'));
      await terminal.waitFor('▾ ✓ Ran 1 command');
      await terminal.waitFor('▸ $ seq 1 15');

      const outputRow = (n) => new RegExp(`^ {4,}${n}(?:\\s*[\\u2581-\\u2588])?\\s*$`, 'm');
      click(cellOf('▸ $ seq 1 15', 'seq 1 15'));
      await terminal.waitFor('▾ $ seq 1 15');
      await terminal.waitFor(outputRow(5), {describe: 'expanded seq output row 5'});

      const five = cellOf(outputRow(5), '5');
      mouse(0, five.col, five.row, false);
      mouse(32, five.col + 2, five.row, false);
      mouse(0, five.col + 2, five.row, true);
      await sleep(550);
      assert(terminal.capture().includes('▾ $ seq 1 15'), 'dragging across tool output does not collapse the call');
      terminal.key('Escape');

      click(five);
      await sleep(60);
      click(five);
      await sleep(550);
      assert(terminal.capture().includes('▾ $ seq 1 15'), 'double-clicking tool output does not collapse the call');
      terminal.key('Escape');

      click(five);
      await sleep(60);
      terminal.paste('pasted draft');
      await terminal.waitFor('pasted draft', {describe: 'bracketed paste enters the composer'});
      await sleep(550);
      assert(terminal.capture().includes('▾ $ seq 1 15'), 'pasting after release cancels the pending collapse');
      terminal.key('C-c');
      await terminal.waitForAbsent('pasted draft');

      click(cellOf(outputRow(5), '5'));
      await sleep(320);
      terminal.resize(92, 14);
      await sleep(550);

      // Start at the bottom of the 92x14 pane and scroll up until output row 5
      // is visible while the owning `$ seq 1 15` header sits above the
      // viewport (also proving the near-deadline resize cancelled collapse).
      terminal.key('End');
      await terminal.waitFor('Fifth reply paragraph.', {describe: 'scrolled to bottom of 92x14 pane'});
      for (let step = 0; step < 30; step += 1) {
        const view = terminal.capture({join: false});
        if (outputRow(5).test(view) && !view.includes('$ seq 1 15')) break;
        mouse(64, 10, 4, false);
        await sleep(80);
      }
      const beforeCollapse = terminal.capture({join: false});
      assert(outputRow(5).test(beforeCollapse), `expanded row 5 visible in 92x14 pane after resize:\n${beforeCollapse}`);
      assert(!beforeCollapse.includes('$ seq 1 15'), `header scrolled above 92x14 pane:\n${beforeCollapse}`);

      click(cellOf(outputRow(5), '5'));
      await terminal.waitFor('▸ $ seq 1 15', {describe: 'collapsed tool header re-anchored in view'});
      const after = terminal.capture({join: false});
      assert(after.includes('First reply paragraph.'), `first reply paragraph stays visible:\n${after}`);
      assert(!outputRow(5).test(after), `tool output is hidden after collapse:\n${after}`);

      await exitWith(terminal);
    },
  },
  {
    name: 'resize_rewraps_history_without_losing_rows',
    area: 'terminal-modes',
    title: 'Resizing rewraps a long message and its answer in place',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const marker = 'resize-marker-0123456789';
      const prompt = `${marker} ${'the quick brown fox wraps '.repeat(4)}end-of-prompt`;
      const terminal = await tui.launch([], {waitForComposer: true});
      await terminal.sendPrompt(prompt);
      await terminal.waitFor(ANSWER);

      for (const [cols, rows] of [[60, 18], [140, 40]]) {
        await terminal.resizeTo(cols, rows);
        const transcript = terminal.screen({history: true});
        // The interface wraps at a cell, so a long message continues on the next
        // row behind its own prefix; joining the rows recovers the text.
        const unwrapped = transcript
          .split('\n')
          .map((row) => row.replace(/^\s*┃\s?/, ''))
          .join('');
        assert(unwrapped.includes(marker), `${cols}x${rows} keeps the message`);
        assert(unwrapped.includes('end-of-prompt'), `${cols}x${rows} keeps the message tail`);
        assert(transcript.includes(ANSWER), `${cols}x${rows} keeps the answer`);
        const visible = terminal.capture({join: false});
        const width = Math.max(0, ...visible.split('\n').map((line) => line.length));
        assert(width <= cols, `no visible row is wider than ${cols} columns (widest ${width})`);
        assert(width > cols - 10, `${cols}x${rows} repainted at the new width (widest ${width})`);
      }

      await exitWith(terminal);
    },
  },
  {
    name: 'resize_during_a_stream_rewraps_and_finishes',
    area: 'terminal-modes',
    title: 'Resizing while the answer streams rewraps the live output and the run still finishes',
    async run(tui) {
      const marker = 'stream-marker-0123456789';
      const tail = 'STREAM-END';
      const answer = `${marker} ${'streaming words keep arriving '.repeat(20)}${tail}`;
      await tui.setup({
        respond: (request) =>
          (request.tools ?? []).length > 0 && userText(request).includes('resize-stream')
            ? {text: answer, chunkSize: 6, chunkDelayMs: 90}
            : {text: ANSWER},
      });
      const terminal = await tui.launch([], {waitForComposer: true});
      await terminal.sendPrompt('resize-stream please');
      await terminal.waitFor('working…');
      await terminal.waitFor('stream-marker', {describe: 'the streamed answer'});

      const painted = await terminal.resizeTo(60, 18, {
        expectComposer: false,
        stable: false,
        until: (text) => text.includes('stream-marker'),
      });
      const visible = terminal.capture({join: false});
      const widest = Math.max(0, ...visible.split('\n').map((line) => line.length));
      assert(widest > 50 && widest <= 60, `the live stream was repainted at 60 columns (widest ${widest})`);
      assert(painted.includes('stream-marker'), 'the streamed text survived the resize');

      await terminal.waitFor(tail, {timeout: 30_000});
      const unwrapped = terminal
        .screen({history: true})
        .split('\n')
        .map((row) => row.replace(/^\s*[┃│]\s?/, ''))
        .join('');
      assert(unwrapped.includes(marker), 'the whole streamed message is kept');
      assert(unwrapped.includes(tail), 'the stream finished after the resize');
      await exitWith(terminal);
    },
  },
  {
    name: 'resize_during_thinking_rewraps_multiline_history',
    area: 'terminal-modes',
    title: 'Resizing while expanded reasoning streams keeps every history row and still answers',
    async run(tui) {
      const userLines = ['first user line', 'second user line', 'last user line'];
      const thoughts = [
        'First thought: keep every history row',
        'Second thought: rewrap the reasoning',
        'Third thought: settle into the answer',
      ];
      const thinking = `${thoughts.join('\n')}\n${'checking the resize barrier '.repeat(16)}REASONING-END`;
      const answer = 'The answer arrives after the reasoning.';
      await tui.setup({
        config: {tuiThinkingDetailsExpanded: true},
        respond: (request) =>
          (request.tools ?? []).length > 0
            ? {thinking, text: answer, chunkSize: 4, chunkDelayMs: 60}
            : {text: ANSWER},
      });
      const terminal = await tui.launch([], {waitForComposer: true});
      terminal.send(userLines[0]);
      terminal.key('C-j');
      terminal.send(userLines[1]);
      terminal.key('C-j');
      terminal.send(userLines[2]);
      terminal.key('Enter');
      await terminal.waitFor('First thought', {history: true, describe: 'the streamed reasoning'});
      await terminal.waitFor('checking the resize barrier', {history: true, describe: 'the reasoning body'});
      const unwrap = (text) =>
        text
          .split('\n')
          .map((row) => row.replace(/^\s*[┃│]\s?/, ''))
          .join('');

      for (const [cols, rows] of [[100, 24], [60, 24], [120, 40], [45, 18], [100, 36]]) {
        const painted = await terminal.resizeTo(cols, rows, {
          stable: false,
          until: (text) => [...userLines, ...thoughts].every((line) => unwrap(text).includes(line)),
        });
        assert(painted.includes('Ask anything...'), `${cols}x${rows} keeps the composer`);
        const unwrapped = unwrap(painted);
        for (const line of userLines) {
          assert(unwrapped.includes(line), `${cols}x${rows} keeps ${line}`);
        }
        for (const thought of thoughts) {
          assert(unwrapped.includes(thought), `${cols}x${rows} keeps ${thought}`);
        }
      }

      await terminal.waitFor(answer, {timeout: 30_000, describe: 'the answer after the reasoning'});
      const settled = terminal.screen({history: true});
      assert(settled.includes('Thought'), 'the reasoning settled into a thought row');
      for (const thought of thoughts) {
        assert(settled.includes(thought), `the settled transcript keeps ${thought}`);
      }
      assert.equal(
        lastTurnText(tui.model),
        userLines.join('\n'),
        'the model received the multiline prompt once',
      );
      await exitWith(terminal);
    },
  },
  {
    name: 'fullscreen_composer_click_places_the_cursor',
    area: 'composer',
    title: 'Clicking an editable cell inserts there in single-line and scrolled, wrapped drafts',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const terminal = await tui.launch(['--fullscreen'], {cols: 28, rows: 18, waitForComposer: true});
      const cellOf = async (text, cellOffset) => {
        await terminal.waitFor(text);
        const lines = terminal.capture({join: false}).split('\n');
        const row = lines.findIndex((line) => line.includes(text));
        assert(row >= 0, lines.join('\n'));
        return {column: lines[row].indexOf(text) + cellOffset + 1, row: row + 1};
      };
      const mouse = (button, {column, row}, release = false) =>
        terminal.bytes(Buffer.from(`\x1b[<${button};${column};${row}${release ? 'm' : 'M'}`));
      const click = (cell) => {
        mouse(0, cell);
        mouse(0, cell, true);
      };

      terminal.send('abcdef');
      click(await cellOf('abcdef', 2));
      terminal.send('X');
      const first = await cellOf('abXcdef', 0);
      const second = {...first, column: first.column + 1};

      mouse(0, first);
      mouse(0, second, true);
      terminal.send('1');
      await terminal.waitFor('abX1cdef');

      mouse(0, first);
      mouse(32, second);
      mouse(0, first, true);
      terminal.send('2');
      await terminal.waitFor('abX12cdef');

      mouse(0, first);
      terminal.send('3');
      mouse(0, first, true);
      terminal.send('4');
      await terminal.waitFor('abX1234cdef');

      terminal.key('End', 'C-j');
      terminal.paste('pad '.repeat(45) + '中界tail');
      click(await cellOf('中界tail', 2));
      assert(!terminal.capture().includes('abX1234cdef'), 'the first draft line has scrolled out of view');
      terminal.send('Y');
      await terminal.waitFor('中Y界tail');
    },
  },
  {
    name: 'scrollback_click_places_draft_cursor',
    area: 'composer',
    title: 'Scrollback captures clicks only while editing and inserts at the clicked cell',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const terminal = await tui.launch(['--scrollback'], {waitForComposer: true, cols: 60});
      const mouseEnabled = () => tui.tmuxRun(['display-message', '-p', '-t', terminal.session, '#{mouse_any_flag}']).trim() === '1';
      assert(!mouseEnabled(), 'empty scrollback draft preserves native selection');
      const prefix = 'a'.repeat(60);
      terminal.send(`${prefix}界bc`);
      terminal.key('C-j');
      terminal.send('last line');
      await terminal.waitFor('last line');
      assert(mouseEnabled(), 'editing enables terminal mouse reporting');
      const lines = terminal.capture({join: false}).split('\n');
      const row = lines.findIndex((line) => line.includes('界bc'));
      assert(row > 0 && lines[row - 1].includes('aaaa'), 'target is on a wrapped row');
      const report = `\x1b[<0;${lines[row].indexOf('界') + 3};${row + 1}`;
      terminal.bytes(Buffer.from(`${report}M${report}m`));
      terminal.send('X');
      terminal.key('Enter');
      await terminal.waitFor(ANSWER);
      assert.equal(lastTurnText(tui.model), `${prefix}界Xbc\nlast line`);
      assert(!mouseEnabled(), 'submitting releases the mouse');
      const draft = Array.from({length: 18}, (_, i) => `draft-row-${i}`);
      terminal.bytes(Buffer.from(draft.map((line) => `\x1b[200~${line}\x1b[201~`).join('\n')));
      await terminal.waitFor('draft-row-17');
      const visible = terminal.capture({join: false}).split('\n');
      const first = visible.findIndex((line) => line.includes('draft-row-'));
      assert(first >= 0 && !visible[first].includes('draft-row-0'), 'the editor is scrolled');
      const oldCell = `\x1b[<0;${visible[first].indexOf('draft-row-') + 1};${first + 1}`;
      terminal.bytes(Buffer.from(`\x1b[200~\nnew-row\x1b[201~${oldCell}M${oldCell}mX`));
      await terminal.waitFor('new-rowX');
      const before = terminal.capture({join: false}).split('\n')[first];
      terminal.paste('\nlast-row');
      await terminal.waitFor('last-row');
      const after = terminal.capture({join: false}).split('\n')[first];
      assert.notEqual(before, after, 'the old screen cell now shows another draft row');
      terminal.bytes(Buffer.from(`${oldCell}M${oldCell}mY`));
      const edited = await terminal.waitFor(/last-rowY|Ydraft-row-/);
      assert(edited.includes('last-rowY'), edited);
      terminal.bytes(Buffer.from(`${oldCell}M${oldCell}mZ`));
      const target = after.match(/draft-row-(\d+)/);
      assert(target, after);
      await terminal.waitFor(`Z${target[0]}`);
      draft[Number(target[1])] = `Z${target[0]}`;
      terminal.key('Enter');
      await waitForRequest(tui.model, (request) => userText(request) === `${draft.join('\n')}\nnew-rowX\nlast-rowY`);
      await exitWith(terminal);
      const raw = terminal.raw();
      assert(raw.slice(raw.lastIndexOf('\x1b[?2026l\x1b[0m')).includes('\x1b[?1006l\x1b[?1003l\x1b[?1002l'), 'exit resets mouse reporting even after mouse-off was queued');
    },
  },
  {
    name: 'fullscreen_composer_click_with_a_clipped_task_strip',
    area: 'composer',
    title: 'Clicking a wrapped draft uses the visible textarea when task rows squeeze the composer',
    async run(tui) {
      const tasks = ['A', 'B', 'C'];
      await tui.setup({
        respond: (request) => {
          if ((request.tools ?? []).length === 0) return {text: ANSWER};
          if (lastMessage(request).role === 'tool') return {text: 'Started the composer tasks.'};
          return {
            toolCalls: tasks.map((name) => ({
              name: 'exec',
              arguments: {
                command: `sleep 60; echo composer-task-${name}`,
                runInBackground: true,
                title: `Composer task ${name}`,
              },
            })),
          };
        },
      });
      const terminal = await tui.launch(['--fullscreen'], {cols: 80, rows: 24, waitForComposer: true});
      terminal.send('start three composer tasks');
      terminal.key('Enter');
      const pending = new Set(tasks);
      while (pending.size > 0) {
        const command = new RegExp(`composer-task-([${[...pending].join('')}])`);
        const screen = await terminal.waitFor(new RegExp(`Permission required[\\s\\S]*${command.source}`));
        pending.delete(screen.slice(screen.lastIndexOf('Permission required')).match(command)[1]);
        terminal.key('y');
      }
      await terminal.waitFor('3 in background');
      terminal.resize(80, 12);

      const wrappedRowWidth = 72;
      terminal.paste('first'.padEnd(wrappedRowWidth, '1') + 'target'.padEnd(wrappedRowWidth, '2') + 'tail');
      await terminal.waitFor('target222');
      const lines = terminal.capture({join: false}).split('\n');
      const row = lines.findIndex((line) => line.includes('target222'));
      assert(row >= 0 && lines[row + 1]?.includes('tail'), lines.join('\n'));
      assert(!lines.some((line) => line.includes('first111')), lines.join('\n'));
      const column = lines[row].indexOf('target') + 3;
      const report = `\x1b[<0;${column};${row + 1}`;
      terminal.bytes(Buffer.from(`${report}M${report}m`));
      terminal.send('X');
      await terminal.waitFor('taXrget');
    },
  },
  {
    name: 'composer_edits_multiline_and_stores_a_large_paste',
    area: 'composer',
    title: 'The composer edits a newline draft and stores a multi-line paste as an attachment',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const terminal = await tui.launch([], {waitForComposer: true});

      terminal.send('first line');
      terminal.key('C-j');
      terminal.send('second line');
      await terminal.waitFor('second line');
      for (let index = 0; index < 5; index += 1) terminal.key('BSpace');
      terminal.send('END');
      await terminal.waitFor('secondEND');
      terminal.key('Enter');
      await terminal.untilComposer();
      await terminal.waitFor(ANSWER);
      assert.equal(lastTurnText(tui.model), 'first line\nsecondEND');

      const pasted = ['alpha', 'beta', 'gamma', 'delta', 'epsilon'].join('\n');
      terminal.paste(pasted);
      await terminal.waitFor('[... 5 lines]');
      terminal.key('Enter');
      await terminal.untilComposer();
      await terminal.waitFor('[Pasted text:', {history: true});
      const sent = await waitForRequest(
        tui.model,
        (request) => (request.tools ?? []).length > 0 && userText(request).includes('[Pasted text:'),
        {describe: 'the stored paste in a turn request'},
      );
      assert(!userText(sent.body).includes('epsilon'), 'a stored paste is not inlined into the prompt');

      await exitWith(terminal);
    },
  },
  {
    name: 'remote_attachment_draft_survives_refusal_and_uncertain_delivery',
    area: 'composer',
    title: 'Recover confirmed attachments and reconcile a lost submission receipt on the original line',
    run: draftDelivery,
  },
  {
    name: 'remote_draft_uploads_local_bytes_and_mentions_target_files',
    requiresSharing: true,
    area: 'composer',
    title: 'Both terminal layouts send dragged bytes and choose mentions on the selected computer',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const env = tui.environment({DIMCODE_SHARING_LISTEN: '127.0.0.1:0'});
      const targetHome = path.join(tui.root, 'receiver');
      const project = fs.realpathSync(path.join(tui.root, 'workspace'));
      const name = 'notes.txt';
      const source = path.join(tui.root, name);
      fs.writeFileSync(source, 'keyboard-side notes');
      fs.writeFileSync(path.join(project, name), 'workspace-side notes');
      const server = (...args) => execFileSync(tui.binary, ['--home', targetHome, ...args],
        {env, cwd: project, encoding: 'utf8', timeout: 20000});
      const {addresses: [address], token} = JSON.parse(server('share', 'enable'));
      try {
        server('share', 'rename', 'ReviewOffice');
        const owner = await rpc(path.join(targetHome, 'rust', 'daemon.sock'));
        const providerId = '018ec73f-4d19-7f61-bc79-7130618bb455', modelId = 'fixture-model';
        try {
          const created = await owner.call('provider.create', {
            providerId, displayName: 'Fixture', enabled: true,
            connection: {driverKind: 'openai-compatible', auth: {type: 'anonymous'}, baseUrl: tui.model.url},
            models: [modelId, 'fixture-text'].map(id => ({
              modelId: id, displayName: id, status: 'active', capabilities: {
                modalities: id === modelId ? ['text', 'image'] : ['text'], vision: id === modelId,
                audio: false, toolCalling: true, streaming: true, structuredOutput: true, reasoning: false,
                contextWindow: 128000, maxOutputTokens: 4096,
              },
            })),
          });
          assert.ok(created.result, JSON.stringify(created.error));
          const selected = await owner.call('provider.select', {providerId,modelId});
          assert.ok(selected.result, JSON.stringify(selected.error));
        } finally {
          owner.stream.destroy();
        }
        const localPng = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC', targetPng = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNgYPgPAAEDAQAIicLsAAAAAElFTkSuQmCC';
        const photo = path.join(tui.root, 'photo.png');
        fs.writeFileSync(photo, Buffer.from(localPng, 'base64'));
        fs.writeFileSync(path.join(project, 'photo.png'), Buffer.from(targetPng, 'base64'));
        const command = [tui.binary, '--home', tui.home, 'share', 'connect', address, '--'];
        execFileSync(command[0], [...command.slice(1), 'mcp', 'list'], {input: token + '\n', env, cwd: project, timeout: 20000, stdio: ['pipe','pipe','pipe']});
        for (const args of [[], ['--fullscreen']]) {
          const terminal = await tui.launch([...args, '--cwd', project], {command, env, cols: 110, rows: 32, waitForComposer: true});
          terminal.send('Compare ');
          terminal.paste(source);
          await terminal.waitFor('[notes.txt]');
          terminal.send(' to @notes');
          await terminal.waitFor('@notes.txt');
          terminal.key('Tab', 'Enter');
          await terminal.waitFor(ANSWER);
          const sent = lastTurnText(tui.model);
          assert.ok(sent.includes('[@notes.txt](' + path.join(project, name) + ')'), sent);
          const ref = /\[notes\.txt\]\(<?([^)>]+)>?\)/.exec(sent);
          assert.ok(ref, sent);
          assert.ok(ref[1].startsWith(fs.realpathSync(targetHome) + path.sep), ref[1]);
          assert.equal(fs.readFileSync(ref[1], 'utf8'), 'keyboard-side notes');
          const prompt = 'Compare pictures ' + (args.length ? 'fullscreen' : 'scrollback');
          terminal.send(prompt + ' ');
          terminal.paste(photo);
          await terminal.waitFor('[Image #1]');
          terminal.send(' and @photo');
          await terminal.waitFor('@photo.png');
          terminal.key('Tab', 'Enter');
          const pictures = await waitForRequest(tui.model, request => request.tools?.length && userText(request).includes(prompt) &&
            (lastMessage(request).content ?? []).some?.(part => part.type === 'image_url'));
          const urls = lastMessage(pictures.body).content.filter(part => part.type === 'image_url').map(part => part.image_url.url);
          assert.deepEqual(urls, [localPng,targetPng].map(bytes => 'data:image/png;base64,' + bytes));
          await terminal.waitFor(ANSWER);
          await exitWith(terminal);
        }
        const line = await tui.launch(['--no-tui', '--cwd', project], {command, env, cols: 120, rows: 30});
        await line.waitFor('Enter: send');
        line.send('Line comparison ');
        line.paste(source);
        line.send(' and @photo');
        await line.waitFor('* photo.png');
        line.key('Tab', 'Enter');
        const lineRequest = await waitForRequest(tui.model, request =>
          request.tools?.length && userText(request).includes('Line comparison'));
        const content = lastMessage(lineRequest.body).content;
        assert.equal(content.find(part => part.type === 'image_url')?.image_url.url, 'data:image/png;base64,' + targetPng);
        const uploaded = /\[notes\.txt\]\(<?([^)>]+)>?\)/.exec(content.filter(part => part.type === 'text').map(part => part.text).join(''));
        assert.ok(uploaded);
        assert.equal(fs.readFileSync(uploaded[1], 'utf8'), 'keyboard-side notes');
        await line.waitFor('Enter: send');
        line.send('/exit');
        line.key('Enter');
        assert.equal((await line.waitForExit()).status, 0);
        await tui.runCommandAsync([...command.slice(1), '--cwd', project, 'exec', '-p', 'Inspect headless photo', '--image', photo], {env});
        const headless = await waitForRequest(tui.model, request => request.tools?.length && userText(request).includes('Inspect headless photo'));
        assert.ok(userText(headless.body).includes(localPng));
        const plain = await tui.launch(['--no-tui', '--cwd', project, '--provider', providerId, '--model', 'fixture-text'], {command, env});
        await plain.waitFor('Enter: send');
        plain.send('Text model file ');
        plain.paste(photo);
        plain.send(' and @photo');
        await plain.waitFor('* photo.png');
        plain.key('Tab', 'Enter');
        const textOnly = await waitForRequest(tui.model, request => request.tools?.length && userText(request).includes('Text model file'));
        const text = userText(textOnly.body);
        assert.ok(text.includes(path.join(project, 'photo.png')), text);
        assert.ok(!text.includes('image_url'), text);
        const storedPhoto = /\[photo\.png\]\(<?([^)>]+)>?\)/.exec(text);
        assert.ok(storedPhoto, text);
        assert.equal(fs.readFileSync(storedPhoto[1]).toString('base64'), localPng);
        await plain.waitFor('Enter: send');
        plain.send('/exit');
        plain.key('Enter');
        assert.equal((await plain.waitForExit()).status, 0);
      } finally {
        server('daemon', 'stop');
      }
    },
  },
  {
    name: 'file_mention_inserts_a_workspace_link',
    area: 'composer',
    title: 'The @ picker lists the workspace and inserts the chosen file into the draft',
    async run(tui) {
      await tui.setup({
        files: {
          'lantern-notes.md': '# Notes\n\nThe fixture note mentions a lantern.\n',
          'src/app.ts': 'export const answer = 42;\n',
        },
        respond: () => ({text: ANSWER}),
      });
      const workspace = fs.realpathSync(path.join(tui.root, 'workspace'));
      const terminal = await tui.launch(['--cwd', workspace], {waitForComposer: true});

      terminal.send('read @lantern');
      await terminal.waitFor('Files');
      await terminal.waitFor('@lantern-notes.md');
      terminal.key('Enter');
      await terminal.waitFor('@lantern-notes.md', {describe: 'the inserted mention'});
      // Typing a path prefix completes to the file under it, so a directory is
      // picked the way a file is.
      terminal.send('@sr');
      await terminal.waitFor('@src/');
      terminal.key('Tab');
      await terminal.waitFor('@src/app.ts', {describe: 'the completed directory prefix'});
      terminal.key('Tab');
      terminal.key('Enter');
      await terminal.untilComposer();
      await terminal.waitFor(ANSWER);

      const sent = lastTurnText(tui.model);
      for (const name of ['lantern-notes.md', 'src/app.ts']) {
        const link = `[@${name}](${path.join(workspace, name)})`;
        assert(sent.includes(link), `the prompt links ${name}: ${sent}`);
      }

      await exitWith(terminal);
    },
  },
  {
    name: 'home_commands_without_a_session_answer_and_send_nothing',
    area: 'no-session',
    title: 'Commands that need a session answer at home, keep the draft and reach no model',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      // The palette is the only way to run a command over a draft: typing `/`
      // into a non-empty composer appends it, so every command here is picked
      // from the palette and the draft it restored is asserted after it.
      const runFromPalette = async (terminal, filter, expected) => {
        terminal.key('C-o');
        await terminal.waitFor(/Commands|Command Palette/, {describe: 'the command palette'});
        terminal.send(filter);
        await terminal.waitFor(`/${filter}`, {describe: `the ${filter} entry`});
        terminal.key('Enter');
        await terminal.waitFor(expected);
      };
      for (const args of [[], ['--fullscreen']]) {
        const terminal = await tui.launch(args, {waitForComposer: true});
        await terminal.waitFor('Welcome to DIM');
        terminal.send('kept draft');
        await terminal.waitFor('kept draft');

        await runFromPalette(terminal, 'export', '/export needs a session');
        assert(!terminal.capture().includes('Export session'), 'no export dialog opened');
        assert(terminal.capture().includes('kept draft'), 'the draft survives /export');

        await runFromPalette(terminal, 'queue', '/queue needs a session');
        assert(terminal.capture().includes('kept draft'), 'the draft survives /queue');

        await runFromPalette(terminal, 'mcp list', 'No MCP server is configured.');
        assert(terminal.capture().includes('kept draft'), 'the draft survives /mcp list');

        assert(
          !terminal.capture({history: true}).includes('┃ kept draft'),
          'the commands never sent the draft',
        );
        assert.equal(tui.model.requests.length, 0, 'no home command asked the fixture model');

        for (let index = 0; index < 'kept draft'.length; index += 1) terminal.key('BSpace');
        await terminal.waitForAbsent('kept draft');
        await exitWith(terminal);
      }
    },
  },
  {
    name: 'hook_context_and_stop_follow_up_stay_status_rows',
    area: 'hooks',
    title: 'A prompt hook context and a stop follow-up draw one closed row each and reach the model',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const workspace = fs.realpathSync(path.join(tui.root, 'workspace'));
      const script = path.join(workspace, '.dimcode', 'stop-hook.sh');
      fs.mkdirSync(path.dirname(script), {recursive: true});
      fs.writeFileSync(
        script,
        [
          `if [ -f ${quote(path.join(workspace, '.dimcode', 'stop-once'))} ]; then exit 0; fi`,
          `: > ${quote(path.join(workspace, '.dimcode', 'stop-once'))}`,
          `printf '%s' '{"decision":"block","reason":"run the suite\\nfix parser::tests\\nkeep the diff small"}'`,
          '',
        ].join('\n'),
      );
      fs.writeFileSync(
        path.join(workspace, '.dimcode', 'hooks.json'),
        JSON.stringify({
          hooks: {
            UserPromptSubmit: [
              {hooks: [{type: 'command', command: "printf 'SEARCH-MEMORY-FIRST\\n'"}]},
            ],
            Stop: [{hooks: [{type: 'command', command: `sh ${quote(script)}`}]}],
          },
        }),
      );
      tui.runCommand(['--home', tui.home, '--model-config', tui.modelFile, 'hooks', 'enable'], {cwd: workspace});
      const operator = 'why does the parser stall?';
      const terminal = await tui.launch(['--cwd', workspace], {waitForComposer: true});

      await terminal.sendPrompt(operator);
      await terminal.waitFor(ANSWER);
      await terminal.waitFor('Hook context · 1 line ▸');
      assert(!terminal.capture().includes('SEARCH-MEMORY-FIRST'), 'a closed hook row keeps its body closed');

      const turn = await waitForRequest(
        tui.model,
        (request) => (request.tools ?? []).length > 0 && userText(request).includes(operator),
        {describe: 'the prompt-hook turn'},
      );
      assert(
        userText(turn.body).includes('SEARCH-MEMORY-FIRST'),
        'the hook context reaches the model with the operator words',
      );
      assert(
        userText(turn.body).indexOf(operator) < userText(turn.body).indexOf('SEARCH-MEMORY-FIRST'),
        'the operator words stay the message and the hook context follows',
      );

      await terminal.waitFor('Stop hook follow-up · 3 lines ▸', {timeout: 30_000});
      assert(
        !terminal.capture({history: true}).includes('1 lines'),
        'a one-line hook context is named in the singular',
      );
      assert(
        !terminal.capture({history: true}).includes('<dim-hooks'),
        'the marker is the model\'s, not the transcript\'s',
      );
      for (const line of ['run the suite', 'fix parser::tests', 'keep the diff small']) {
        assert(!terminal.capture().includes(line), `a closed follow-up keeps ${line} out of the transcript`);
      }
      await waitForRequest(
        tui.model,
        (request) =>
          (request.tools ?? []).length > 0 &&
          userText(request).includes('<dim-hooks kind="follow-up">'),
        {describe: 'the stop-hook follow-up turn'},
      );

      terminal.send('/tools');
      terminal.key('Enter');
      await terminal.waitFor('Hook context · 1 line ▸');
      terminal.send('/tools');
      terminal.key('Enter');
      await terminal.waitFor('Hook context · 1 line ▾');
      await terminal.waitFor('SEARCH-MEMORY-FIRST');
      await terminal.waitFor('Stop hook follow-up · 3 lines ▾');
      await terminal.waitFor('fix parser::tests');
      terminal.send('/tools');
      terminal.key('Enter');
      await terminal.waitFor('Hook context · 1 line ▸');
      assert(!terminal.capture().includes('SEARCH-MEMORY-FIRST'), 'the row closes again');

      await exitWith(terminal);
    },
  },
  {
    name: 'remote_resume_lists_sibling_worktree_sessions',
    area: 'commands',
    title: 'List sessions in sibling worktrees of the selected App Server',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const repo = fs.realpathSync(path.join(tui.root, 'workspace'));
      const sibling = path.join(tui.root, 'linked checkout');
      const single = path.join(tui.root, 'single checkout');
      const targetHome = path.join(tui.root, 'receiver');
      const target = ['--home', targetHome, '--model-config', tui.modelFile];
      const git = (...args) => execFileSync('git', ['-C', repo, ...args], {env: tui.environment({}), stdio: 'pipe'});
      git('init', '-q');
      fs.mkdirSync(path.join(repo, 'sub'));
      fs.writeFileSync(path.join(repo, 'sub', 'seed'), 'fixture');
      git('add', 'sub/seed');
      git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'initial');
      git('worktree', 'add', '--detach', sibling);
      git('clone', '-q', repo, single);
      const bin = path.join(tui.root, 'keyboard-bin');
      fs.mkdirSync(bin);
      const marker = path.join(bin, 'called');
      fs.writeFileSync(path.join(bin, 'git'), `#!/bin/sh\n: > ${quote(marker)}\nexit 1\n`, {mode: 0o700});
      try {
        for (const [cwd, title] of [[path.join(sibling, 'sub'), 'linked checkout session'], [path.join(single, 'sub'), 'single checkout session']]) {
          const output = await tui.runCommandAsync([...target, '--cwd', cwd, 'exec', '--json', '-p', title]);
          const id = output.trim().split('\n').map(line => JSON.parse(line).params?.sessionId).find(Boolean);
          assert(id, 'the target created a session');
          tui.runCommand([...target, 'session', 'rename', id, title]);
        }
        const env = {
          DIMCODE_TERMINAL_PROXY: path.join(targetHome, 'rust', 'daemon.sock'),
          DIMCODE_TERMINAL_NAME: 'Build host', DIMCODE_TERMINAL_ID: 'fixture', DIMCODE_TERMINAL_ADDRESS: '127.0.0.1:1',
          PATH: bin + path.delimiter + process.env.PATH,
        };
        for (const mode of [[], ['--fullscreen']]) {
          for (const [cwd, included, excluded] of [
            [path.join(repo, 'sub'), 'linked checkout session', 'single checkout session'],
            [path.join(single, 'sub') + '/', 'single checkout session', 'linked checkout session'],
          ]) {
            const terminal = await tui.launch(mode, {
              command: [tui.binary, '--home', tui.home], env: {...env, DIMCODE_TERMINAL_WORKSPACE: cwd}, cols: 112, rows: 30,
            });
            await terminal.untilComposer();
            terminal.send('/resume');
            terminal.key('Enter');
            await terminal.waitFor('Sessions · This repository');
            const screen = await terminal.waitFor(included);
            assert(!screen.includes(excluded));
            terminal.key('C-s');
            await terminal.waitFor(excluded);
            terminal.key('Escape');
            await terminal.waitForAbsent('Sessions · All workspaces');
            await exitWith(terminal);
          }
        }
        assert(!fs.existsSync(marker), 'the terminal must never run Git against target paths');
      } finally {
        tui.runCommand([...target, 'daemon', 'stop']);
      }
    },
  },
  {
    name: 'devices_open_remote_directory_and_return_to_local',
    area: 'commands',
    title: 'Switch targets from Home, pair once, browse the target and return in both layouts',
    async run(tui) {
      let complete, waitingFor;
      const includesInput = (request, text) => request.tools?.length && request.messages.some(message =>
        message.role === 'user' && typeof message.content === 'string' && message.content.includes(text));
      await tui.setup({respond: async request => {
        if (waitingFor && includesInput(request, waitingFor)) {
          waitingFor = undefined;
          await new Promise(resolve => { complete = resolve; });
          return {text: 'Remote work finished'};
        }
        if (request.tools?.length && userText(request).includes('request fixture approval') && lastMessage(request).role !== 'tool') {
          return {toolCalls: [{name: 'exec', arguments: {command: 'echo decision-fixture'}}]};
        }
        if (userText(request).includes('Draft for the side task')) return {text: 'Side draft delivered'};
        return {text: ANSWER};
      }});
      const env = tui.environment({DIMCODE_SHARING_LISTEN: '127.0.0.1:0'});
      if (!fs.existsSync(path.join(path.dirname(tui.binary), 'dim-sharing'))) {
        const terminal = await tui.launch([], {env, waitForComposer: true});
        terminal.send('/devices');
        terminal.key('Enter');
        await terminal.waitFor('This computer');
        terminal.key('Home', 'Enter');
        await terminal.waitFor('This computer · Active sessions');
        await terminal.waitFor('New session…');
        return;
      }
      const remoteHome = path.join(tui.root, 'other');
      const project = path.join(tui.root, 'remote-project');
      fs.mkdirSync(project);
      fs.mkdirSync(path.join(project, 'vendor'));
      fs.mkdirSync(path.join(project, '.fixture-hidden'));
      const remote = (...args) => execFileSync(tui.binary, ['--home', remoteHome, '--model-config', tui.modelFile, ...args],
        {encoding: 'utf8', env, cwd: project, timeout: 20000});
      const shared = JSON.parse(remote('share', 'enable'));
      try {
        remote('share', 'rename', 'OfficeFixture');
        for (const [index, args] of [[], ['--fullscreen']].entries()) {
          const terminal = await tui.launch([...args, '--cwd', project], {env, waitForComposer: true, cols: 120, rows: 36});
          terminal.send('/devices');
          terminal.key('Enter');
          await terminal.waitFor('This computer');
          terminal.key('Escape');
          await terminal.waitForAbsent('Computers');
          terminal.send('/devices');
          terminal.key('Enter');
          await terminal.waitFor('Add by address');
          terminal.key('End', 'Enter');
          await terminal.waitFor('Computer address');
          terminal.send(shared.addresses[0]);
          terminal.key('Enter');
          if (!index) {
            await terminal.waitFor('Sharing token');
            terminal.send(shared.token);
            assert(!terminal.capture().includes(shared.token));
            terminal.key('Enter');
          }
          await terminal.waitFor('OfficeFixture · Active sessions');
          terminal.key('End', 'Enter');
          await terminal.waitFor('Enter a path');
          terminal.key('End', 'Enter');
          await terminal.waitFor('Directory path');
          terminal.send(project);
          terminal.key('Enter');
          await terminal.waitFor('vendor');
          assert(!terminal.capture().includes('.fixture-hidden'));
          terminal.key('End', 'Up', 'Enter');
          await terminal.waitFor('OfficeFixture:');
          terminal.send('/devices');
          terminal.key('Enter');
          await terminal.waitFor('This computer');
          terminal.key('Home', 'Enter');
          await terminal.waitForAbsent('OfficeFixture:');
          await terminal.waitFor('Ask anything');
          const owner = await tui.launch(['--cwd', project], {
            command: [tui.binary, '--home', remoteHome, '--model-config', tui.modelFile],
            env, waitForComposer: true,
          });
          const topic = `Work on remote fixture ${index}`;
          await owner.sendPrompt(topic);
          await owner.waitFor(ANSWER);
          const devicesOverDraft = async () => {
            terminal.key('C-o');
            await terminal.waitFor(/Commands|Command Palette/);
            terminal.send('devices');
            terminal.key('Enter');
            await terminal.waitFor('Remembered');
          };
          terminal.send('local draft ');
          terminal.paste(['one', 'two', 'three', 'four', 'five'].join('\n'));
          await terminal.waitFor('[... 5 lines]');
          await devicesOverDraft();
          terminal.key('Home', 'Down', 'Enter');
          await terminal.waitFor('OfficeFixture · Active sessions');
          terminal.key('Home', 'Enter');
          await terminal.waitFor('OfficeFixture:');
          await terminal.waitFor(topic);
          assert(!terminal.capture().includes('local draft'));
          terminal.send('remote draft');
          await devicesOverDraft();
          terminal.key('End', 'Enter');
          await terminal.waitFor('Computer address');
          terminal.send('127.0.0.1:1');
          terminal.key('Enter');
          await terminal.waitFor(/refused/i);
          terminal.key('Escape');
          await terminal.waitForAbsent('Computers');
          await terminal.waitFor('remote draft');
          const calls = turnRequests(tui.model).length;
          const reset = JSON.parse(remote('share', 'reset'));
          await terminal.waitFor('Reconnect OfficeFixture');
          terminal.key('Home', 'Enter');
          await terminal.waitFor('Sharing token');
          terminal.send(reset.token);
          terminal.key('Enter');
          await terminal.waitForAbsent('Computers');
          await terminal.waitFor(topic);
          await terminal.waitFor('remote draft');
          assert.equal(turnRequests(tui.model).length, calls, 'reconnecting preserves unsent input');
          const working = `keep working after switch ${index}`;
          waitingFor = working;
          await owner.sendPrompt(working);
          await waitForRequest(tui.model, request => includesInput(request, working));
          await terminal.waitFor(working);
          await devicesOverDraft();
          terminal.key('Home', 'Enter');
          await terminal.waitForAbsent('OfficeFixture:');
          await terminal.waitFor('local draft');
          complete();
          await owner.waitFor('Remote work finished');
          complete = undefined;
          terminal.key('Enter');
          const local = await waitForRequest(tui.model, request => request.tools?.length
            && userText(request).includes('local draft') && userText(request).includes('[Pasted text:'));
          assert(!userText(local.body).includes('[... 5 lines]'), 'the draft retained its paste content');
          await terminal.waitFor(ANSWER);
          await checkDeviceDraftDestination(terminal, devicesOverDraft);
          await exitWith(terminal);
          await exitWith(owner);
        }
        const named = await tui.launch(['--target', 'OfficeFixture'], {env});
        await named.waitFor('OfficeFixture · Active sessions');
        named.key('Escape');
        await named.waitFor('Computers');
        named.key('Escape');
        await named.waitForAbsent('Computers');
        named.send('/devices');
        named.key('Enter');
        await named.waitFor('This computer');
        named.key('C-c');
        await named.waitFor('Press Ctrl+C again to exit');
        named.key('C-c');
        assert.equal((await named.waitForExit()).status, 130);
        const disconnected = await tui.launch(['--target', 'OfficeFixture'], {env});
        await disconnected.waitFor('OfficeFixture · Active sessions');
        disconnected.key('End', 'Enter');
        await disconnected.waitFor('Start a session here');
        remote('share', 'disable');
        await disconnected.waitFor('Disconnected · draft kept');
        disconnected.key('C-d');
        assert.equal((await disconnected.waitForExit()).status, 0);
      } finally {
        complete?.();
        remote('daemon', 'stop');
      }
    },
  },
  {
    name: 'settings_manage_inbound_sharing',
    area: 'commands',
    title: 'Settings controls inbound access, displays the complete token and revokes it on reset',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const terminal = await tui.launch(['--fullscreen'], {
        env: {DIMCODE_SHARING_LISTEN: '127.0.0.1:0'}, waitForComposer: true, cols: 90, rows: 36,
      });
      terminal.send('/settings');
      terminal.key('Enter');
      await terminal.waitFor('Language');
      terminal.key('End');
      if (!fs.existsSync(path.join(path.dirname(tui.binary), 'dim-sharing'))) {
        assert(!terminal.capture().includes('Remote access'));
        return;
      }
      const status = () => JSON.parse(execFileSync(tui.binary, ['--home', tui.home, 'share', 'status'], {
        env: tui.environment({}), encoding: 'utf8',
      }));
      assert.equal(status().enabled, false);
      await terminal.waitFor('Remote access');
      terminal.key('Enter');
      await terminal.waitFor('Allow connections to this computer');
      terminal.key('Enter');
      await terminal.waitFor('DIM1-');
      const token = status().token;
      terminal.key('Down', 'Enter');
      await terminal.waitFor('Enter save');
      terminal.send('-fixture');
      terminal.key('Enter');
      await terminal.waitFor('-fixture');
      terminal.key('Down', 'Enter');
      await terminal.waitFor('Esc back');
      assert(terminal.capture().includes(token), 'the complete sharing token is readable');
      terminal.key('Escape', 'Down', 'Enter');
      await terminal.waitFor('Disconnect all shared terminals');
      terminal.key('y');
      await terminal.waitFor('Allow connections to this computer');
      for (let i = 0; i < 30 && status().token === token; i++) await sleep(100);
      assert(status().token !== token, 'confirming reset must rotate the token');
      const previousToken = status().token;
      terminal.key('Home', 'Enter');
      await terminal.waitFor('off');
      assert.equal(status().enabled, false);
      terminal.key('End', 'Enter');
      await terminal.waitFor('Disconnect all shared terminals');
      terminal.key('y');
      await terminal.waitFor('Allow connections to this computer');
      assert.equal(status().enabled, false);
      terminal.key('Home', 'Enter');
      await terminal.waitFor('DIM1-');
      assert(status().token && status().token !== previousToken, 'rotation while off replaces the token on next enable');
    },
  },
  {
    name: 'slash_palette_lists_curated_commands_and_settings',
    area: 'commands',
    title: 'The command palette keeps its curated order and opens a settings panel',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const terminal = await tui.launch([], {waitForComposer: true});

      terminal.send('/');
      await terminal.waitFor('Commands');
      await terminal.waitFor('/new');
      const palette = terminal.capture();
      assert(palette.includes('type filter'), 'the palette names its keyboard actions');
      assert(palette.indexOf('/new') < palette.indexOf('/resume'), 'the curated order puts /new first');
      assert(palette.indexOf('/resume') < palette.indexOf('/models'), 'the curated order follows /new');

      terminal.send('settings');
      await terminal.waitFor('/settings');
      terminal.key('Enter');
      await terminal.waitFor('Settings');
      await terminal.waitFor('Theme');
      await terminal.waitFor('Language');
      terminal.key('Escape');
      await terminal.waitForAbsent('Settings');

      terminal.send('/keys');
      terminal.key('Enter');
      await terminal.waitFor('Enter submit when idle', {history: true});
      await terminal.waitForAbsent('Commands');

      await exitWith(terminal);
    },
  },
  {
    name: 'palette_tab_completes_and_enter_runs_the_command',
    area: 'commands',
    title: 'Tab in the palette completes the command, never runs it, and Enter still runs it',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      for (const args of [[], ['--fullscreen']]) {
        const terminal = await tui.launch(args, {waitForComposer: true});

        terminal.send('/ke');
        terminal.key('Tab');
        await terminal.waitFor('/keys');
        terminal.send('x');
        await terminal.waitFor('keysx');
        assert(!terminal.capture({history: true}).includes('Enter submit when idle'), 'Tab completed without running it');
        terminal.key('C-c');
        await terminal.waitForAbsent('keysx');

        // Repeated Tab keeps completing the palette's selection and never
        // exits it, so the draft still takes the next typed character.
        terminal.send('/');
        for (let index = 0; index < 3; index += 1) terminal.key('Tab');
        terminal.send('x');
        await terminal.waitFor('newx', {describe: 'the repeated completion'});
        terminal.key('C-c');
        await terminal.waitForAbsent('newx');

        terminal.send('/ke');
        terminal.key('Tab');
        await terminal.waitFor('/keys');
        terminal.key('Enter');
        await terminal.waitFor('Enter submit when idle', {history: true});

        assert.equal(tui.model.requests.length, 0, 'completing a command reaches no model');
        await exitWith(terminal);
      }
    },
  },
  {
    name: 'streaming_status_reports_and_interrupts',
    area: 'runs',
    title: 'A streaming run reports its state and Escape interrupts it',
    async run(tui) {
      await tui.setup({
        respond: (request) =>
          (request.tools ?? []).length > 0 && userText(request).includes('slow')
            ? {text: 'slow answer. '.repeat(80), chunkSize: 8, chunkDelayMs: 120}
            : {text: ANSWER},
      });
      const terminal = await tui.launch([], {
        // The bar is suppressed inside tmux by default; turn it on to prove the
        // run draws it and takes it down again.
        env: {DIM_TERMINAL_PROGRESS: 'on'},
        waitForComposer: true,
      });
      const bar = (code) => `\u001b]9;4;${code}\u0007`;

      terminal.send('slow run');
      terminal.key('Enter');
      await terminal.waitFor('working…');
      await terminal.waitFor('esc interrupt');
      await terminal.waitForRaw((raw) => raw.includes('state=working'), {describe: 'the working status'});
      await terminal.waitForRaw((raw) => raw.includes(bar('3')), {describe: 'the working progress bar'});

      terminal.key('Escape');
      await terminal.waitFor('User interrupted the run.');
      await terminal.waitFor('stopped');
      await terminal.untilComposer();
      await terminal.waitForRaw((raw) => /state=(idle|error)/.test(afterLastWorking(raw)), {
        describe: 'a settled status reported after the interrupted run',
      });
      await terminal.waitForRaw((raw) => raw.includes(bar('0')), {describe: 'the removed progress bar'});

      await exitWith(terminal);
      const raw = terminal.raw();
      assert(raw.includes('\u001b]7501;state=clear'), 'leaving clears the status record');
      assert(raw.includes(bar('3')), 'the run reports its work through OSC 9;4');
      assert(raw.includes(bar('0')), 'settling and leaving take the progress bar down');
      const last = raw.lastIndexOf('\u001b]9;4;');
      assert.equal(raw.slice(last, last + bar('0').length), bar('0'), 'no progress bar remains after exit');
    },
  },
  {
    name: 'composer_borders_match_in_scrollback_and_fullscreen',
    area: 'runs',
    title: 'Scrollback and fullscreen share the same composer border layout at 100 and 60 columns',
    async run(tui) {
      const USAGE = /\d+(?:\.\d+)?[KM]?\s*\/\s*128K/;
      await tui.setup({
        respond: (request) => {
          if ((request.tools ?? []).length === 0) return {text: ANSWER};
          if (userText(request).includes('slow think')) {
            return {
              thinking: 'thinking about it '.repeat(60),
              text: 'Thinking done.',
              chunkSize: 4,
              chunkDelayMs: 120,
            };
          }
          return {text: ANSWER};
        },
      });

      const composerChrome = (terminal) => {
        const lines = terminal
          .capture({join: false})
          .split('\n')
          .map((line) => line.trimEnd())
          .filter(Boolean);
        const top = [...lines].reverse().find((line) => line.includes('╭') && line.includes('╮')) ?? '';
        const bottom = [...lines].reverse().find((line) => line.includes('╰') && line.includes('╯')) ?? '';
        const footer = [...lines].reverse().find((line) => line.includes('Workspace write')) ?? '';
        return {top, bottom, footer, lines};
      };

      const assertRunningChrome = (chrome, mode, cols) => {
        const ctx = `${mode} ${cols} cols:\n${chrome.lines.join('\n')}`;
        assert(chrome.top.includes('local / fixture-model'), `model on top border (${ctx})`);
        assert(!chrome.top.includes('thinking…'), `status not on top border (${ctx})`);
        assert(chrome.bottom.includes('thinking…'), `thinking status on bottom border (${ctx})`);
        assert(chrome.bottom.includes('esc interrupt'), `esc interrupt on bottom border (${ctx})`);
        assert(USAGE.test(chrome.bottom), `context usage on bottom border (${ctx})`);
        assert(chrome.footer.includes('Workspace write'), `permission preset on footer (${ctx})`);
        assert(!chrome.footer.includes('esc interrupt'), `no esc interrupt on footer (${ctx})`);
        assert(!chrome.footer.includes('local / fixture-model'), `no model on footer (${ctx})`);
        assert(!USAGE.test(chrome.footer), `no usage on footer (${ctx})`);
      };

      for (const [mode, args] of [
        ['scrollback', []],
        ['fullscreen', ['--fullscreen']],
      ]) {
        const terminal = await tui.launch(args, {cols: 100, rows: 24, waitForComposer: true});

        // Complete one fast turn so the Session exists and context usage is shown.
        await terminal.sendPrompt('seed usage');
        await terminal.waitFor(ANSWER);
        await terminal.waitFor(USAGE, {describe: `${mode} context usage reading`});

        const idle = composerChrome(terminal);
        assert(idle.top.includes('local / fixture-model'), `${mode} idle top border carries model:\n${idle.top}`);
        assert(!idle.top.includes('thinking…') && !idle.top.includes('working…'), `${mode} idle top has no status`);
        assert(!idle.bottom.includes('thinking…') && !idle.bottom.includes('esc interrupt'), `${mode} idle bottom has no run status`);
        assert(USAGE.test(idle.bottom), `${mode} idle bottom carries context usage:\n${idle.bottom}`);
        assert(!idle.footer.includes('local / fixture-model') && !USAGE.test(idle.footer), `${mode} idle footer keeps only permission:\n${idle.footer}`);

        if (mode === 'scrollback') {
          terminal.send('!');
          await terminal.waitFor('! shell mode');
          const shellChrome = composerChrome(terminal);
          assert(
            shellChrome.top.includes('! shell mode') && shellChrome.top.includes('local / fixture-model'),
            `shell mode keeps top-border title beside model:\n${shellChrome.top}`,
          );
          terminal.key('BSpace');
          await terminal.waitForAbsent('! shell mode');
        }

        terminal.send('slow think');
        terminal.key('Enter');
        await terminal.waitFor('thinking…');
        await terminal.waitFor('esc interrupt');
        assertRunningChrome(composerChrome(terminal), mode, 100);

        // Shrink to 60 columns while the slow-thinking turn is running.
        terminal.resize(60, 24);
        const deadline = Date.now() + 20_000;
        let narrow = null;
        for (;;) {
          const chrome = composerChrome(terminal);
          const widest = Math.max(0, ...chrome.lines.map((line) => line.length));
          if (
            chrome.top.trimEnd().endsWith('╮') &&
            chrome.bottom.trimEnd().endsWith('╯') &&
            chrome.top.includes('local / fixture-model') &&
            chrome.bottom.includes('thinking…') &&
            chrome.bottom.includes('esc interrupt') &&
            USAGE.test(chrome.bottom)
          ) {
            narrow = chrome;
            break;
          }
          if (Date.now() >= deadline) {
            throw new Error(
              `${mode} did not repaint unified composer at 60 cols within 20000ms (widest ${widest})\n${chrome.lines.join('\n')}`,
            );
          }
          await sleep(100);
        }
        assertRunningChrome(narrow, mode, 60);

        terminal.key('Escape');
        await terminal.waitFor('stopped');
        await terminal.untilComposer();
        await exitWith(terminal);
      }
    },
  },
  {
    name: 'queue_holds_steers_and_deletes',
    area: 'input-queue',
    title: 'Queued input is held during a run, deleted, and steered through the panel',
    async run(tui) {
      await tui.setup({
        config: {tuiDefaultSendMode: 'queue'},
        respond: (request) =>
          (request.tools ?? []).length > 0 && userText(request).includes('slow')
            ? {text: 'slow answer. '.repeat(120), chunkSize: 8, chunkDelayMs: 150}
            : {text: ANSWER},
      });
      const terminal = await tui.launch([], {waitForComposer: true});

      terminal.send('slow run');
      terminal.key('Enter');
      await terminal.waitFor('esc interrupt');

      terminal.send('first follow-up');
      terminal.key('Enter');
      await terminal.waitFor('Queue (1)');
      await terminal.waitFor('Held · first follow-up');
      terminal.send('second follow-up');
      terminal.key('Enter');
      await terminal.waitFor('Queue (2)');
      await terminal.waitFor('Held · second follow-up');

      terminal.send('/queue');
      terminal.key('Enter');
      await terminal.waitFor('Held · awaiting application', {describe: 'the queue panel'});
      // The panel replaces the queue strip, so its own row text is the
      // deletion's barrier: the strip's `Held · …` form is already gone.
      terminal.key('x');
      await terminal.waitForAbsent('first follow-up');
      assert(
        terminal.capture().includes('second follow-up'),
        'the deletion leaves the other held row selected',
      );
      terminal.key('Enter');
      await terminal.waitFor('No queued messages.', {describe: 'the emptied queue'});
      assert(!terminal.capture().includes('Enter steer'), 'an empty queue offers no steer or delete');
      await terminal.waitFor('┃ second follow-up', {history: true});
      assert(
        !terminal.capture({history: true}).includes('┃ first follow-up'),
        'a deleted message never enters the transcript',
      );
      terminal.key('Escape');
      await terminal.waitForAbsent('Queue');

      await terminal.waitForAbsent('esc interrupt', {timeout: 40_000});
      await exitWith(terminal);
    },
  },
  {
    name: 'approval_panel_allows_one_command',
    area: 'approvals',
    title: 'The permission panel allows one command and its output returns to the model',
    async run(tui) {
      await tui.setup({
        respond: (request) => {
          if ((request.tools ?? []).length === 0) return {text: ANSWER};
          if (lastMessage(request).role === 'tool') return {text: 'The command printed fixture-ok.'};
          return {toolCalls: [{name: 'exec', arguments: {command: 'echo fixture-ok'}}]};
        },
      });
      const terminal = await tui.launch([], {waitForComposer: true});

      terminal.send('run it');
      terminal.key('Enter');
      await terminal.waitFor('Permission required');
      await terminal.waitFor('echo fixture-ok');
      await terminal.waitFor('Allow once');
      await terminal.waitForRaw((raw) => raw.includes('state=blocked:kind=permission'), {
        describe: 'the blocked status',
      });

      terminal.key('y');
      await terminal.waitFor('Ran 1 command');
      await terminal.waitFor('The command printed fixture-ok.');
      await terminal.untilComposer();

      const turn = await waitForRequest(tui.model, (request) => lastMessage(request).role === 'tool', {
        describe: 'the tool result request',
      });
      assert(
        String(lastMessage(turn.body).content).includes('fixture-ok'),
        'the tool output reaches the model',
      );

      await exitWith(terminal);
    },
  },
  ...['scrollback', 'fullscreen'].map((mode) => ({
    name: `${mode}_skill_draft_at_home_and_over_a_question`,
    area: 'skills',
    title: 'A Skill completes at Home and keeps the task draft in focus over a question',
    async run(tui) {
      await tui.setup({
        files: {
          '.agents/skills/review-demo/SKILL.md': '---\nname: review-demo\ndescription: Review a draft\n---\nUse the fixture reviewer.',
        },
        respond: (request) => {
          if (lastMessage(request).role === 'user' && userText(request).includes('ask before review')) {
            const question = {header: 'Review', question: 'Which draft?', options: [{label: 'First'}, {label: 'Second'}]};
            return {toolCalls: [{name: 'ask_user', arguments: {questions: [question]}}]};
          }
          return {text: ANSWER};
        },
      });
      const args = ['--cwd', path.join(tui.root, 'workspace')];
      if (mode === 'fullscreen') args.push('--fullscreen');
      const terminal = await tui.launch(args, {waitForComposer: true});
      terminal.send('/review-de');
      await terminal.waitFor('Tab / Enter complete');
      terminal.key('Enter');
      terminal.send('ask before review');
      await terminal.waitFor('/skill:review-demo ask before review');
      assert.equal(turnRequests(tui.model).length, 0);
      terminal.key('Enter');
      await terminal.waitFor('Which draft?');
      for (const finish of ['Escape', 'Enter']) {
        terminal.send('/skills');
        terminal.key('Enter');
        await terminal.waitFor('Enter use');
        terminal.key('Enter');
        terminal.key('Home');
        terminal.send('check the draft ');
        await terminal.waitFor('check the draft /skill:review-demo');
        terminal.key('C-o');
        await terminal.waitFor('/new');
        terminal.key('Escape');
        await terminal.waitFor('check the draft /skill:review-demo');
        terminal.send('after restore');
        await terminal.waitFor('check the draft /skill:review-demo after restore');
        assert.equal(turnRequests(tui.model).length, 1);
        terminal.key(finish);
        await terminal.waitFor('Which draft?');
      }
      terminal.key('Down', 'Enter');
      await terminal.waitFor('Review your answers');
      terminal.key('Enter');
      await waitForRequest(tui.model, (request) => request.messages?.some((m) => m.role === 'tool' && JSON.stringify(m.content).includes('Second')));
      await waitForRequest(tui.model, (request) => request.messages?.some((m) => m.role === 'user' && JSON.stringify(m.content).includes('check the draft /skill:review-demo after restore')));
      await terminal.waitFor(ANSWER);
      terminal.send('/new');
      terminal.key('Enter');
      await terminal.waitFor('─ new session ─');
      await terminal.untilComposer();
      terminal.send('/review-de');
      await terminal.waitFor('Tab / Enter complete');
      terminal.key('Tab');
      terminal.send('back at home');
      await terminal.waitFor('/skill:review-demo back at home');
    },
  })),
  ...['scrollback', 'fullscreen'].map((mode) => ({
    name: `${mode}_question_arrives_during_completion`,
    area: 'skills',
    title: 'A later question does not take the keys from a command or Skill draft',
    async run(tui) {
      let gate;
      let turn;
      await tui.setup({
        files: {'.agents/skills/review-demo/SKILL.md': '---\nname: review-demo\ndescription: Review a draft\n---\nReview.'},
        respond: async (request) => {
          if (!request.tools?.length) return {text: ANSWER};
          if (lastMessage(request).role === 'tool') return {text: `Recorded ${turn}`};
          await gate.promise;
          return {toolCalls: [{name: 'ask_user', arguments: {questions: [{header: 'Pick', question: `Choose for ${turn}`, options: [{label: 'One'}, {label: 'Two'}]}]}}]};
        },
      });
      const args = ['--cwd', path.join(tui.root, 'workspace')];
      if (mode === 'fullscreen') args.push('--fullscreen');
      const terminal = await tui.launch(args, {waitForComposer: true});
      for (turn of ['command', 'skill row', 'skill draft']) {
        gate = Promise.withResolvers();
        terminal.send(`ask about ${turn}`);
        terminal.key('Enter');
        await waitForRequest(tui.model, (r) => r.tools?.length && userText(r).includes(`ask about ${turn}`));
        terminal.send(turn === 'command' ? '/goal' : '/review-de');
        await terminal.waitFor(turn === 'command' ? 'follow or change' : 'Tab / Enter complete');
        if (turn === 'skill draft') {
          terminal.key('Enter', 'Home');
          terminal.send('before ');
          await terminal.waitFor('before /skill:review-demo');
        }
        gate.resolve();
        await terminal.waitFor('answer required');
        if (turn !== 'skill draft') terminal.key(turn === 'command' ? 'Tab' : 'Enter');
        if (turn === 'skill row') terminal.key('Home');
        terminal.send('keep typing');
        await terminal.waitFor({command: '/goal keep typing', 'skill row': 'keep typing/skill:review-demo', 'skill draft': 'before keep typing/skill:review-demo'}[turn]);
        terminal.key('Escape');
        await terminal.waitFor(`Choose for ${turn}`);
        terminal.key('Down', 'Enter', 'Enter');
        await terminal.waitFor(`Recorded ${turn}`);
        assert(userText(turnRequests(tui.model).at(-1).body).includes('Two'));
      }
    },
  })),
  {
    name: 'question_panel_answer_reaches_the_next_request',
    area: 'approvals',
    title: 'A question is answered in the panel and the answer reaches the model',
    async run(tui) {
      await tui.setup({
        respond: (request) => {
          if ((request.tools ?? []).length === 0) return {text: ANSWER};
          if (lastMessage(request).role === 'tool') return {text: 'The answer was recorded.'};
          return {
            toolCalls: [
              {
                name: 'ask_user',
                arguments: {
                  questions: [
                    {
                      header: 'Fixture choice',
                      question: 'Which fixture should I use?',
                      options: [{label: 'Option A'}, {label: 'Option B'}],
                    },
                  ],
                },
              },
            ],
          };
        },
      });
      const terminal = await tui.launch([], {waitForComposer: true});

      terminal.send('ask me');
      terminal.key('Enter');
      await terminal.waitFor('Which fixture should I use?');
      await terminal.waitFor('Option A');
      await terminal.waitForRaw((raw) => raw.includes('state=blocked:kind=question'), {
        describe: 'the blocked question status',
      });
      // Opening `/` over a question restores the composer with `answer required` on its bottom border.
      terminal.send('/');
      await terminal.waitFor('answer required');
      terminal.key('Escape');
      await terminal.waitFor('Option A');

      terminal.key('Down');
      terminal.key('Enter');
      await terminal.waitFor('Review your answers');
      terminal.key('Enter');
      await terminal.waitFor('The answer was recorded.');
      await terminal.untilComposer();

      const turn = await waitForRequest(tui.model, (request) => lastMessage(request).role === 'tool', {
        describe: 'the answered question request',
      });
      assert(
        String(lastMessage(turn.body).content).includes('Option B'),
        `the chosen answer reaches the model: ${lastMessage(turn.body).content}`,
      );

      await exitWith(terminal);
    },
  },
  {
    name: 'background_task_row_and_tasks_panel',
    area: 'tasks',
    title: 'A background command shows a task row and the tasks panel can stop it',
    async run(tui) {
      await tui.setup({
        respond: (request) => {
          if ((request.tools ?? []).length === 0) return {text: ANSWER};
          if (lastMessage(request).role === 'tool') return {text: 'Started the fixture task.'};
          return {
            toolCalls: [
              {
                name: 'exec',
                arguments: {command: 'sleep 30', runInBackground: true, title: 'Fixture background work'},
              },
            ],
          };
        },
      });
      const terminal = await tui.launch([], {waitForComposer: true});

      terminal.send('task please');
      terminal.key('Enter');
      await terminal.waitFor('Permission required');
      terminal.key('a');
      await terminal.waitFor('Ran 1 command');
      await terminal.waitFor('Fixture background work');
      await terminal.waitFor('1 in background');

      terminal.send('/tasks');
      terminal.key('Enter');
      await terminal.waitFor('Background tasks');
      await terminal.waitFor('command · Running');
      terminal.key('x');
      await terminal.waitForAbsent('1 in background');
      terminal.key('Escape');
      await terminal.waitForAbsent('Background tasks');
      await terminal.untilComposer();

      await exitWith(terminal);
    },
  },
  {
    name: 'task_strip_stays_and_switches_the_open_detail',
    area: 'tasks',
    title: 'The task strip stays drawn under an open task detail; in fullscreen a row click switches the detail and Esc closes it, keeping the draft',
    async run(tui) {
      const ticker = (name) => `for i in $(seq 1 60); do echo tick-${name}-$i; sleep 1; done`;
      await tui.setup({
        respond: (request) => {
          if ((request.tools ?? []).length === 0) return {text: ANSWER};
          if (lastMessage(request).role === 'tool') return {text: 'Started both tickers.'};
          return {
            toolCalls: [
              {name: 'exec', arguments: {command: ticker('a'), runInBackground: true, title: 'Ticker A'}},
              {name: 'exec', arguments: {command: ticker('b'), runInBackground: true, title: 'Ticker B'}},
            ],
          };
        },
      });
      const stripRows = (text) => ['A', 'B'].every((name) => new RegExp(`◐ Ticker ${name}\\s+\\d+s`).test(text));
      const start = async (terminal) => {
        terminal.send('start two tickers');
        terminal.key('Enter');
        const pending = new Set(['a', 'b']);
        while (pending.size > 0) {
          const names = [...pending].join('');
          const screen = await terminal.waitFor(new RegExp(`Permission required[\\s\\S]*echo tick-[${names}]-`));
          const panel = screen.slice(screen.lastIndexOf('Permission required'));
          pending.delete(panel.match(new RegExp(`echo tick-([${names}])-`))[1]);
          terminal.key('y');
        }
        await terminal.waitFor('Started both tickers.');
        await terminal.waitFor('2 in background');
        assert(stripRows(terminal.capture()), `both task rows drawn\n${terminal.capture()}`);
      };

      const full = await tui.launch(['--fullscreen'], {waitForComposer: true, cols: 140, rows: 40});
      await start(full);
      const cellOf = (text) => {
        const lines = full.capture({join: false}).split('\n');
        const index = lines.findLastIndex((line) => line.includes(text));
        assert(index !== -1, `pane missing ${text}\n${lines.join('\n')}`);
        return {col: lines[index].indexOf(text) + 1, row: index + 1};
      };
      const click = ({col, row}) => {
        full.bytes(Buffer.from(`\u001b[<0;${col};${row}M`, 'utf8'));
        full.bytes(Buffer.from(`\u001b[<0;${col};${row}m`, 'utf8'));
      };
      full.send('keep this draft');
      await full.waitFor('keep this draft');

      click(cellOf('◐ Ticker A'));
      await full.waitFor('╭─ Ticker A');
      await full.waitFor('x stop command · Esc close');
      await full.waitFor(/tick-a-\d+/);
      const first = Number(full.capture().match(/tick-a-(\d+)/g).at(-1).split('-').at(-1));
      await full.waitFor(new RegExp(`tick-a-${first + 1}\\b`), {describe: 'the next live output line'});
      assert(stripRows(full.capture()), `the strip stays under the detail\n${full.capture()}`);

      click(cellOf('◐ Ticker B'));
      await full.waitFor('╭─ Ticker B');
      await full.waitFor(/tick-b-\d+/);
      assert(!full.capture().includes('╭─ Ticker A'), `the click replaced the detail\n${full.capture()}`);
      assert(stripRows(full.capture()), `the strip stays after the switch\n${full.capture()}`);

      full.key('Escape');
      await full.waitForAbsent('Esc close');
      await full.waitFor('keep this draft');
      assert(stripRows(full.capture()), `the strip is drawn with the composer\n${full.capture()}`);
      for (let index = 0; index < 'keep this draft'.length; index += 1) full.key('BSpace');
      await full.untilComposer();
      await exitWith(full);

      const scroll = await tui.launch([], {waitForComposer: true});
      await start(scroll);
      scroll.send('/tasks');
      scroll.key('Enter');
      await scroll.waitFor('Background tasks');
      assert(stripRows(scroll.capture()), `the strip stays under the list\n${scroll.capture()}`);
      scroll.key('Enter');
      await scroll.waitFor('x stop command · Esc back');
      await scroll.waitFor(/tick-[ab]-\d+/);
      assert(stripRows(scroll.capture()), `the strip stays under the detail\n${scroll.capture()}`);
      scroll.key('Escape');
      await scroll.waitFor('Background tasks');
      scroll.key('Escape');
      await scroll.waitForAbsent('Background tasks');
      await scroll.untilComposer();
      await exitWith(scroll);
    },
  },
  {
    name: 'member_conversation_and_stop_all',
    area: 'members',
    title: 'A delegated member draws a task row, opens its conversation and stop-all settles it',
    async run(tui) {
      await tui.setup({
        respond: (request) => {
          const tools = (request.tools ?? []).length > 0;
          const last = lastMessage(request);
          const text = userText(request);
          if (text.includes('MEMBER-TASK')) {
            return {text: 'member working line. '.repeat(60), chunkSize: 6, chunkDelayMs: 250};
          }
          if (!tools) return {text: ANSWER};
          if (text.includes('resultId')) return {text: 'Noted the cancellation.'};
          if (last.role === 'tool') return {text: 'The member was started.'};
          return {
            toolCalls: [
              {
                name: 'agent',
                arguments: {
                  action: 'request',
                  title: 'Fixture member task',
                  prompt: 'MEMBER-TASK: inspect the fixture workspace',
                  profile: {
                    name: 'fixture-member',
                    description: 'A fixture member',
                    systemPrompt: 'You are the fixture member.',
                    tools: [],
                  },
                },
              },
            ],
          };
        },
      });
      const terminal = await tui.launch([], {waitForComposer: true});

      terminal.send('delegate-member-task');
      terminal.key('Enter');
      await terminal.waitFor('Permission required');
      await terminal.waitFor('Tool `agent` requires approval');
      terminal.key('y');
      await terminal.waitFor('The member was started.');
      await terminal.waitFor('Fixture member task');
      await terminal.waitFor('1 in background');

      terminal.send('/tasks');
      terminal.key('Enter');
      await terminal.waitFor('Background tasks');
      await terminal.waitFor('agent · fixture-member · Running');
      terminal.key('Enter');
      await terminal.waitFor('Member conversation');
      await terminal.waitFor('x interrupt member · Esc back');
      await terminal.waitFor('MEMBER-TASK: inspect the fixture workspace');
      await terminal.waitFor('Member Session:');
      terminal.key('Escape');
      await terminal.waitFor('Background tasks');
      terminal.key('C-x');
      await terminal.waitFor('attempted 1, errors 0, unavailable');
      await terminal.waitFor('agent cancelled: stopped by the user');
      terminal.key('Escape');
      await terminal.waitForAbsent('Background tasks');
      await terminal.waitFor('Noted the cancellation.', {timeout: 30_000});
      await terminal.waitForAbsent('1 in background');
      await exitWith(terminal);
    },
  },
  {
    name: 'chatgpt_plan_opens_at_connect',
    area: 'providers',
    title: 'ChatGPT plan has its own sign-in entry in the native provider panel',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const terminal = await tui.launch([], {waitForComposer: true, server: true});
      terminal.send('/connect');
      terminal.key('Enter');
      terminal.send('OpenAI ChatGPT');
      await terminal.waitFor(/OpenAI ChatGPT\s+Sign in/);
      terminal.key('Enter');
      await terminal.waitFor('Continue with ChatGPT');
      await terminal.waitFor('Dim stores your ChatGPT connection on this computer.');
      terminal.key('Escape');
      await terminal.waitFor(/OpenAI ChatGPT\s+Sign in/);
      terminal.key('Escape');
      await terminal.waitFor('302.AI');
      terminal.send('OpenAI Codex');
      const codexSearch = await terminal.waitFor('> OpenAI Codex');
      assert.equal(codexSearch.match(/OpenAI Codex/g)?.length, 1, 'Only the search input names Codex');
      terminal.key('Escape');
      await terminal.waitFor('302.AI');
      terminal.key('Escape');
      await terminal.waitForAbsent('Enter open');
      await exitWith(terminal);
    },
  },
  {
    name: 'provider_panel_adds_and_keeps_a_custom_provider',
    area: 'providers',
    title: 'The provider panel adds a custom provider against the fixture on the production App Server and keeps it after a restart',
    async run(tui) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const open = () => tui.launch([], {waitForComposer: true, server: true});
      const terminal = await open();

      terminal.send('/connect');
      terminal.key('Enter');
      terminal.send('Add custom');
      await terminal.waitFor('Add custom provider');
      terminal.key('Enter');
      await terminal.waitFor('Name:');
      terminal.send('Fixture provider');
      terminal.key('Enter');
      terminal.send(tui.model.url);
      terminal.key('Enter');
      terminal.send('fixture-key');
      terminal.key('Enter');
      // The prefix and Test + apply take an empty answer each.
      terminal.key('Enter');
      terminal.key('Enter');
      await terminal.waitFor('Disable provider', {timeout: 40_000});
      assert(terminal.capture().includes('Added Fixture provider'), 'the create was applied');
      terminal.key('Escape');
      await terminal.waitFor('Enter open', {describe: 'the providers list again'});
      await terminal.waitForAbsent('Disable provider');
      terminal.key('Escape');
      await terminal.waitFor('302.AI', {describe: 'the cleared provider filter'});
      terminal.key('Escape');
      await terminal.waitForAbsent('Providers');

      await terminal.sendPrompt('a turn through the added provider');
      await terminal.waitFor(ANSWER);
      const turn = await waitForRequest(
        tui.model,
        (request) =>
          (request.tools ?? []).length > 0 && userText(request).includes('a turn through the added provider'),
        {describe: 'the turn through the added provider'},
      );
      assert.equal(turn.authorization, 'Bearer fixture-key', 'the turn used the added provider');
      await exitWith(terminal);

      tui.stopDaemon();
      const restarted = await open();
      restarted.send('/connect');
      restarted.key('Enter');
      restarted.send('Fixture');
      await restarted.waitFor('Fixture provider');
      await restarted.waitFor('ready');
      restarted.key('Escape');
      restarted.key('Escape');
      await restarted.waitForAbsent('Providers');
      await exitWith(restarted);
    },
  },
  {
    name: 'prompt_suggestion_offers_and_takes_the_next_message',
    area: 'suggestions',
    title: 'A completed turn offers the next message; Tab takes it in scrollback, Tab keeps the focus in fullscreen, Right takes it there, and Escape withdraws it',
    async run(tui) {
      const OFFER = /run the fixture suite #(\d+)/;
      const prediction = (request) => userText(request).startsWith('You predict the next message');
      let next = 0;
      await tui.setup({
        respond: (request) =>
          prediction(request) ? {text: `run the fixture suite #${(next += 1)}`} : {text: ANSWER},
        documents: {'rust/prompt-suggestion.json': {enabled: true, maxChars: 80}},
      });
      const predictions = () => tui.model.requests.filter((entry) => prediction(entry.body)).length;
      const drawn = (terminal) => OFFER.exec(terminal.capture())?.[0];
      const takeTurn = async (terminal, text) => {
        await terminal.sendPrompt(text);
        await terminal.waitFor(ANSWER);
        const deadline = Date.now() + 30_000;
        for (;;) {
          const offer = drawn(terminal);
          if (offer) return offer;
          if (Date.now() >= deadline) {
            throw new Error(`waited 30000ms for a suggestion offer\n${terminal.capture()}`);
          }
          await sleep(100);
        }
      };
      const clears = async (terminal, offer) => {
        terminal.key('C-c');
        await terminal.waitFor('Press Ctrl+C again to exit', {describe: 'the cleared draft'});
        assert(!terminal.capture().includes(offer), 'the cleared draft leaves no offer behind');
      };

      for (const args of [[], ['--fullscreen']]) {
        const scrollback = args.length === 0;
        const terminal = await tui.launch(args, {waitForComposer: true});

        const first = await takeTurn(terminal, 'offer me the next message');
        terminal.key('Tab');
        if (scrollback) {
          terminal.send('x');
          await terminal.waitFor(`${first}x`, {describe: 'the offer Tab completed into the draft'});
        } else {
          await terminal.waitFor(first, {describe: 'the offer Tab left alone'});
          terminal.key('Enter');
          const sent = await settleWithout(
            tui.model,
            (request) => userText(request) === first,
            {describe: 'the offer Tab and Enter did not send'},
          );
          assert(sent, 'fullscreen Tab kept the focus and Enter did not send the offer');
          terminal.key('Right');
          terminal.send('z');
          await terminal.waitFor(`${first}z`, {describe: 'the offer Right put in the draft'});
        }
        await clears(terminal, first);

        const second = await takeTurn(terminal, 'offer it again');
        assert.notEqual(second, first, 'the next completed turn draws a different offer');
        terminal.key('Right');
        assert(
          !tui.model.requests.some((entry) => userText(entry.body) === second),
          'taking the offer fills the draft without sending it',
        );
        terminal.send('z');
        await terminal.waitFor(`${second}z`, {describe: 'the offer Right put in the draft'});
        await clears(terminal, second);

        const third = await takeTurn(terminal, 'offer it one more time');
        assert.notEqual(third, second, 'each completed turn draws its own offer');
        const asked = predictions();
        terminal.key('Escape');
        await terminal.waitForAbsent(third);
        assert.equal(predictions() - asked, 0, 'Escape withdraws the offer without asking again');
        await exitWith(terminal);
      }
    },
  },
  {
    name: 'suggestion_asks_nothing_while_a_draft_exists',
    area: 'suggestions',
    title: 'A draft withdraws the offer and asks for no other suggestion until the next turn completes',
    async run(tui) {
      const prediction = (request) => userText(request).startsWith('You predict the next message');
      let next = 0;
      await tui.setup({
        respond: (request) => (prediction(request) ? {text: `draft offer #${(next += 1)}`} : {text: ANSWER}),
        documents: {'rust/prompt-suggestion.json': {enabled: true, maxChars: 80}},
      });
      const predictions = () => tui.model.requests.filter((entry) => prediction(entry.body)).length;
      const terminal = await tui.launch([], {waitForComposer: true});

      await terminal.sendPrompt('offer me something');
      await terminal.waitFor(ANSWER);
      await terminal.waitFor('draft offer #1', {timeout: 30_000});
      assert.equal(predictions(), 1, 'the completed turn asked once');

      terminal.send('a draft of my own');
      await terminal.waitFor('a draft of my own');
      await terminal.waitForAbsent('draft offer #1');
      assert(
        await settleWithout(tui.model, (request) => prediction(request), {after: predictions()}),
        'the draft asked for no other suggestion',
      );
      assert.equal(predictions(), 1, 'still one suggestion request while the draft exists');

      terminal.key('Enter');
      await terminal.waitFor(ANSWER);
      await terminal.waitFor('draft offer #2', {timeout: 30_000});
      assert.equal(predictions(), 2, 'the next completed turn asked again');
      await exitWith(terminal);
    },
  },
  {
    name: 'daemon_build_mismatch_notice_scrollback',
    area: 'daemon',
    title: 'Scrollback says the kept daemon is another build of this version, on Home and on a direct resume, names the command that stops it, stays silent for a matching build, and calls an unmarked daemon unverified',
    async run(tui) {
      const matched = await otherBuildNotice(tui);
      await exitWith(matched);
      await unverifiedBuildNotice(tui);
    },
  },
  {
    name: 'daemon_build_mismatch_notice_fullscreen',
    area: 'daemon',
    title: 'Fullscreen shows the same other-build notice on Home and on a direct resume, and an explicit --server never gets it',
    async run(tui) {
      const matched = await otherBuildNotice(tui, {fullscreen: true});
      await exitWith(matched);
      // The managed server is a separate artifact with a product profile of its
      // own, so the fixture daemon leaves first.
      tui.stopDaemon();
      const served = await tui.launch([], {
        waitForComposer: true,
        server: true,
        env: {DIM_TEST_BUILD_ID: 'build-2026-11-18'},
      });
      const screen = served.capture({history: true});
      assert(
        !screen.includes(OTHER_BUILD_NOTICE) && !screen.includes('could not be verified'),
        `an explicit --server gets no embedded-build notice: ${screen}`,
      );
      await exitWith(served);
    },
  },
  {
    name: 'daemon_stop_reports_disconnect_and_resume_replays_history',
    area: 'daemon',
    title: 'A stopped daemon keeps the draft in the same process, names the resume command, and the resumed session replays history',
    async run(tui) {
      const RESUMED_ANSWER = 'The resumed session answered the new prompt, not a replay.';
      await tui.setup({
        respond: (request) =>
          (request.tools ?? []).length > 0 && userText(request).includes('after restart')
            ? {text: RESUMED_ANSWER}
            : {text: ANSWER},
      });
      const terminal = await tui.launch([], {waitForComposer: true});

      await terminal.sendPrompt('remember this');
      await terminal.waitFor(ANSWER);
      terminal.send('unfinished follow-up');
      await terminal.waitFor('unfinished follow-up');

      tui.stopDaemon();
      await terminal.waitFor('Disconnected; draft kept');
      assert(
        terminal.capture().includes('unfinished follow-up'),
        'the disconnected interface still shows the draft it kept',
      );
      const screen = await terminal.waitFor('dimcode --resume');
      const resume = /dimcode --resume ([0-9a-f-]{36})/.exec(screen);
      assert(resume, `the notice names the resume command: ${screen}`);
      assert.equal((await terminal.waitForExit()).status, 0);
      assert(restoreTail(terminal.raw()).includes('\u001b[?25h'), 'the terminal is restored when the daemon leaves');

      const resumed = await tui.launch(['--resume', resume[1]], {waitForComposer: true});
      await resumed.waitFor('remember this', {history: true});
      await resumed.waitFor(ANSWER, {history: true});
      await resumed.sendPrompt('after restart');
      // The replayed answer is already on screen, so the new turn is proven by
      // an answer only the new prompt can produce.
      await resumed.waitFor(RESUMED_ANSWER, {describe: 'the answer to the new prompt'});
      assert.equal(lastTurnText(tui.model), 'after restart', 'the resumed session sent the new prompt, not a replay');

      await exitWith(resumed);
    },
  },
  {
    name: 'npm_launcher_streams_and_passes_interrupts',
    area: 'launcher',
    title: 'The npm launcher runs the native TUI and forwards terminal input and exit status',
    async run(tui, {repository} = {}) {
      await tui.setup({respond: () => ({text: ANSWER})});
      const wrapper = path.join(tui.root, 'wrapper');
      const platform = `${process.platform}-${process.arch}`;
      const packageDir = path.join(wrapper, 'node_modules', `dimcode-${platform}`, 'bin');
      fs.mkdirSync(packageDir, {recursive: true});
      fs.writeFileSync(
        path.join(wrapper, 'node_modules', `dimcode-${platform}`, 'package.json'),
        JSON.stringify({name: `dimcode-${platform}`}),
      );
      // The case's home lives on the system temporary directory, which need not
      // share a filesystem with the checkout or `--binary`, so the platform
      // package gets a copy that keeps the executable mode.
      const packaged = path.join(packageDir, 'dimcode');
      fs.copyFileSync(tui.binary, packaged);
      fs.chmodSync(packaged, 0o755);
      const launcherDir = path.join(wrapper, 'bin');
      fs.mkdirSync(launcherDir, {recursive: true});
      fs.copyFileSync(path.join(repository, 'npm', 'bin', 'dimcode.mjs'), path.join(launcherDir, 'dimcode.mjs'));
      fs.copyFileSync(path.join(repository, 'npm', 'package.json'), path.join(wrapper, 'package.json'));

      const terminal = await tui.launch(['--home', tui.home, '--model-config', tui.modelFile], {
        command: ['node', path.join(launcherDir, 'dimcode.mjs')],
        waitForComposer: true,
      });

      await terminal.sendPrompt('launcher prompt');
      await terminal.waitFor(ANSWER);
      assert.equal(lastTurnText(tui.model), 'launcher prompt');

      // The pre-interrupt barrier is the product's own settled status; the
      // status row alone may not have been drawn yet.
      await terminal.waitForRaw((raw) => /state=(done|idle|error)/.test(afterLastWorking(raw)), {
        describe: 'the settled run before the interrupt',
      });
      terminal.key('C-c');
      await terminal.waitFor(/Press Ctrl\+C again to exit|Interrupt requested; Ctrl\+C again to exit/, {
        describe: 'the interrupt confirmation',
      });
      assert(terminal.alive(), 'the first interrupt keeps the interface');
      terminal.key('C-c');
      const status = await terminal.waitForExit();
      assert.equal(status.status, 130, 'a second interrupt ends the CLI with the interrupt status');
      assert(restoreTail(terminal.raw()).includes('\u001b[?25h'), 'the launcher leaves the terminal restored');
    },
  },
];

export {scenarios};
