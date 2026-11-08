import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';

const repo = path.resolve(import.meta.dirname, '..');
const defaults = path.join(repo, 'runtime/default-plugins.json');
const { plugins, optional = {} } = JSON.parse(readFileSync(defaults, 'utf8'));
const collisions = Object.keys(optional).filter((name) => Object.hasOwn(plugins, name));
assert.equal(collisions.length, 0, `A name cannot be in both "plugins" and "optional": ${collisions.join(', ')}`);
/** True when `candidate` lies strictly inside `parent`. */
const within = (parent, candidate) => {
  const relative = path.relative(parent, candidate);
  return relative !== '' && relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
};
const locations = [...Object.entries(plugins), ...Object.entries(optional)]
  .map(([name, relative]) => {
    const directory = path.resolve(path.join(path.dirname(defaults), relative));
    return { name, relative, folded: directory.toLowerCase() };
  });
for (let first = 0; first < locations.length; first += 1) {
  for (let second = first + 1; second < locations.length; second += 1) {
    const a = locations[first];
    const b = locations[second];
    assert(!(a.folded === b.folded || within(a.folded, b.folded) || within(b.folded, a.folded)),
    `Two plugins cannot stage into overlapping directories: ${a.name} (${a.relative}) and ${b.name} (${b.relative})`);
  }
}
const shipped = { ...plugins, ...optional };
const optionalNames = new Set(Object.keys(optional));
const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
const registry = process.env.DIMCODE_NPM_REGISTRY ?? 'https://registry.npmjs.org';

const copy = (source, target) => cpSync(source, target, {
  recursive: true,
  filter: (file) => {
    const base = path.basename(file);
    return base !== 'node_modules' && base !== 'tsconfig.json' && !base.endsWith('.test.mjs');
  },
});

const install = (target, name) => {
  const result = spawnSync(npm, ['ci', '--omit=dev', '--ignore-scripts', '--no-bin-links',
    '--no-audit', '--no-fund', '--registry', registry],
  { cwd: target, stdio: 'inherit', shell: process.platform === 'win32' });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `npm ci failed for ${name}`);
};

/** The dependency hash of one plugin: its package files, or null without them. */
const dependencies = (source) => {
  const files = ['package.json', 'package-lock.json'];
  if (!files.every((file) => existsSync(path.join(source, file)))) return null;
  const hash = createHash('sha256');
  for (const file of files) {
    hash.update(file).update('\0').update(readFileSync(path.join(source, file))).update('\0');
  }
  return hash.digest('hex');
};

/** Packages in a lock file that restrict their platform, with the field. */
const platformPackages = (lock) => {
  const found = [];
  const visit = (packages) => {
    for (const [name, meta] of Object.entries(packages ?? {})) {
      if (!meta || typeof meta !== 'object') continue;
      for (const field of ['os', 'cpu', 'libc']) {
        if (meta[field] !== undefined) found.push(`${name} (${field})`);
      }
      if (meta.dependencies) visit(meta.dependencies);
    }
  };
  visit(lock.packages);
  visit(lock.dependencies);
  return found;
};

/** Refuses a listed plugin whose lock names a platform-specific package. */
const assertPortable = () => {
  for (const [name, relative] of Object.entries(plugins)) {
    const file = path.join(path.dirname(defaults), relative, 'package-lock.json');
    if (!existsSync(file)) continue;
    const platform = platformPackages(JSON.parse(readFileSync(file, 'utf8')));
    if (platform.length > 0) {
      throw new Error(`The default plugin ${name} is not portable: package ${platform[0]}`);
    }
  }
};

/** True when a current plugin directory equals, sits inside, or contains `directory`. */
const occupied = (current, directory) => [...current].some((entry) =>
  entry === directory || within(directory, entry) || within(entry, directory));

/** Refuses a recorded or configured directory that does not lie inside the stage. */
const validate = (output, previous) => {
  const paths = [...Object.entries(shipped).map(([name, dir]) => [name, dir]),
    ...Object.entries(previous).filter(([, entry]) => entry?.dir).map(([name, entry]) => [name, entry.dir])];
  for (const [name, dir] of paths) {
    const resolved = path.resolve(output, dir);
    assert(within(output, resolved), `The directory for ${name} must lie strictly inside the staging directory: ${resolved}`);
  }
};

/** Puts `partial` in place of `target`, restoring `target` if the swap fails. */
const swap = (partial, target, rename) => {
  const previous = `${target}.previous.${process.pid}`;
  rmSync(previous, { recursive: true, force: true });
  const existed = existsSync(target);
  if (existed) rename(target, previous);
  try {
    rename(partial, target);
  } catch (error) {
    if (existed) rename(previous, target);
    throw error;
  }
  rmSync(previous, { recursive: true, force: true });
};

const writeAtomically = (file, content) => {
  const temporary = `${file}.tmp.${process.pid}`;
  writeFileSync(temporary, content);
  renameSync(temporary, file);
};

