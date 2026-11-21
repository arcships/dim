import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import test from 'node:test';

const script = 'test-macos-native.mjs';

test('macOS discovery selects gated modules, individual tests and native assertions in Cargo targets', t => {
  const cwd = mkdtempSync(join(tmpdir(), 'dim-native-selection-'));
  t.after(() => rmSync(cwd, {recursive: true, force: true}));
  for (const dir of ['src', 'scripts', 'tests']) mkdirSync(join(cwd, dir));
  cpSync(new URL(script, import.meta.url), join(cwd, 'scripts', script));
  writeFileSync(join(cwd, 'Cargo.toml'), '[package]\nname="native-selection"\nversion="0.1.0"\nedition="2024"\n');
  writeFileSync(join(cwd, 'src/lib.rs'), `
#[cfg(all(test, target_os = "macos"))]
mod native;
#[test]
fn ordinary() {}
#[test]
fn native_assertion() {
    #[cfg(target_os = "macos")]
    assert!(true);
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn both_platforms() {}
#[cfg(not(target_os = "macos"))]
#[test]
fn other_platform() {}
#[cfg_attr(not(target_os = "macos"), ignore = "requires Apple Vision")]
#[test]
fn vision() {}
`);
  writeFileSync(join(cwd, 'src/native.rs'), '#[test]\nfn nested_case() {}\n');
  writeFileSync(join(cwd, 'tests/engine.rs'), `
#![cfg(target_os = "macos")]
#[test]
fn engine_case() {}
`);
  execFileSync('git', ['init', '--quiet'], {cwd});
  execFileSync('git', ['add', '--', 'Cargo.toml', 'src', 'tests'], {cwd});
  const output = execFileSync(process.execPath, [join(cwd, 'scripts', script), '--list'], {cwd, encoding: 'utf8'});
  const tests = output.trim().split('\n').flatMap(row => row.split(': ')[1].split(', '));
  assert.deepEqual(tests.sort(), ['engine_case', 'native_assertion', 'nested_case', 'vision']);
});

test('macOS discovery follows consolidated autotests = false entry roots and #[path] submodules', t => {
  const cwd = mkdtempSync(join(tmpdir(), 'dim-native-consolidated-'));
  t.after(() => rmSync(cwd, {recursive: true, force: true}));
  for (const dir of ['src', 'scripts', 'tests/sub']) mkdirSync(join(cwd, dir), {recursive: true});
  cpSync(new URL(script, import.meta.url), join(cwd, 'scripts', script));
  writeFileSync(join(cwd, 'Cargo.toml'), [
    '[package]',
    'name = "native-consolidated"',
    'version = "0.1.0"',
    'edition = "2024"',
    'autotests = false',
    '',
    '[[test]]',
    'name = "suite"',
    'path = "tests/suite.rs"',
    '',
  ].join('\n'));
  writeFileSync(join(cwd, 'src/lib.rs'), '');
  writeFileSync(join(cwd, 'tests/suite.rs'), `
#[cfg(target_os = "macos")]
mod engine;
#[path = "sub/guarded.rs"]
mod guarded;
`);
  writeFileSync(join(cwd, 'tests/engine.rs'), '#[test]\nfn engine_case() {}\n');
  writeFileSync(join(cwd, 'tests/sub/guarded.rs'), `
#[cfg(target_os = "macos")]
#[test]
fn path_case() {}
`);
  execFileSync('git', ['init', '--quiet'], {cwd});
  execFileSync('git', ['add', '--', 'Cargo.toml', 'src', 'tests'], {cwd});
  const output = execFileSync(process.execPath, [join(cwd, 'scripts', script), '--list'], {cwd, encoding: 'utf8'});
  assert.equal(output.trim(), 'native-consolidated suite: engine_case, path_case');
});
