import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const assetDir = path.resolve(process.env.HARNESS_RELEASE_ASSET_DIR || path.join(root, 'release-assets'));
const bundleDir = path.resolve(process.env.HARNESS_BUNDLE_DIR || path.join(root, 'target/release/bundle'));
const { version } = JSON.parse(await fs.readFile(path.join(root, 'package.json'), 'utf8'));
const tauri = JSON.parse(await fs.readFile(path.join(root, 'apps/desktop/src-tauri/tauri.conf.json'), 'utf8'));
const cargo = await fs.readFile(path.join(root, 'Cargo.toml'), 'utf8');
const cargoVersion = cargo.match(/\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m)?.[1];
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version) || tauri.version !== version || cargoVersion !== version) {
  throw new Error('Package, Tauri, and Rust workspace versions must match.');
}
if (process.env.GITHUB_REF_TYPE === 'tag' && process.env.GITHUB_REF_NAME !== `v${version}`) {
  throw new Error(`Release tag must be v${version}.`);
}

const prefix = `Gemma-Harness-v${version}`;
const targets = {
  'macos-arm64': { platform: 'darwin', arch: 'arm64', files: [['dmg', '.dmg', `${prefix}-macOS-arm64.dmg`]] },
  'macos-x64': { platform: 'darwin', arch: 'x64', files: [['dmg', '.dmg', `${prefix}-macOS-x64.dmg`]] },
  'windows-x64': { platform: 'win32', arch: 'x64', files: [['nsis', '.exe', `${prefix}-Windows-x64-setup.exe`], ['msi', '.msi', `${prefix}-Windows-x64.msi`]] },
  'linux-x64': { platform: 'linux', arch: 'x64', files: [['deb', '.deb', `${prefix}-Linux-x64.deb`], ['appimage', '.AppImage', `${prefix}-Linux-x64.AppImage`]] },
};
const expected = Object.values(targets).flatMap(target => target.files.map(file => file[2]));

async function walk(dir) {
  const entries = await fs.readdir(dir, { withFileTypes: true });
  const result = [];
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) result.push(...await walk(full));
    else if (entry.isFile()) result.push(full);
  }
  return result;
}

async function collect(targetName) {
  const target = targets[targetName];
  if (!target) throw new Error(`Unknown release target: ${targetName}`);
  if (process.platform !== target.platform || process.arch !== target.arch) {
    throw new Error(`${targetName} requires ${target.platform}/${target.arch}; got ${process.platform}/${process.arch}.`);
  }
  await fs.mkdir(assetDir, { recursive: true });
  for (const [directory, extension, destination] of target.files) {
    const matches = (await walk(path.join(bundleDir, directory))).filter(file =>
      file.endsWith(extension) && path.basename(file).includes(version));
    if (matches.length !== 1) throw new Error(`Expected exactly one ${extension} for ${version}; found ${matches.length}.`);
    if ((await fs.stat(matches[0])).size === 0) throw new Error(`Empty package: ${matches[0]}`);
    await fs.copyFile(matches[0], path.join(assetDir, destination));
    process.stdout.write(`Collected ${destination}\n`);
  }
}

async function sha256(file) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest('hex');
}

async function verifyLocal() {
  const names = (await fs.readdir(assetDir)).filter(name => name !== 'SHA256SUMS').sort();
  if (JSON.stringify(names) !== JSON.stringify([...expected].sort())) {
    throw new Error(`Release needs exactly these six installers: ${expected.join(', ')}; found: ${names.join(', ')}`);
  }
  const sums = [];
  for (const name of expected) {
    const file = path.join(assetDir, name);
    const stat = await fs.lstat(file);
    if (!stat.isFile() || stat.size === 0) throw new Error(`Not a nonempty file: ${name}`);
    sums.push(`${await sha256(file)}  ${name}`);
  }
  await fs.writeFile(path.join(assetDir, 'SHA256SUMS'), `${sums.join('\n')}\n`);
  process.stdout.write('Verified all six installers and generated SHA256SUMS.\n');
}

async function verifyRemote() {
  const remotePath = process.env.HARNESS_RELEASE_REMOTE || path.join(root, 'release-remote.json');
  const remote = JSON.parse(await fs.readFile(remotePath, 'utf8'));
  if (!remote.isDraft || !remote.isPrerelease) throw new Error('The release must be a prerelease draft during verification.');
  const expectedNames = [...expected, 'SHA256SUMS'].sort();
  const remoteNames = remote.assets.map(asset => asset.name).sort();
  if (JSON.stringify(remoteNames) !== JSON.stringify(expectedNames)) throw new Error('Uploaded release assets do not match the expected set.');
  for (const asset of remote.assets) {
    const size = (await fs.stat(path.join(assetDir, asset.name))).size;
    if (!size || asset.size !== size) throw new Error(`Uploaded asset size mismatch: ${asset.name}`);
  }
  process.stdout.write('Verified all uploaded release assets.\n');
}

const [command, argument] = process.argv.slice(2);
if (command === 'collect') await collect(argument);
else if (command === 'verify-local') await verifyLocal();
else if (command === 'verify-remote') await verifyRemote();
else throw new Error('Usage: release-assets.mjs collect <target> | verify-local | verify-remote');