/** Blocks this thread without busy-waiting; staging is sequential by nature. */
const sleep = (milliseconds) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, milliseconds);

const alive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code !== 'ESRCH';
  }
};

/** The pid a lock's owner file names, or null while it is being written. */
const lockOwner = (lock) => {
  try {
    const pid = Number(readFileSync(path.join(lock, 'owner'), 'utf8').trim());
    return Number.isInteger(pid) && pid > 0 ? pid : null;
  } catch {
    return null;
  }
};

/**
 * One lock per stage: a candidate directory holding the owner's pid is renamed
 * into place, so a lock always has an owner and a lock without one is stale. A
 * second run waits for a live holder; a lock whose owner is gone, or whose
 * owner is this process, is taken over by removing it and retrying. Two runs
 * can both decide a lock is stale and both take it over; the worst this can do
 * is make one run fail with "Cannot stage", and the live plugin stays whole
 * because a rename onto a non-empty target fails and the swap restores it.
 */
const acquire = (lock, { waitMilliseconds, pollMilliseconds }) => {
  const deadline = Date.now() + waitMilliseconds;
  const candidate = `${lock}.new.${process.pid}`;
  let announced = false;
  for (;;) {
    try {
      rmSync(candidate, { recursive: true, force: true });
      mkdirSync(candidate);
      writeFileSync(path.join(candidate, 'owner'), `${process.pid}\n`);
      renameSync(candidate, lock);
      return;
    } catch (error) {
      rmSync(candidate, { recursive: true, force: true });
      if (error.code !== 'EEXIST' && error.code !== 'ENOTEMPTY') throw error;
    }
    const owner = lockOwner(lock);
    if (owner === null || owner === process.pid || !alive(owner)) {
      rmSync(lock, { recursive: true, force: true });
      continue;
    }
    if (!announced) {
      console.error(`Waiting for another staging run to finish (${lock}).`);
      announced = true;
    }
    if (Date.now() + pollMilliseconds > deadline) {
      throw new Error(`The staging lock is still held after ${waitMilliseconds} ms: ${lock}`);
    }
    sleep(pollMilliseconds);
  }
};

/**
 * Removes what a killed run left behind. A `previous` whose target is missing
 * is renamed back, and every other temporary is removed, but only when its pid
 * is not alive or is this process's own, so a live run's temporaries stay.
 */
const recover = (output, bases) => {
  const dead = (pid) => pid === process.pid || !alive(pid);
  const parents = new Set([output]);
  for (const base of bases) parents.add(path.dirname(base));
  for (const parent of parents) {
    if (!existsSync(parent)) continue;
    for (const entry of readdirSync(parent)) {
      const match = /^(.*)\.(partial|previous)\.(\d+)$/.exec(entry);
      if (!match || !dead(Number(match[3]))) continue;
      const leftover = path.join(parent, entry);
      if (match[2] === 'previous') {
        const target = path.join(parent, match[1]);
        if (existsSync(target)) rmSync(leftover, { recursive: true, force: true });
        else renameSync(leftover, target);
      } else {
        rmSync(leftover, { recursive: true, force: true });
      }
    }
  }
  for (const entry of readdirSync(output)) {
    const temporary = /^(\.stage-plugins\.json|default-plugins\.json)\.tmp\.(\d+)$/.exec(entry);
    const candidate = /^\.stage-plugins\.lock\.new\.(\d+)$/.exec(entry);
    if (!temporary && !candidate) continue;
    if (!dead(Number(temporary?.[2] ?? candidate?.[1]))) continue;
    rmSync(path.join(output, entry), { recursive: true, force: true });
  }
};

/**
 * Updates an existing stage in place under one lock per stage. The rule: every
 * run copies every listed plugin's sources again, so an edit takes effect on
 * the next run; `npm ci` runs only for a default plugin whose package files
 * hash differs from the one recorded in `.stage-plugins.json`, never for an
 * optional plugin; a plugin that is no longer listed is removed; and each
 * plugin is built beside the live copy, whose dependencies it copies when they
 * are reused, then swapped in only when complete. The live copy is never
 * touched before the swap, so every staged plugin is complete at every moment;
 * the swap renames it aside, puts the replacement in place and restores it if
 * that fails. The record is rewritten after each swap, so it always describes
 * what is on disk. A failure names the plugin, exits non-zero, and leaves the
 * plugins before it updated and the rest as they were. A second run waits for
 * the lock, and the first run of a stage left by a killed one takes over its
 * stale lock and recovers its temporaries. Recorded directories are validated
 * against the stage before anything changes, and a directory is removed only
 * when no current plugin uses it.
 */
