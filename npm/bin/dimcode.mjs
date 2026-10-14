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
if (process.platform !== 'win32') {
  // Replacing the launcher preserves terminal ownership and delivers each signal once.
  try {
    process.execve(binary, [binary, ...process.argv.slice(2)], process.env);
  } catch (error) {
    console.error(`Cannot start dimcode: ${error.message}`);
    process.exit(1);
  }
}
const child = spawn(binary, process.argv.slice(2), { stdio: 'inherit' });
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
