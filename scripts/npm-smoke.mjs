// Real install-and-execute E2E. No WeChat access or user identity is needed.
import { mkdirSync, mkdtempSync, writeFileSync, readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { version } from './npm-package.mjs';
const platform = `${process.platform}-${process.arch}`;
const evidence = path.resolve('npm-smoke');
mkdirSync(evidence, { recursive: true });
const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
function run(args, cwd = process.cwd()) {
  const r = spawnSync(npm, args, { cwd, encoding: 'utf8', shell: process.platform === 'win32' });
  if (r.status !== 0) throw new Error(`${args.join(' ')}\n${r.stdout}\n${r.stderr}`);
  return r.stdout;
}
const tarballs = [];
for (const name of [`wxc-${platform}`, 'wxc']) {
  const out = JSON.parse(run(['pack', `./npm-dist/${name}`, '--json', '--pack-destination', evidence]));
  tarballs.push(path.join(evidence, out[0].filename));
}
const install = mkdtempSync(path.join(evidence, 'install-'));
writeFileSync(path.join(install, 'package.json'), '{"private":true}\n');
run(['install', '--offline', '--ignore-scripts', '--no-audit', '--no-fund', ...tarballs], install);
const bin = path.join(install, 'node_modules', '.bin', process.platform === 'win32' ? 'wxc.cmd' : 'wxc');
const results = [];
for (const args of [['--version'], ['--help'], ['--invalid-option']]) {
  const r = spawnSync(bin, args, { encoding: 'utf8', shell: process.platform === 'win32' });
  results.push({ args, status: r.status, stdout: r.stdout, stderr: r.stderr });
  if (args[0] === '--version' && (r.status !== 0 || r.stdout.trim() !== `wxc ${version}`)) throw new Error(JSON.stringify(results));
  if (args[0] === '--help' && (r.status !== 0 || !r.stdout.includes('Usage:'))) throw new Error(JSON.stringify(results));
  if (args[0] === '--invalid-option' && r.status !== 2) throw new Error('Launcher did not preserve clap error exit code');
}
// An omitted optional package must give an actionable failure, not silently run another version.
const manifest = path.join(install, 'node_modules', '@mkdir700', `wxc-${platform}`, 'package.json');
const p = JSON.parse(readFileSync(manifest)); p.version = '0.0.0'; writeFileSync(manifest, JSON.stringify(p));
const mismatch = spawnSync(bin, ['--version'], { encoding: 'utf8', shell: process.platform === 'win32' });
if (mismatch.status !== 1 || !mismatch.stderr.includes('version mismatch')) throw new Error('Mismatched binary was accepted');
writeFileSync(path.join(evidence, 'results.json'), JSON.stringify({ platform, version, results, mismatch: { status: mismatch.status, stderr: mismatch.stderr } }, null, 2));
console.log(`npm install + wxc E2E passed: ${platform} ${version}`);