export function updateStage(destination, {
  waitMilliseconds = 180_000,
  pollMilliseconds = 200,
  rename = renameSync,
} = {}) {
  const output = path.resolve(destination);
  mkdirSync(output, { recursive: true });
  const lock = path.join(output, '.stage-plugins.lock');
  acquire(lock, { waitMilliseconds, pollMilliseconds });
  try {
    updateLocked(output, rename);
  } finally {
    if (lockOwner(lock) === process.pid) rmSync(lock, { recursive: true, force: true });
  }
}

/** The body of an update, run while holding the stage lock. */
function updateLocked(output, rename) {
  const record = path.join(output, '.stage-plugins.json');
  const previous = (() => {
    try {
      const data = JSON.parse(readFileSync(record, 'utf8'));
      return data.plugins && typeof data.plugins === 'object' ? data.plugins : {};
    } catch {
      return {};
    }
  })();
  validate(output, previous);
  recover(output, [...Object.values(shipped).map((relative) => path.resolve(output, relative)),
    ...Object.values(previous).filter((entry) => entry?.dir).map((entry) => path.resolve(output, entry.dir))]);
  const entries = { ...previous };
  const writeRecord = () => writeAtomically(record,
    `${JSON.stringify({ schemaVersion: 1, plugins: entries }, null, 2)}\n`);
  const current = new Set(Object.values(shipped).map((relative) => path.resolve(output, relative)));
  const installed = [];
  for (const [name, relative] of Object.entries(shipped)) {
    const source = path.join(path.dirname(defaults), relative);
    const target = path.join(output, relative);
    const partial = `${target}.partial.${process.pid}`;
    const isOptional = optionalNames.has(name);
    const code = existsSync(path.join(source, 'package.json'));
    assert(!code || existsSync(path.join(source, 'package-lock.json')), `${name} has no package-lock.json`);
    const hash = isOptional ? null : dependencies(source);
    const before = previous[name];
    const reusable = !isOptional && before?.hash === hash && existsSync(target)
      && (!code || existsSync(path.join(target, 'node_modules')));
    try {
      rmSync(partial, { recursive: true, force: true });
      mkdirSync(path.dirname(target), { recursive: true });
      copy(source, partial);
      if (reusable) {
        const modules = path.join(target, 'node_modules');
        if (existsSync(modules)) {
          cpSync(modules, path.join(partial, 'node_modules'), { recursive: true, verbatimSymlinks: true });
        }
      } else if (!isOptional && code) {
        install(partial, name);
        installed.push(name);
      }
      swap(partial, target, rename);
    } catch (error) {
      rmSync(partial, { recursive: true, force: true });
      throw new Error(`Cannot stage ${name}: ${error.message}`);
    }
    if (before?.dir) {
      const old = path.resolve(output, before.dir);
      if (!occupied(current, old)) rmSync(old, { recursive: true, force: true });
    }
    entries[name] = isOptional ? { dir: relative } : { hash, dir: relative };
    writeRecord();
  }
  for (const [name, entry] of Object.entries(previous)) {
    if (Object.hasOwn(shipped, name) || !entry?.dir) continue;
    const old = path.resolve(output, entry.dir);
    if (!occupied(current, old)) rmSync(old, { recursive: true, force: true });
    delete entries[name];
  }
  writeRecord();
  const defaultsCopy = path.join(output, 'default-plugins.json');
  const defaultsTemporary = `${defaultsCopy}.tmp.${process.pid}`;
  copy(defaults, defaultsTemporary);
  renameSync(defaultsTemporary, defaultsCopy);
  console.log(`Updated ${Object.keys(shipped).length} plugins (npm ci: ${installed.length ? installed.join(', ') : 'none'}).`);
}

function freshStage(destination) {
  const output = path.resolve(destination);
  assert(!existsSync(output), 'Destination already exists');
  try {
    mkdirSync(output, { recursive: true });
    cpSync(defaults, path.join(output, 'default-plugins.json'));
    for (const [name, relative] of Object.entries(shipped)) {
      const source = path.join(path.dirname(defaults), relative);
      const target = path.join(output, relative);
      const code = existsSync(path.join(source, 'package.json'));
      assert(!code || existsSync(path.join(source, 'package-lock.json')), `${name} has no package-lock.json`);
      copy(source, target);
      if (!code || optionalNames.has(name)) continue;
      install(target, name);
    }
    console.log(`Staged ${Object.keys(shipped).length} plugins.`);
  } catch (error) {
    rmSync(output, { recursive: true, force: true });
    throw error;
  }
}

function main() {
  const [destination, ...extra] = process.argv.slice(2);
  const update = extra.includes('--update');
  const portable = extra.includes('--portable');
  const arguments_ = extra.filter((argument) => argument !== '--update' && argument !== '--portable');
  assert(destination && arguments_.length === 0,
    'Usage: node scripts/stage-plugins.mjs DESTINATION [--update] [--portable]');
  if (portable) assertPortable();
  if (update) updateStage(path.resolve(destination));
  else freshStage(path.resolve(destination));
}

if (import.meta.main) main();
