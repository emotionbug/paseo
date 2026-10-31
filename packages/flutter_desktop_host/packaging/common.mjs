import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import os from 'node:os';

export const root = fileURLToPath(new URL('../../../', import.meta.url));
export const app = path.join(root, 'packages/flutter_app');
export const release = path.join(root, 'artifacts/flutter');
export const version = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8')).version;
export const platform = ({ darwin: 'macos', win32: 'windows', linux: 'linux' })[process.platform];
export const arch = process.arch;
export const appId = 'sh.paseo.sessions.desktop';
export function run(command, args = [], options = {}) {
  const batch = process.platform === 'win32' && /\.(bat|cmd)$/i.test(command);
  const commandLine = batch ? `"${[command, ...args].map(value => `"${String(value).replaceAll('"', '""')}"`).join(' ')}"` : null;
  const result = spawnSync(batch ? process.env.ComSpec || 'cmd.exe' : command,
    batch ? ['/d', '/s', '/c', commandLine] : args,
    { cwd: root, stdio: 'inherit', ...options, windowsVerbatimArguments: batch });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} exited ${result.status}`);
  return result.stdout?.toString().trim();
}
export function capture(command, args, options = {}) {
  return run(command, args, { ...options, stdio: ['ignore', 'pipe', 'inherit'], encoding: 'utf8' });
}
export const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
export function requireNode24() {
  if (process.versions.node.split('.')[0] !== '24') throw new Error('Use Node.js 24 for build-time packaging and optional extension compiler assets; the daemon itself is native Rust.');
  if (!['x64', 'arm64'].includes(arch)) throw new Error(`Unsupported architecture: ${arch}`);
}
export function flutter() {
  const binary = process.platform === 'win32' ? 'flutter.bat' : 'flutter';
  const candidates = [process.env.FLUTTER_BIN,
    ...[process.env.FLUTTER_ROOT, process.env.FLUTTER_HOME,
      path.join(process.env.XDG_CACHE_HOME || path.join(os.homedir(), '.cache'), 'paseo-flutter-sdk')]
      .filter(Boolean).map(directory => path.join(directory, 'bin', binary)),
    ...(process.env.PATH || '').split(path.delimiter).map(directory => path.join(directory, binary))];
  const found = candidates.find(candidate => candidate && existsSync(candidate));
  if (!found) throw new Error('Flutter SDK not found. Set FLUTTER_BIN or FLUTTER_ROOT, install into $XDG_CACHE_HOME/paseo-flutter-sdk, or add Flutter to PATH.');
  return found;
}
export function buildNumber(target, env = process.env) {
  const value = env.PASEO_BUILD_NUMBER || env.ANDROID_VERSION_CODE || '13000';
  if (!/^\d+$/.test(value)) throw new Error('PASEO_BUILD_NUMBER must be numeric.');
  return target === 'android' ? String(Math.max(13000, Number(value))) : value;
}
export function json(file, value) {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, JSON.stringify(value, null, 2) + '\n');
}
export function artifactStem(target = platform, cpu = arch) {
  return `Paseo-Sessions-Flutter-${version}-${target}-${cpu}`;
}
