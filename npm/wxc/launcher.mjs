#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import path from 'node:path';
const platform = `${process.platform}-${process.arch}`;
const supported = ['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win32-x64'];
if (!supported.includes(platform)) {
  console.error(`wxc: unsupported platform ${platform}; supported: ${supported.join(', ')}`);
  process.exit(1);
}
if (process.platform === 'linux' && !process.report.getReport().header.glibcVersionRuntime) {
  console.error('wxc: this prebuilt Linux package requires glibc (musl/Alpine is not supported).');
  process.exit(1);
}
const require = createRequire(import.meta.url);
let binary;
try {
  const manifest = require.resolve(`@mkdir700/wxc-${platform}/package.json`);
  const installed = require(manifest);
  const main = require('./package.json');
  if (installed.version !== main.version) throw new Error('platform package version mismatch');
  binary = path.join(path.dirname(manifest), 'bin', process.platform === 'win32' ? 'wxc.exe' : 'wxc');
} catch (error) {
  console.error(`wxc: ${error.message}\nReinstall @mkdir700/wxc with optional dependencies enabled.`);
  process.exit(1);
}
const result = spawnSync(binary, process.argv.slice(2), { stdio: 'inherit' });
if (result.error) { console.error(`wxc: ${result.error.message}`); process.exit(1); }
if (result.signal) process.kill(process.pid, result.signal);
process.exit(result.status ?? 1);
