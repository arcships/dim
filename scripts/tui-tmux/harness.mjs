import {execFileSync, spawn} from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {modelConfig, startModelFixture} from './model.mjs';

const DEFAULT_PATH = '/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin';
const TMUX_TIMEOUT_MS = 20_000;
const POLL_MS = 100;

export const DEFAULT_STEP_TIMEOUT_MS = 25_000;

/// The tmux server's socket lives in a short path: a unix socket name is
/// bounded, and the temporary home below it may be long.
export function socketRoot() {
  return fs.existsSync('/tmp') ? '/tmp' : fs.realpathSync(os.tmpdir());
}

function quote(value) {
  return `'${String(value).replace(/'/g, "'\\''")}'`;
}

function shellCommand(environment, argv) {
  const assignments = Object.entries(environment).map(([key, value]) => `${key}=${value}`);
  return ['env', '-i', ...assignments, ...argv].map(quote).join(' ');
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitUntil(predicate, {timeout = DEFAULT_STEP_TIMEOUT_MS, interval = POLL_MS, describe = 'condition'} = {}) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await predicate();
    if (value) return value;
    if (Date.now() >= deadline) throw new Error(`waited ${timeout}ms for ${describe}`);
    await sleep(interval);
  }
}

export function tmuxVersion(tmux = 'tmux') {
  const printed = execFileSync(tmux, ['-V'], {encoding: 'utf8'}).trim();
  const match = /^tmux\s+(\d+)\.(\d+)/.exec(printed);
  if (!match) throw new Error(`cannot read the tmux version from ${JSON.stringify(printed)}`);
  return {printed, major: Number(match[1]), minor: Number(match[2])};
}

/// The product's scrollback reflow clears with `CSI 3J` and shows the cursor
/// (`CSI ?25h`) when the frame for the new size is complete. A cursor report
/// before the clear belongs to the outgoing frame and does not count.
export function reflowComplete(segment) {
  const cleared = segment.lastIndexOf('\u001b[3J');
  return cleared !== -1 && segment.indexOf('\u001b[?25h', cleared) !== -1;
}

/// One scenario's disposable state: a product home, a user home, a model
/// fixture, a dedicated tmux server and socket, and its raw output tap.
export class TmuxCase {
  constructor({name, root, binary, tmux = 'tmux', keep = false, verbose = false}) {
    this.name = name;
    this.root = root;
    this.binary = binary;
    this.tmux = tmux;
    this.keep = keep;
    this.verbose = verbose;
    this.home = path.join(root, 'home');
    this.userHome = path.join(root, 'user-home');
    this.socket = path.join(root, 'tmux.sock');
    this.appServer = path.join(path.dirname(binary), 'dim-app-server');
    this.requestsFile = path.join(root, 'requests.jsonl');
    this.interactionsFile = path.join(root, 'interactions.log');
    this.sessions = [];
    this.closed = false;
  }

  /// `documents` writes product-home settings documents (for example
  /// `hooks.json` or `prompt-suggestion.json`), which is how a case turns on a
  /// daemon feature the production App Server reads from its home.
  async setup({respond, config = {}, files = {}, documents = {}}) {
    fs.mkdirSync(path.join(this.home, 'rust'), {recursive: true, mode: 0o700});
    fs.chmodSync(path.join(this.home, 'rust'), 0o700);
    fs.writeFileSync(path.join(this.home, 'config.json'), JSON.stringify({locale: 'en', ...config}));
    for (const [name, document] of Object.entries(documents)) {
      fs.writeFileSync(path.join(this.home, name), JSON.stringify(document, null, 2));
    }
    fs.mkdirSync(this.userHome, {recursive: true});
    fs.mkdirSync(path.join(this.root, 'tmp'), {recursive: true});
    fs.mkdirSync(path.join(this.root, 'workspace'), {recursive: true});
    for (const [name, content] of Object.entries(files)) {
      const file = path.join(this.root, 'workspace', name);
      fs.mkdirSync(path.dirname(file), {recursive: true});
      fs.writeFileSync(file, content);
    }
    this.model = await startModelFixture({respond, logFile: this.requestsFile});
    this.modelFile = path.join(this.root, 'model.json');
    fs.writeFileSync(this.modelFile, JSON.stringify(modelConfig(this.model.url), null, 2));
    this.writeTmuxConfig();
    this.log(`# ${this.name}: home=${this.home} model=${this.model.url}`);
  }

