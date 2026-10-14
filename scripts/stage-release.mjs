import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync,
  readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { extractPluginArchive, pluginArchiveLimit } from './plugin-package.mjs';

const [binaryDirectory, destination, version, sourceCommit, mode, ...extra] = process.argv.slice(2);
assert(binaryDirectory && destination && version && sourceCommit && extra.length === 0,
  'Usage: node scripts/stage-release.mjs BIN_DIR DESTINATION VERSION SOURCE_COMMIT [--with-plugin]');
assert(mode === undefined || mode === '--with-plugin', 'Unknown staging mode');
assert(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$/.test(version), 'Invalid version');
assert(/^[a-f0-9]{40}$/.test(sourceCommit), 'Invalid source commit');
const hostPlatform = `${process.platform === 'win32' ? 'windows' : process.platform}-${process.arch}`;
const platform = process.env.DIM_BUILD_PLATFORM || hostPlatform;
assert(['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'windows-arm64', 'windows-x64'].includes(platform), 'Unsupported native platform');
const [osName, cpu] = platform.split('-');
assert.equal(osName, hostPlatform.split('-')[0], 'Cross builds require a runner with the target OS');
const repo = path.resolve(import.meta.dirname, '..');
const productVersion = readFileSync(path.join(repo, 'Cargo.toml'), 'utf8').match(/\[workspace.package\]\s+version = "([^"]+)"/)[1];
assert.equal(version, productVersion, 'Version must match Cargo workspace');
assert.equal(version, JSON.parse(readFileSync(path.join(repo, 'npm/package.json'))).version, 'Version must match npm wrapper');
const output = path.resolve(destination);
assert(!existsSync(output), 'Destination already exists');
const digest = (data) => createHash('sha256').update(data).digest('hex');
const run = (command, args, options = {}) => {
  const result = spawnSync(command, args, { encoding: 'utf8', ...options });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${command} failed: ${result.stderr}`);
  return result.stdout;
};
const suffix = process.platform === 'win32' ? '.exe' : '';
const cli = path.resolve(binaryDirectory, `dimcode${suffix}`);
const server = path.resolve(binaryDirectory, `dim-app-server${suffix}`);
assert(lstatSync(cli).isFile() && lstatSync(server).isFile(), 'Native executables must be regular files');
const checkArchitecture = (file) => {
  const data = readFileSync(file);
  if (osName === 'windows') {
    assert.equal(data.readUInt16LE(0), 0x5a4d, 'Expected a PE executable');
    const offset = data.readUInt32LE(0x3c);
    assert.equal(data.readUInt32LE(offset), 0x00004550, 'Invalid PE signature');
    assert.equal(data.readUInt16LE(offset + 4), cpu === 'arm64' ? 0xaa64 : 0x8664, 'PE architecture mismatch');
  } else if (osName === 'darwin') {
    assert.equal(data.readUInt32LE(0), 0xfeedfacf, 'Expected a 64-bit Mach-O executable');
    assert.equal(data.readUInt32LE(4), cpu === 'arm64' ? 0x0100000c : 0x01000007, 'Mach-O architecture mismatch');
  } else {
    assert.equal(data.subarray(0, 6).toString('hex'), '7f454c460201', 'Expected a little-endian ELF64 executable');
    assert.equal(data.readUInt16LE(18), cpu === 'arm64' ? 183 : 62, 'ELF architecture mismatch');
  }
};
if (platform === hostPlatform) {
  assert.equal(run(cli, ['--version']).trim(), `dimcode ${version}`, 'Binary version mismatch');
} else {
  // shortcut: Cross-built binaries are inspected; add target-machine smoke tests when runners exist.
  checkArchitecture(cli);
  checkArchitecture(server);
}
const scratch = mkdtempSync(path.join(os.tmpdir(), 'dimcode-stage-'));
let plugin = null;
try {
  if (mode === '--with-plugin') {
    const expectedDigest = process.env.DIM_OAUTH_PACKAGE_SHA256;
    assert(/^[a-f0-9]{64}$/.test(expectedDigest ?? ''), 'DIM_OAUTH_PACKAGE_SHA256 is required');
    const local = process.env.DIM_OAUTH_PACKAGE_ARCHIVE;
    const url = process.env.DIM_OAUTH_PACKAGE_URL;
    assert(Boolean(local) !== Boolean(url), 'Set exactly one plugin archive source');
    let data;
    if (local) {
      assert(lstatSync(local).isFile(), 'Plugin archive must be a regular file');
      data = readFileSync(local);
    } else {
      let address;
      try { address = new URL(url); }
      catch { throw new Error('Invalid private plugin URL'); }
      assert(['https:', 'http:'].includes(address.protocol) && !address.username && !address.password,
        'Invalid plugin URL');
      const headers = {};
      if (process.env.DIM_OAUTH_DOWNLOAD_TOKEN) headers.Authorization = `Bearer ${process.env.DIM_OAUTH_DOWNLOAD_TOKEN}`;
      let response;
      try { response = await fetch(address, { headers, redirect: 'error', signal: AbortSignal.timeout(60_000) }); }
      catch { throw new Error('Private plugin download failed'); }
      assert(response.ok, 'Private plugin download failed');
      const chunks = [];
      let size = 0;
      try {
        for await (const chunk of response.body) {
          size += chunk.length;
          assert(size <= pluginArchiveLimit, 'Plugin archive is too large');
          chunks.push(chunk);
        }
      } catch { throw new Error('Private plugin download failed or exceeded its size limit'); }
      data = Buffer.concat(chunks);
    }
    const archive = path.join(scratch, 'plugin.tar.gz');
    writeFileSync(archive, data);
    const extracted = path.join(scratch, 'plugin');
    plugin = extractPluginArchive(archive, expectedDigest, extracted);
  }
  mkdirSync(path.join(output, 'bin'), { recursive: true });
  copyFileSync(cli, path.join(output, 'bin', `dimcode${suffix}`));
  copyFileSync(server, path.join(output, 'bin', `dim-app-server${suffix}`));
  chmodSync(path.join(output, 'bin', `dimcode${suffix}`), 0o755);
  chmodSync(path.join(output, 'bin', `dim-app-server${suffix}`), 0o755);
  copyFileSync(path.join(repo, 'LICENSE'), path.join(output, 'LICENSE'));
  const versions = { rust: readFileSync(path.join(repo, 'rust-toolchain.toml'), 'utf8').match(/channel = "([^"]+)"/)[1] };
  if (plugin) {
    const nodeVersion = readFileSync(path.join(repo, '.node-version'), 'utf8').trim();
    assert.equal(process.versions.node, nodeVersion, 'Node version mismatch');
    let nodeBinary = process.execPath;
    let license = path.join(path.dirname(process.execPath), process.platform === 'win32' ? 'LICENSE' : '../LICENSE');
    if (platform !== hostPlatform) {
      const distribution = `node-v${nodeVersion}-${osName === 'windows' ? 'win' : osName}-${cpu}`;
      const filename = `${distribution}.${osName === 'windows' ? 'zip' : 'tar.gz'}`;
      const base = `https://nodejs.org/dist/v${nodeVersion}/`;
      const checksums = await fetch(`${base}SHASUMS256.txt`, { redirect: 'error', signal: AbortSignal.timeout(60_000) });
      assert(checksums.ok, 'Node checksum download failed');
      const expected = (await checksums.text()).split('\n').map(line => line.trim().split(/\s+/)).find(([, name]) => name === filename)?.[0];
      assert(/^[a-f0-9]{64}$/.test(expected ?? ''), 'Missing target Node checksum');
      const response = await fetch(`${base}${filename}`, { redirect: 'error', signal: AbortSignal.timeout(120_000) });
      assert(response.ok, 'Target Node download failed');
      const archive = Buffer.from(await response.arrayBuffer());
      assert.equal(digest(archive), expected, 'Target Node archive digest mismatch');
      const nodeArchive = path.join(scratch, filename);
      writeFileSync(nodeArchive, archive);
      run('tar', ['-xf', nodeArchive, '-C', scratch]);
      nodeBinary = path.join(scratch, distribution, osName === 'windows' ? 'node.exe' : 'bin/node');
      license = path.join(scratch, distribution, 'LICENSE');
    }
    checkArchitecture(nodeBinary);
    const nodeDirectory = path.join(output, 'runtime/node');
    mkdirSync(path.join(nodeDirectory, process.platform === 'win32' ? '.' : 'bin'), { recursive: true });
    copyFileSync(nodeBinary, path.join(nodeDirectory, process.platform === 'win32' ? 'node.exe' : 'bin/node'));
    chmodSync(path.join(nodeDirectory, process.platform === 'win32' ? 'node.exe' : 'bin/node'), 0o755);
    assert(lstatSync(license).isFile(), 'Node distribution LICENSE is required');
    copyFileSync(license, path.join(nodeDirectory, 'LICENSE'));
    const runtime = path.join(output, 'runtime/rutis');
    mkdirSync(runtime);
    copyFileSync(path.join(repo, 'runtime/package.json'), path.join(runtime, 'package.json'));
    copyFileSync(path.join(repo, 'runtime/package-lock.json'), path.join(runtime, 'package-lock.json'));
    const npmCli = path.join(path.dirname(process.execPath), process.platform === 'win32' ? 'node_modules/npm/bin/npm-cli.js' : '../lib/node_modules/npm/bin/npm-cli.js');
    assert(lstatSync(npmCli).isFile(), 'Node distribution npm CLI is required');
    run(process.execPath, [npmCli, 'ci', '--omit=dev', '--ignore-scripts',
      `--os=${osName === 'windows' ? 'win32' : osName}`, `--cpu=${cpu}`, '--registry=https://registry.npmjs.org'], { cwd: runtime });
    assert(lstatSync(path.join(runtime, 'node_modules/@esbuild', `${osName === 'windows' ? 'win32' : osName}-${cpu}`,
      osName === 'windows' ? 'esbuild.exe' : 'bin/esbuild')).isFile(), 'Missing target runtime executable');
    rmSync(path.join(runtime, 'node_modules/.bin'), { recursive: true, force: true });
    cpSync(path.join(scratch, 'plugin'), path.join(output, 'plugins/dim-oauth'), { recursive: true });
    writeFileSync(path.join(output, 'NOTICE'), 'Dim public source is licensed under MIT; see LICENSE.\nThe separately supplied plugins/dim-oauth runtime is excluded from that MIT grant.\nThird-party runtimes retain their original license notices.\n');
    versions.node = process.versions.node;
    versions.rutis = '0.10.0';
  }
  const files = {};
  const inventory = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const absolute = path.join(directory, entry.name);
      const relative = path.relative(output, absolute).split(path.sep).join('/');
      if (entry.isDirectory()) inventory(absolute);
      else if (entry.isFile()) files[relative] = digest(readFileSync(absolute));
      else if (entry.isSymbolicLink()) {
        // npm-generated bin links are confined to the installed runtime by npm.
        assert(relative.startsWith('runtime/rutis/node_modules/.bin/'), 'Unexpected release symlink');
      } else assert.fail('Unexpected release file type');
    }
  };
  inventory(output);
  writeFileSync(path.join(output, 'release-manifest.json'), `${JSON.stringify({schemaVersion:1,version,sourceCommit,platform,toolchain:versions,plugin,files}, null, 2)}\n`);
  console.log(`Staged ${platform} ${version} (${plugin ? 'with private plugin' : 'API key only'}).`);
} catch (error) {
  rmSync(output, { recursive: true, force: true });
  throw error;
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
