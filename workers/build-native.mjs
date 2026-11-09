import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.dirname(fileURLToPath(import.meta.url));
const os = { darwin: 'macos', win32: 'windows', linux: 'linux' }[process.platform];
if (!os) throw new Error(`Unsupported desktop OS: ${process.platform}`);
const bin = path.join(root, 'native', 'bin', os);
fs.mkdirSync(bin, { recursive: true });
let result;
if (os === 'macos') {
  const architecture = process.arch === 'arm64' ? 'arm64' : 'x86_64';
  result = spawnSync('swiftc', ['-O', '-target', `${architecture}-apple-macosx14.0`, path.join(root, 'native/macos/DesktopHelper.swift'), '-o', path.join(bin, 'harness-desktop')], { stdio: 'inherit' });
} else {
  const cargo = process.env.CARGO || 'cargo';
  result = spawnSync(cargo, ['build', '--release', '--manifest-path', path.join(root, 'native/Cargo.toml')], { stdio: 'inherit' });
  if (result.status === 0) {
    const filename = os === 'windows' ? 'harness-desktop.exe' : 'harness-desktop';
    fs.copyFileSync(path.join(root, 'native/target/release', filename), path.join(bin, filename));
  }
}
if (result.error) throw result.error;
process.exit(result.status ?? 1);