  writeTmuxConfig() {
    this.configFile = path.join(this.root, 'tmux.conf');
    fs.writeFileSync(
      this.configFile,
      [
        'set-option -g remain-on-exit on',
        'set-option -g exit-empty off',
        'set-option -g default-terminal "screen-256color"',
        'set-option -g default-shell "/bin/sh"',
        // The server is started by this process, so its initial environment
        // carries the developer's variables until the case overrides them.
        'set-option -g update-environment ""',
        'set-option -g history-limit 20000',
        'set-option -g status off',
        'set-option -g escape-time 10',
        'set-option -g window-size manual',
        'set-option -g detach-on-destroy on',
        'set-option -g set-clipboard off',
        '',
      ].join('\n'),
    );
  }

  environment(extra = {}) {
    return {
      HOME: this.userHome,
      TMPDIR: path.join(this.root, 'tmp'),
      PATH: process.env.PATH ?? DEFAULT_PATH,
      LANG: 'en_US.UTF-8',
      TERM: 'screen-256color',
      SHELL: '/bin/sh',
      // The pane is a real tmux pane, so the CLI must take the multiplexer
      // branch: no synchronized-output negotiation of its own, and clipboard
      // output through tmux.
      TMUX: `${this.socket},0,0`,
      DIMCODE_UPDATE_CHECK: '0',
      ...extra,
    };
  }

  tmuxRun(args, {allowFailure = false} = {}) {
    if (this.closed) throw new Error(`case ${this.name} is closed; tmux ${args[0]} not run`);
    return this.execTmux(args, {allowFailure});
  }

  /// The raw tmux call, also used by the teardown that runs after the case is
  /// marked closed.
  execTmux(args, {allowFailure = false} = {}) {
    try {
      return execFileSync(
        this.tmux,
        ['-S', this.socket, ...(this.configFile ? ['-f', this.configFile] : []), ...args],
        {
          encoding: 'utf8',
          timeout: TMUX_TIMEOUT_MS,
          stdio: ['ignore', 'pipe', 'pipe'],
        },
      );
    } catch (error) {
      if (allowFailure) return '';
      const stderr = error.stderr?.toString().trim() ?? '';
      throw new Error(`tmux ${args.join(' ')} failed: ${stderr || error.message}`);
    }
  }

  /// Starts one CLI (or launcher) on its own tmux session and taps the pane's
  /// output stream, which is where escape sequences a pane cannot show live.
  /// The pane is created with a shell, the tap is attached, and only then is
  /// the program respawned in it: a `new-session` that runs the program first
  /// would let it write its terminal probe and first frames before the pipe
  /// exists.
  ///
  /// `modelConfig` runs the development model profile, whose daemon serves
  /// sessions but not provider management. `server` runs the managed
  /// production profile instead: the standalone App Server next to the CLI with
  /// the case's product home, so `/connect` manages real providers.
  async launch(
    argv = [],
    {env = {}, cols = 100, rows = 30, name, command, waitForComposer = false, modelConfig = true, server = false} = {},
  ) {
    const session = name ?? `cli-${this.sessions.length + 1}`;
    this.sessions.push(session);
    if (server && !fs.existsSync(this.appServer)) {
      throw new Error(`${this.appServer} is missing; build it with: cargo build --locked -p dim-app-server`);
    }
    const program =
      command ??
      (server
        ? [this.binary, '--home', this.home, '--server', this.appServer]
        : modelConfig
          ? [this.binary, '--home', this.home, '--model-config', this.modelFile]
          : [this.binary, '--home', this.home]);
    const line = shellCommand(this.environment(env), [...program, ...argv]);
    // Keep the developer's login shell and profile out of the pane: the
    // placeholder is an explicit `/bin/sh` with the case's disposable HOME and
    // PATH, so nothing it does can write to the user's home or print into the
    // tap. The tap exists before the CLI starts, so no first frame escapes it.
    const placeholder = shellCommand(this.environment({}), ['/bin/sh', '-c', 'exec /bin/sleep 3600']);
    this.log(`# launch ${session}: ${line}`);
    this.tmuxRun(['start-server']);
    for (const [key, value] of Object.entries(this.environment(env))) {
      this.tmuxRun(['set-environment', '-g', key, value]);
    }
    this.tmuxRun(['new-session', '-d', '-s', session, '-x', String(cols), '-y', String(rows), placeholder]);
    const rawFile = path.join(this.root, `${session}.raw`);
    this.tmuxRun(['pipe-pane', '-o', '-t', session, `cat >> ${quote(rawFile)}`]);
    this.tmuxRun(['respawn-pane', '-k', '-t', session, line]);
    const terminal = new Terminal(this, {session, rawFile});
    if (waitForComposer) await terminal.untilComposer();
    return terminal;
  }

