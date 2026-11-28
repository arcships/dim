#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {DEFAULT_STEP_TIMEOUT_MS, TmuxCase, socketRoot, tmuxVersion} from './harness.mjs';
import {scenarios} from './scenarios.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const repository = path.resolve(here, '../..');

function usage(stream = process.stdout) {
  stream.write(`Usage: node scripts/tui-tmux/run.mjs [options] [SCENARIO...]

Runs the native terminal UI's end-to-end scenarios in dedicated tmux sessions:
real terminal input, resizes, and assertions on the captured screen.

Options:
  --list               List the scenarios and exit
  --keep               Keep each case's home, socket and raw output
  --artifacts DIR      Where failed cases are copied (default target/tui-tmux/<run>)
  --binary PATH        The dimcode binary (default target/debug/dimcode)
  --tmux PATH          The tmux program (default tmux)
  --timeout MS         A scenario's budget (default ${DEFAULT_STEP_TIMEOUT_MS * 4})
  --area AREA          Run only one area; repeatable
  --verbose            Print each tmux interaction as it happens
  --help               Show this help
`);
}

function parse(argv) {
  const options = {
    keep: false,
    verbose: false,
    list: false,
    binary: path.join(repository, 'target', 'debug', 'dimcode'),
    tmux: 'tmux',
    timeout: DEFAULT_STEP_TIMEOUT_MS * 4,
    areas: [],
    names: [],
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    const value = () => {
      const next = argv[index + 1];
      if (next === undefined) throw new Error(`${argument} needs a value`);
      index += 1;
      return next;
    };
    switch (argument) {
      case '--list':
        options.list = true;
        break;
      case '--keep':
        options.keep = true;
        break;
      case '--verbose':
        options.verbose = true;
        break;
      case '--artifacts':
        options.artifacts = path.resolve(value());
        break;
      case '--binary':
        options.binary = path.resolve(value());
        break;
      case '--tmux':
        options.tmux = value();
        break;
      case '--timeout': {
        const timeout = Number(value());
        if (!Number.isSafeInteger(timeout) || timeout <= 0) throw new Error('--timeout takes a positive number of milliseconds');
        options.timeout = timeout;
        break;
      }
      case '--area':
        options.areas.push(value());
        break;
      case '--help':
        usage();
        process.exit(0);
        break;
      default:
        if (argument.startsWith('-')) throw new Error(`unknown option ${argument}`);
        options.names.push(argument);
    }
  }
  return options;
}

function selected(options) {
  let chosen = scenarios;
  if (options.areas.length) chosen = chosen.filter((scenario) => options.areas.includes(scenario.area));
  if (options.names.length) {
    const known = new Set(scenarios.map((scenario) => scenario.name));
    for (const name of options.names) {
      if (!known.has(name)) throw new Error(`unknown scenario ${name}; --list names them`);
    }
    chosen = chosen.filter((scenario) => options.names.includes(scenario.name));
  }
  return chosen;
}

function runId() {
  return `${Date.now().toString(36)}-${process.pid.toString(36)}`;
}

function preflight(options) {
  if (!fs.existsSync(options.binary)) {
    throw new Error(`the CLI is not built at ${options.binary}; run: cargo build --locked -p dimcode`);
  }
  fs.accessSync(options.binary, fs.constants.X_OK);
  const version = tmuxVersion(options.tmux);
  if (version.major < 3 || (version.major === 3 && version.minor < 2)) {
    throw new Error(`the tmux suite needs tmux 3.2 or newer; ${options.tmux} reports ${version.printed}`);
  }
  return version;
}

