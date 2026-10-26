#!/usr/bin/env node
import { createRequire } from 'node:module';
import { spawn } from 'node:child_process';
import path from 'node:path';

const platform = `${process.platform === 'win32' ? 'windows' : process.platform}-${process.arch}`;
if (!['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'windows-arm64', 'windows-x64'].includes(platform)) {
  console.error(`dimcode does not support ${platform}.`);
  process.exit(1);
}
let binary;
try {
  const metadata = createRequire(import.meta.url).resolve(`dimcode-${platform}/package.json`);
  binary = path.join(path.dirname(metadata), 'bin', process.platform === 'win32' ? 'dimcode.exe' : 'dimcode');
} catch {
  console.error(`Missing dimcode-${platform}. Reinstall dimcode with npm optional dependencies enabled.`);
  process.exit(1);
}
// The CLI tells its SKUs apart by the package it was started from.
const { name } = createRequire(import.meta.url)('../package.json');
const env = { ...process.env, DIMCODE_NPM_PACKAGE: name };
// The Web workspace is the optional dimcode-web package; an installed one is
// named to the binary unless the user points at another frontend.
if (env.DIMCODE_WEB_ASSETS === undefined) {
  try {
    const web = createRequire(import.meta.url).resolve('dimcode-web/package.json');
    env.DIMCODE_WEB_ASSETS = path.join(path.dirname(web), 'web');
  } catch {
    // Not installed: `dimcode web` says how to get it.
  }
}
// The repository's TypeScript plugins are the optional dimcode-plugins package:
// its default config file, the rutis runtime it depends on, and this Node.
if (env.DIMCODE_PLUGIN_DEFAULTS === undefined) {
  try {
    const plugins = path.dirname(createRequire(import.meta.url).resolve('dimcode-plugins/package.json'));
    env.DIMCODE_PLUGIN_DEFAULTS = path.join(plugins, 'plugins.default.json');
    env.DIMCODE_JS_RUNTIME ??= path.dirname(createRequire(path.join(plugins, 'package.json')).resolve('@arcships/rutis-runtime/package.json'));
    env.DIMCODE_NODE ??= process.execPath;
  } catch {
    // Not installed: the App Server runs only the plugins compiled into it.
  }
}
// `process.execve` came with Node 22.15 and 23.11; an older Node, which npm
// installs under with only an engines warning, spawns like Windows does.
if (process.platform !== 'win32' && typeof process.execve === 'function') {
  // Replacing the launcher preserves terminal ownership and delivers each signal once.
  try {
    process.execve(binary, [binary, ...process.argv.slice(2)], env);
  } catch (error) {
    console.error(`Cannot start dimcode: ${error.message}`);
    process.exit(1);
  }
}
const child = spawn(binary, process.argv.slice(2), { stdio: 'inherit', env });
// The Windows console sends Ctrl+C to both processes; forwarding would interrupt twice.
const onInterrupt = () => {};
const onTerminate = () => child.kill('SIGTERM');
process.on('SIGINT', onInterrupt);
process.on('SIGTERM', onTerminate);
child.on('error', (error) => {
  console.error(`Cannot start dimcode: ${error.message}`);
  process.exitCode = 1;
});
child.on('exit', (code, signal) => {
  process.removeListener('SIGINT', onInterrupt);
  process.removeListener('SIGTERM', onTerminate);
  process.exitCode = signal ? 1 : code ?? 1;
});