  log(line) {
    const entry = `${new Date().toISOString()} ${line}`;
    fs.appendFileSync(this.interactionsFile, `${entry}\n`);
    if (this.verbose) process.stderr.write(`  ${entry}\n`);
  }

  runCommand(args, {env = {}, cwd, timeout = 60_000} = {}) {
    this.log(`# command: ${this.binary} ${args.join(' ')}${cwd ? ` (cwd ${cwd})` : ''}`);
    return execFileSync(this.binary, args, {
      encoding: 'utf8',
      timeout,
      cwd,
      env: {...process.env, ...this.environment(env)},
      stdio: ['ignore', 'pipe', 'pipe'],
    });
  }

  /// The same command without blocking this process. A command that reaches
  /// the model would deadlock a synchronous spawn, because the fixture answers
  /// on this event loop.
  runCommandAsync(args, {env = {}, cwd, timeout = 60_000} = {}) {
    this.log(`# command: ${this.binary} ${args.join(' ')}${cwd ? ` (cwd ${cwd})` : ''}`);
    return new Promise((resolve, reject) => {
      const child = spawn(this.binary, args, {
        cwd,
        env: {...process.env, ...this.environment(env)},
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      let stdout = '';
      let stderr = '';
      child.stdout.on('data', (chunk) => (stdout += chunk));
      child.stderr.on('data', (chunk) => (stderr += chunk));
      const timer = setTimeout(() => child.kill('SIGKILL'), timeout);
      child.on('error', reject);
      child.on('exit', (code, signal) => {
        clearTimeout(timer);
        const line = `${this.binary} ${args.join(' ')} ended ${code ?? signal}`;
        if (code === 0) resolve(stdout);
        else reject(new Error(`${line}\n${stderr}\n${stdout}`));
      });
    });
  }

  stopDaemon() {
    try {
      return this.runCommand(['--home', this.home, 'daemon', 'stop'], {timeout: 30_000});
    } catch {
      return '';
    }
  }

  async close() {
    if (this.closed) return;
    // A scenario abandoned by its timeout must not keep issuing commands into
    // a case whose server and directory are about to go away.
    this.closed = true;
    this.execTmux(['kill-server'], {allowFailure: true});
    this.stopDaemon();
    this.killLeftovers();
    await this.model?.close();
    if (!this.keep) fs.rmSync(this.root, {recursive: true, force: true});
  }

  /// The daemon's own shutdown is the normal exit. When it does not settle,
  /// every process still naming this case's home is the leftover: the linked
  /// `dimcode-server` and the standalone `dim-app-server` profile alike, whose
  /// own path says nothing about this case.
  killLeftovers() {
    if (!fs.existsSync(path.join(this.home, 'rust', 'daemon.sock'))) return;
    let listing = '';
    try {
      listing = execFileSync('ps', ['-axo', 'pid=,command='], {encoding: 'utf8', timeout: 5_000});
    } catch {
      return;
    }
    for (const line of listing.split('\n')) {
      const match = /^\s*(\d+)\s+(.*)$/.exec(line);
      if (!match || !match[2].includes(this.home)) continue;
      const pid = Number(match[1]);
      if (pid === process.pid) continue;
      try {
        process.kill(pid, 'SIGKILL');
      } catch {
        // The process left between the listing and the signal.
      }
    }
  }

  /// Copies everything a failure needs to be diagnosed from the case's own
  /// state: both captures, the raw terminal stream, the fixture's request log,
  /// the interaction log and the daemon's own log.
  writeArtifacts(target, error) {
    fs.mkdirSync(target, {recursive: true});
    const write = (name, content) => fs.writeFileSync(path.join(target, name), content);
    write('failure.txt', `${error?.stack ?? error}\n`);
    for (const session of this.sessions) {
      const terminal = new Terminal(this, {
        session,
        rawFile: path.join(this.root, `${session}.raw`),
      });
      try {
        write(`${session}-screen.txt`, terminal.capture({history: false}));
        write(`${session}-scrollback.txt`, terminal.capture({history: true}));
      } catch (captureError) {
        write(`${session}-screen.txt`, `<capture failed: ${captureError.message}>\n`);
      }
      const raw = path.join(this.root, `${session}.raw`);
      if (fs.existsSync(raw)) fs.copyFileSync(raw, path.join(target, `${session}-pane.raw`));
    }
    for (const [source, name] of [
      [this.interactionsFile, 'interactions.log'],
      [this.requestsFile, 'requests.jsonl'],
      [path.join(this.home, 'rust', 'daemon.log'), 'daemon.log'],
      [path.join(this.home, 'config.json'), 'config.json'],
      [path.join(this.root, 'model.json'), 'model.json'],
    ]) {
      if (fs.existsSync(source)) fs.copyFileSync(source, path.join(target, name));
    }
    const listing = [];
    const walk = (directory, prefix = '') => {
      for (const entry of fs.readdirSync(directory, {withFileTypes: true})) {
        const relative = path.join(prefix, entry.name);
        if (entry.isDirectory()) {
          listing.push(`${relative}/`);
          if (relative.split(path.sep).length < 3) walk(path.join(directory, entry.name), relative);
        } else {
          listing.push(`${relative} ${fs.statSync(path.join(directory, entry.name)).size}`);
        }
      }
    };
    if (fs.existsSync(this.home)) walk(this.home);
    write('home-listing.txt', `${listing.join('\n')}\n`);
    return target;
  }
}

export class Terminal {
  constructor(owner, {session, rawFile}) {
    this.owner = owner;
    this.session = session;
    this.rawFile = rawFile;
  }

  send(text) {
    this.owner.log(`send ${this.session} ${JSON.stringify(text)}`);
    this.owner.tmuxRun(['send-keys', '-t', this.session, '-l', '--', text]);
  }

  key(...keys) {
    this.owner.log(`key ${this.session} ${keys.join(' ')}`);
    this.owner.tmuxRun(['send-keys', '-t', this.session, ...keys]);
  }

  bytes(buffer) {
    const hex = Array.from(buffer, (byte) => byte.toString(16).padStart(2, '0'));
    this.owner.log(`bytes ${this.session} ${hex.join(' ')}`);
    this.owner.tmuxRun(['send-keys', '-t', this.session, '-H', ...hex]);
  }

  paste(text) {
    this.owner.log(`paste ${this.session} ${JSON.stringify(text)}`);
    this.bytes(Buffer.concat([
      Buffer.from('\u001b[200~', 'utf8'),
      Buffer.from(text, 'utf8'),
      Buffer.from('\u001b[201~', 'utf8'),
    ]));
  }

  resize(cols, rows) {
    this.owner.log(`resize ${this.session} ${cols}x${rows}`);
    this.owner.tmuxRun(['resize-window', '-t', this.session, '-x', String(cols), '-y', String(rows)]);
  }

  /// Resizes the pane and waits for the frame the product drew for that size.
  /// A capture alone cannot tell a repainted screen from the pane's resized
  /// grid: after a shrink the old, clipped frame still fits the new width, so
  /// the barrier needs the product's own repaint. The scrollback reflow clears
  /// and rewrites from `CSI 3J` and shows the cursor (`CSI ?25h`) when the frame
  /// is complete; the wait requires both after the resize, tmux to report the
  /// target size, the composer to be drawn, and the widest row to reach the new
  /// width. A caller may pass `until` to wait for content tied to that frame;
  /// the returned text is the capture that satisfied it, never the frame before
  /// the rewrite.
  async resizeTo(cols, rows, {timeout = DEFAULT_STEP_TIMEOUT_MS, expectComposer = true, stable = true, until} = {}) {
    const paneSize = () =>
      this.owner
        .tmuxRun(['display-message', '-p', '-t', this.session, '#{pane_width}x#{pane_height}'], {allowFailure: true})
        .trim();
    const target = `${cols}x${rows}`;
    const near = Math.max(1, cols - 10);
    const before = this.raw().length;
    this.resize(cols, rows);
    const deadline = Date.now() + timeout;
    let settled = null;
    for (;;) {
      let text = '';
      let widest = 0;
      const segment = this.raw().slice(before);
      const repainted = reflowComplete(segment);
      if (repainted && paneSize() === target) {
        text = this.capture({join: false});
        widest = Math.max(0, ...text.split('\n').map((line) => line.length));
        const content = until ? this.capture({history: true}) : text;
        const matched = !until || until(content);
        const drawn =
          (!expectComposer || text.includes('Ask anything...')) && widest >= near && widest <= cols && matched;
        if (drawn && (!stable || settled === text)) return content;
        settled = drawn ? text : null;
      } else {
        settled = null;
      }
      if (Date.now() >= deadline) {
        throw new Error(
          `pane did not settle at ${target} within ${timeout}ms: pane ${paneSize() || 'unknown'}, repainted ${repainted}, widest row ${widest}\n${this.capture({join: false})}\n--- output tail ---\n${tail(this.raw(), 2000)}`,
        );
      }
      await sleep(POLL_MS);
    }
  }

  capture({history = false, join = true} = {}) {
    const args = ['capture-pane', '-p', '-t', this.session];
    if (join) args.push('-J');
    if (history) args.push('-S', '-');
    return this.owner.tmuxRun(args);
  }

  raw() {
    return fs.existsSync(this.rawFile) ? fs.readFileSync(this.rawFile, 'utf8') : '';
  }

  /// The visible screen or, with `history`, the transcript the pane keeps above
  /// it, as the user can scroll to it.
  screen({history = false} = {}) {
    return this.capture({history});
  }

  async waitFor(expected, {timeout = DEFAULT_STEP_TIMEOUT_MS, history = false, describe} = {}) {
    const matches = (text) => (typeof expected === 'string' ? text.includes(expected) : expected.test(text));
    const deadline = Date.now() + timeout;
    const what = describe ?? (expected instanceof RegExp ? expected.toString() : JSON.stringify(expected));
    for (;;) {
      const text = this.capture({history});
      if (matches(text)) return text;
      if (Date.now() >= deadline) {
        throw new Error(
          `pane did not show ${what} within ${timeout}ms\n--- screen ---\n${this.capture()}\n--- transcript ---\n${this.capture({history: true})}\n--- output tail ---\n${tail(this.raw(), 4000)}`,
        );
      }
      await sleep(POLL_MS);
    }
  }

  async waitForRaw(predicate, {timeout = DEFAULT_STEP_TIMEOUT_MS, describe = 'terminal output'} = {}) {
    const deadline = Date.now() + timeout;
    for (;;) {
      const raw = this.raw();
      if (predicate(raw)) return raw;
      if (Date.now() >= deadline) {
        throw new Error(
          `terminal output did not satisfy ${describe} within ${timeout}ms\n--- output tail ---\n${tail(raw, 4000)}\n--- screen ---\n${this.capture()}`,
        );
      }
      await sleep(POLL_MS);
    }
  }

  async waitForAbsent(expected, {timeout = DEFAULT_STEP_TIMEOUT_MS} = {}) {
    const deadline = Date.now() + timeout;
    for (;;) {
      if (!this.capture().includes(expected)) return;
      if (Date.now() >= deadline) {
        throw new Error(`pane still showed ${JSON.stringify(expected)} after ${timeout}ms\n${this.capture()}`);
      }
      await sleep(POLL_MS);
    }
  }

  untilComposer({timeout = DEFAULT_STEP_TIMEOUT_MS} = {}) {
    return this.waitFor('Ask anything...', {timeout, describe: 'the composer'});
  }

  async sendPrompt(text, {timeout = DEFAULT_STEP_TIMEOUT_MS} = {}) {
    this.send(text);
    this.key('Enter');
    return this.untilComposer({timeout});
  }

  alive() {
    const listing = this.owner.tmuxRun(['list-panes', '-t', this.session, '-F', '#{pane_dead}'], {allowFailure: true});
    return listing.trim() === '0';
  }

  /// Waits for the CLI to end; `remain-on-exit` keeps the pane, its last screen
  /// and its exit status. A signal death is not a clean exit: the status is
  /// empty and the signal names the cause, so both are reported.
  async waitForExit({timeout = DEFAULT_STEP_TIMEOUT_MS} = {}) {
    const ended = await waitUntil(
      () => {
        const listing = this.owner.tmuxRun(
          ['list-panes', '-t', this.session, '-F', '#{pane_dead} #{pane_dead_status} #{pane_dead_signal}'],
          {allowFailure: true},
        ).trim();
        if (!listing) return false;
        const [dead, status = '', signal = ''] = listing.split(' ');
        if (dead !== '1') return false;
        return {
          status: status === '' ? null : Number(status),
          // Before tmux 3.3 the field is unknown and the format prints itself,
          // so only a real signal name counts.
          signal: /^SIG[A-Z0-9]+$/.test(signal) ? signal : null,
        };
      },
      {timeout, describe: `${this.session} to exit`},
    );
    if (ended.signal) {
      throw new Error(`${this.session} died from signal ${ended.signal}, not a clean exit\n${this.capture()}`);
    }
    if (ended.status === null || Number.isNaN(ended.status)) {
      throw new Error(`${this.session} ended without an exit status\n${this.capture()}`);
    }
    return ended;
  }
}

function tail(text, length) {
  return text.length > length ? `…${text.slice(-length)}` : text;
}