async function withTimeout(promise, timeout, name) {
  let timer;
  const expired = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${name} exceeded its ${timeout}ms budget`)), timeout);
  });
  try {
    return await Promise.race([promise, expired]);
  } finally {
    clearTimeout(timer);
  }
}

async function main() {
  const options = parse(process.argv.slice(2));
  if (options.list) {
    let area = '';
    for (const scenario of scenarios) {
      if (scenario.area !== area) {
        area = scenario.area;
        process.stdout.write(`\n${area}\n`);
      }
      process.stdout.write(`  ${scenario.name}\n      ${scenario.title}\n`);
    }
    process.stdout.write('\n');
    return;
  }
  const version = preflight(options);
  const chosen = selected(options);
  if (!chosen.length) throw new Error('no scenario selected');
  const run = runId();
  const artifactsRoot = options.artifacts ?? path.join(repository, 'target', 'tui-tmux', run);
  const root = path.join(socketRoot(), `dimtui-${run}`);
  process.stderr.write(`tui-tmux: ${version.printed}, ${chosen.length} scenario(s), binary ${options.binary}\n`);
  process.stderr.write(`tui-tmux: failure artifacts under ${artifactsRoot}\n\n`);

  let failed = 0, skipped = 0;
  let active = null;
  let stopping = false;
  const stop = async (code, why) => {
    if (stopping) return;
    stopping = true;
    process.stderr.write(`tui-tmux: ${why}; closing ${active?.name ?? 'the run'}\n`);
    try {
      await active?.close();
    } catch (error) {
      process.stderr.write(`tui-tmux: cleanup failed: ${error.message}\n`);
    }
    if (!options.keep) fs.rmSync(root, {recursive: true, force: true});
    process.exit(code);
  };
  for (const [signal, code] of [['SIGINT', 130], ['SIGTERM', 143]]) {
    process.once(signal, () => stop(code, signal));
  }
  process.once('unhandledRejection', (error) => {
    stop(1, `unhandled rejection: ${error?.stack ?? error}`);
  });
  process.once('uncaughtException', (error) => {
    stop(1, `uncaught exception: ${error?.stack ?? error}`);
  });
  for (const [index, scenario] of chosen.entries()) {
    if (stopping) break;
    if (scenario.requiresSharing && !fs.existsSync(path.join(path.dirname(options.binary), 'dim-sharing'))) {
      skipped++;
      process.stderr.write(`SKIP ${scenario.name} (build dim-sharing beside the CLI to run this scenario)\n`);
      continue;
    }
    const started = Date.now();
    const caseRoot = path.join(root, String(index));
    const owner = new TmuxCase({
      name: scenario.name,
      root: caseRoot,
      binary: options.binary,
      tmux: options.tmux,
      keep: options.keep,
      verbose: options.verbose,
    });
    active = owner;
    process.stderr.write(`RUN  ${scenario.name}\n`);
    let failure;
    try {
      fs.mkdirSync(caseRoot, {recursive: true});
      const work = scenario.run(owner, {repository, runId: run, verbose: options.verbose});
      try {
        await withTimeout(work, options.timeout, scenario.name);
      } catch (error) {
        failure = error;
        // A timed-out scenario is still running: the case is closed next, and
        // whatever it throws on the way out is not a second failure.
        work.catch(() => {});
      }
    } catch (error) {
      failure = error;
    }
    if (failure) {
      failed += 1;
      const target = path.join(artifactsRoot, scenario.name);
      try {
        owner.writeArtifacts(target, failure);
      } catch (artifactError) {
        process.stderr.write(`tui-tmux: could not write artifacts for ${scenario.name}: ${artifactError.message}\n`);
      }
      process.stderr.write(`FAIL ${scenario.name} (${((Date.now() - started) / 1000).toFixed(1)}s)\n`);
      process.stderr.write(`${indent(failure.stack ?? String(failure))}\n`);
      process.stderr.write(`     artifacts: ${target}\n\n`);
    } else {
      process.stderr.write(`PASS ${scenario.name} (${((Date.now() - started) / 1000).toFixed(1)}s)\n\n`);
    }
    try {
      await owner.close();
    } catch (error) {
      if (!failure) {
        failed += 1;
        process.stderr.write(`FAIL ${scenario.name} (teardown)\n`);
      }
      process.stderr.write(`tui-tmux: closing ${scenario.name} failed: ${error.message}\n\n`);
    } finally {
      active = null;
    }
  }
  if (!options.keep) fs.rmSync(root, {recursive: true, force: true});
  process.stderr.write(`${chosen.length - failed - skipped}/${chosen.length - skipped} scenario(s) passed${skipped ? `, ${skipped} skipped` : ''}\n`);
  if (failed) process.exitCode = 1;
}

function indent(text) {
  return text
    .split('\n')
    .map((line) => `     ${line}`)
    .join('\n');
}

main().catch((error) => {
  process.stderr.write(`tui-tmux: ${error.message}\n`);
  process.exitCode = 1;
});
