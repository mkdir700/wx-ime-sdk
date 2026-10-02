import { readFileSync, writeFileSync, mkdirSync, copyFileSync, chmodSync } from 'node:fs';
import path from 'node:path';

export const platforms = {
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'linux-arm64': 'aarch64-unknown-linux-gnu',
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'win32-x64': 'x86_64-pc-windows-msvc',
};
export const version = readFileSync(new URL('../Cargo.toml', import.meta.url), 'utf8')
  .match(/^version = "([^"]+)"/m)[1];
if (!/^\d+\.\d+\.\d+(?:-(?:alpha|beta|rc)\.\d+)?$/.test(version)) throw new Error('Unsupported release version');
export const distTag = version.includes('-') ? version.split('-')[1].split('.')[0] : 'latest';
const common = {
  version, description: 'WeChat IME clipboard command line client',
  repository: { type: 'git', url: 'git+https://github.com/mkdir700/wx-ime-sdk.git' },
  license: 'Apache-2.0', publishConfig: { access: 'public' },
};
const [command, platform, binary] = process.argv.slice(2);
if (command === 'metadata') {
  console.log(`version=${version}\ndist_tag=${distTag}`);
} else if (command === 'build') {
  if (!platforms[platform]) throw new Error(`Unsupported platform: ${platform}`);
  const out = path.resolve('npm-dist');
  const dir = path.join(out, `wxc-${platform}`);
  mkdirSync(path.join(dir, 'bin'), { recursive: true });
  const exe = platform.startsWith('win32') ? 'wxc.exe' : 'wxc';
  copyFileSync(binary, path.join(dir, 'bin', exe));
  chmodSync(path.join(dir, 'bin', exe), 0o755);
  copyFileSync('LICENSE', path.join(dir, 'LICENSE'));
  const [os, cpu] = platform.split('-');
  writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ ...common,
    name: `@mkdir700/wxc-${platform}`, os: [os], cpu: [cpu],
    ...(os === 'linux' ? { libc: ['glibc'] } : {}), files: ['bin', 'LICENSE'],
  }, null, 2) + '\n');
  const main = path.join(out, 'wxc');
  mkdirSync(main, { recursive: true });
  copyFileSync('npm/wxc/launcher.mjs', path.join(main, 'launcher.mjs'));
  chmodSync(path.join(main, 'launcher.mjs'), 0o755);
  copyFileSync('npm/README.md', path.join(main, 'README.md'));
  copyFileSync('LICENSE', path.join(main, 'LICENSE'));
  writeFileSync(path.join(main, 'package.json'), JSON.stringify({ ...common,
    name: '@mkdir700/wxc', type: 'module', bin: { wxc: 'launcher.mjs' },
    engines: { node: '>=22' }, files: ['launcher.mjs', 'README.md', 'LICENSE'],
    optionalDependencies: Object.fromEntries(Object.keys(platforms).map(p => [`@mkdir700/wxc-${p}`, version])),
  }, null, 2) + '\n');
} else if (command) {
  throw new Error(`Unknown command: ${command}`);
}
