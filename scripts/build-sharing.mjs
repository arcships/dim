import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { buildPlatform } from './native-binary.mjs';

const [destination] = process.argv.slice(2);
assert(destination, 'Usage: node scripts/build-sharing.mjs BIN_DIR');
const { osName, cpu } = buildPlatform();
const dir = path.resolve(destination);
mkdirSync(dir, { recursive: true });
const cwd = path.resolve(import.meta.dirname, '../runtime/native-sharing');
const options = { cwd, encoding: 'utf8', env: { ...process.env, CGO_ENABLED: '0',
  GOOS: osName === 'windows' ? 'windows' : osName, GOARCH: cpu === 'arm64' ? 'arm64' : 'amd64' } };
const suffix = osName === 'windows' ? '.exe' : '';
execFileSync('go', ['build', '-mod=readonly', '-trimpath', '-ldflags=-s -w', '-o', path.join(dir, 'dim-sharing' + suffix), '.'], options);
const goroot = execFileSync('go', ['env', 'GOROOT'], options).trim();
const modules = execFileSync('go', ['list', '-mod=readonly', '-deps', '-f',
  '{{if and .Module (not .Module.Main)}}{{.Module.Path}} {{.Module.Version}} {{.Module.Dir}}{{end}}', '.'], options);
const notices = [readFileSync(path.join(goroot, 'LICENSE'), 'utf8')];
for (const line of [...new Set(modules.trim().split(/\r?\n/).filter(Boolean))].sort()) {
  const [, name, version, directory] = /^(\S+) (\S+) (.+)$/.exec(line);
  notices.push(`${name} ${version}\n\n${readFileSync(path.join(directory, 'LICENSE'), 'utf8')}`);
}
writeFileSync(path.join(dir, 'GO-THIRD-PARTY-NOTICES.txt'), notices.join('\n\n'));
