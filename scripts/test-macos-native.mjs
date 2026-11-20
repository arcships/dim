import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {closeSync, mkdtempSync, openSync, readFileSync, realpathSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, posix, relative, resolve, sep} from 'node:path';
import {fileURLToPath} from 'node:url';

const root = realpathSync(fileURLToPath(new URL('../', import.meta.url)));
process.chdir(root);
const macOS = /target_os\s*=\s*"macos"|target_vendor\s*=\s*"apple"/;
const testAttribute = /#\[(?:\w+::)*test\b/;

function macOSGuard(attributes) {
  return [...attributes.matchAll(/#!?\[cfg(_attr)?\(([\s\S]*?)\)\]/g)].some(([, conditionalAttribute, condition]) => {
    if (!macOS.test(condition)) return false;
    if (conditionalAttribute) return /^\s*not\s*\(\s*target_(?:os|vendor)\s*=\s*"(?:macos|apple)"\s*\)\s*,\s*ignore\b/.test(condition);
    return !/\bnot\s*\(|target_os\s*=\s*"linux"|\bany\s*\(\s*test\b/.test(condition);
  });
}

function nativeTests(file, source, gatedModules) {
  const fileGated = macOSGuard(source.match(/^#!\[cfg\([^\]]+\]/gm)?.join('\n') ?? '') ||
    gatedModules.some(directory => file === `${directory}.rs` || file.startsWith(`${directory}/`));
  const modules = [];
  const names = [];
  for (const match of source.matchAll(/^([ \t]*)((?:#\[[^\]]+\]\s*)*)(?:(?:pub(?:\([^)]*\))?|async|unsafe)\s+)*(mod|fn)\s+(\w+)\b/gm)) {
    const [, indent, attributes, kind, name] = match;
    while (modules.length && match.index >= modules.at(-1)) modules.pop();
    if (kind === 'mod' && (!macOSGuard(attributes) || !/^\s*\{/.test(source.slice(match.index + match[0].length)))) continue;
    if (kind === 'fn' && !testAttribute.test(attributes)) continue;
    const endPattern = new RegExp(`^${indent}\\}`, 'gm');
    endPattern.lastIndex = match.index + match[0].length;
    const lineEnd = source.indexOf('\n', endPattern.lastIndex);
    const end = source.slice(endPattern.lastIndex, lineEnd).trimEnd().endsWith('}')
      ? lineEnd
      : endPattern.exec(source)?.index ?? source.length;
    if (kind === 'mod') {
      modules.push(end);
      continue;
    }
    const body = source.slice(match.index + match[0].length, end);
    if (fileGated || modules.length || macOSGuard(attributes) || macOSGuard(body) ||
      /cfg!\(target_os\s*=\s*"macos"\)|\bapple_vision_available\s*\(/.test(body) ||
      (posix.basename(file) === 'modality_flow.rs' && /\bassert_plugin_ocr_refused\(/.test(body)) ||
      (posix.basename(file) === 'host_platform_flow.rs' && /\bconfined_platform\(/.test(body) && !macOS.test(attributes))) {
      names.push(name);
    }
  }
  return names;
}

const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--locked', '--no-deps', '--format-version=1'], {encoding: 'utf8'}));
const targets = metadata.packages.flatMap(pkg => pkg.targets.filter(t => t.test && ['lib', 'test', 'bin'].includes(t.kind[0])).map(t => ({
  pkg: pkg.name, name: t.name, kind: t.kind[0], src: relative(root, t.src_path).split(sep).join('/'), tests: new Set(),
})));
const files = execFileSync('git', ['ls-files', '-z', '--', '*.rs'], {encoding: 'utf8'}).split('\0').filter(Boolean);
const sources = new Map(files.map(file => [file, readFileSync(file, 'utf8')]));
const gatedModules = [];
for (const [file, source] of sources) {
  for (const [, attributes, name] of source.matchAll(/^\s*((?:#\[[^\]]+\]\s*)+)(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;/gm)) {
    if (!macOSGuard(attributes)) continue;
    const directory = /\/(lib|main|mod)\.rs$/.test(file) ? posix.dirname(file) : file.slice(0, -3);
    gatedModules.push(posix.join(directory, name));
  }
}
for (const [file, source] of sources) {
  const names = nativeTests(file, source, gatedModules);
  if (!names.length) continue;
  const target = targets.find(t => t.src === file) ?? targets.find(t => file.startsWith(`${t.src.slice(0, -3)}/`)) ??
    targets.find(t => /\/(lib|main)\.rs$/.test(t.src) && file.startsWith(`${posix.dirname(t.src)}/`));
  assert(target, `No Cargo test target owns ${file}`);
  for (const name of names) target.tests.add(name);
}
const selected = targets.filter(t => t.tests.size);
assert(selected.length, 'No native macOS test targets found');
for (const t of selected) console.log(`${t.pkg} ${t.name}: ${[...t.tests].join(', ')}`);
if (process.argv.includes('--list')) process.exit(0);
assert.equal(process.platform, 'darwin', 'Native tests require macOS');
assert.equal(process.arch, 'arm64', 'Apple Vision tests require an ARM64 runner');
const temporary = mkdtempSync(join(tmpdir(), 'dim-macos-ci-'));
try {
  const env = {...process.env, DIMCODE_HOME: join(temporary, 'home')};
  const artifacts = join(temporary, 'artifacts.jsonl');
  const output = openSync(artifacts, 'w');
  const packages = [...new Set(selected.map(t => t.pkg))];
  const targetArgs = [...new Set(selected.map(t => t.kind === 'lib' ? '--lib' : `--${t.kind}=${t.name}`))];
  try {
    execFileSync('cargo', ['test', '--locked', '--no-run', '--message-format=json-render-diagnostics',
      ...packages.flatMap(pkg => ['-p', pkg]), ...targetArgs], {env, stdio: ['ignore', output, 'inherit']});
  } finally {
    closeSync(output);
  }
  const records = readFileSync(artifacts, 'utf8').trimEnd().split('\n').map(line => JSON.parse(line));
  for (const t of selected) {
    const record = records.find(r => r.reason === 'compiler-artifact' && r.profile.test && r.executable && relative(root, r.target.src_path) === t.src);
    assert(record, `Missing compiled test target ${t.src}`);
    const listing = execFileSync(record.executable, ['--list', '--format=terse'], {encoding: 'utf8', env});
    const available = listing.split('\n').filter(line => line.endsWith(': test')).map(line => line.slice(0, -6));
    const exact = [...t.tests].map(name => {
      const matches = available.filter(test => test.split('::').at(-1) === name);
      assert.equal(matches.length, 1, `Expected one ${name} in ${t.src}, found ${matches.length}`);
      return matches[0];
    });
    execFileSync(process.execPath, [resolve('scripts/run-rust-test.mjs'), artifacts, t.name, '--test-threads=2', '--exact', ...exact], {env, stdio: 'inherit'});
  }
} finally {
  rmSync(temporary, {recursive: true, force: true});
}
