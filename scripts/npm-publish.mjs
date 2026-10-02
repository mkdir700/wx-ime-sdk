import { createHash } from 'node:crypto';
import { readFileSync, readdirSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { platforms, version, distTag } from './npm-package.mjs';
if (process.env.VERSION !== version || process.env.DIST_TAG !== distTag) throw new Error('Release metadata mismatch');
const dir = 'npm-dist-tarballs';
const names = [...Object.keys(platforms).map(p => `wxc-${p}`), 'wxc'];
if (readdirSync(dir).length !== names.length) throw new Error('Expected exactly six verified tarballs');
for (const name of names) {
  const file = `${dir}/mkdir700-${name}-${version}.tgz`;
  const integrity = 'sha512-' + createHash('sha512').update(readFileSync(file)).digest('base64');
  const existing = spawnSync('npm', ['view', `@mkdir700/${name}@${version}`, 'dist.integrity', '--json'], { encoding: 'utf8' });
  if (existing.status === 0) {
    if (JSON.parse(existing.stdout) !== integrity) throw new Error(`Published bytes differ: ${name}@${version}; do not overwrite a release`);
    console.log(`Already published, integrity matches: ${name}`);
    continue;
  }
  // Registry/network/auth failures must not be mistaken for an unpublished version.
  let error;
  try { error = JSON.parse(existing.stdout).error; } catch { /* fail below */ }
  if (error?.code !== 'E404') throw new Error(existing.stderr || existing.stdout);
  const result = spawnSync('npm', ['publish', file, '--access', 'public', '--provenance', '--tag', distTag], { stdio: 'inherit' });
  if (result.status !== 0) process.exit(result.status ?? 1);
}
