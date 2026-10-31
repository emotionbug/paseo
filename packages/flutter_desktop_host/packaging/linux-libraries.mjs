import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { capture } from './common.mjs';

// Keep glibc and graphics-driver dispatch on the host. Every other linked
// library (including native audio/tray plugins and capture commands) travels
// inside the AppImage built on the Ubuntu 24.04 baseline.
const hostLibrary = /^(ld-linux|lib(c|m|pthread|dl|rt|resolv|nss_[^.]+|anl|util)\.so|lib(GL|GLX|EGL|GLdispatch|drm|gbm)[-.])/;

// Standalone and desktop daemon roots must be relocatable without an AppRun
// LD_LIBRARY_PATH wrapper. Resolve the same host-library policy into sibling lib/.
export function bundleNativeLinuxLibraries(runtime) {
  const destination = path.join(runtime, 'lib');
  const seen = new Set();
  const copies = new Map();
  function linked(file) {
    const real = fs.realpathSync(file);
    if (seen.has(real)) return;
    seen.add(real);
    const descriptor = fs.openSync(real, 'r');
    const header = Buffer.alloc(20);
    const count = fs.readSync(descriptor, header, 0, header.length, 0);
    fs.closeSync(descriptor);
    if (count !== header.length || header.subarray(0, 6).toString('hex') !== '7f454c460201') return;
    if (header.readUInt16LE(18) !== (process.arch === 'arm64' ? 183 : 62)) throw new Error(`Foreign native runtime library: ${real}`);
    const inspection = spawnSync('ldd', [real], { encoding: 'utf8', env: { ...process.env, LC_ALL: 'C' } });
    if (inspection.error) throw inspection.error;
    const result = `${inspection.stdout || ''}\n${inspection.stderr || ''}`;
    if (/not a dynamic executable|statically linked/.test(result)) return;
    if (inspection.status !== 0 || result.includes('=> not found')) throw new Error(`Unresolved native runtime libraries for ${real}:\n${result}`);
    for (const line of result.split('\n')) {
      const source = line.match(/=>\s+(\/[^\s]+)\s+\(/)?.[1];
      if (!source || source.startsWith(runtime + path.sep) || hostLibrary.test(path.basename(source))) continue;
      const output = path.join(destination, path.basename(source));
      if (!fs.existsSync(output)) {
        fs.copyFileSync(fs.realpathSync(source), output);
        copies.set(output, source);
        linked(output);
      }
    }
    const relative = path.relative(path.dirname(file), destination).split(path.sep).join('/');
    const search = capture('patchelf', ['--print-rpath', file]).split(':').filter(value => value.startsWith('$ORIGIN'));
    search.push(relative ? `$ORIGIN/${relative}` : '$ORIGIN');
    capture('patchelf', ['--set-rpath', [...new Set(search)].join(':'), file]);
  }
  function walk(directory) {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const file = path.join(directory, entry.name);
      if (entry.isDirectory()) walk(file);
      else if (entry.isFile()) linked(file);
    }
  }
  walk(runtime);
  const notices = path.join(destination, 'licenses', 'system');
  fs.mkdirSync(notices, { recursive: true });
  for (const source of copies.values()) {
    const owner = capture('dpkg-query', ['-S', `*/${path.basename(source)}`]).split('\n')[0];
    const packageName = owner.slice(0, owner.indexOf(': ')).split(':')[0];
    const copyright = path.join('/usr/share/doc', packageName, 'copyright');
    if (!fs.existsSync(copyright)) throw new Error(`Missing redistribution notice for ${source}`);
    fs.copyFileSync(copyright, path.join(notices, `${packageName}.copyright`));
  }
}
export function bundleLinuxLibraries(appdir) {
  const destination = path.join(appdir, 'usr/lib/paseo-dependencies');
  fs.mkdirSync(destination, { recursive: true });
  const executables = path.join(appdir, 'usr/bin');
  fs.mkdirSync(executables, { recursive: true });
  for (const name of ['ffmpeg', 'parecord', 'pactl', 'xdg-email', 'xdg-open', 'xdg-mime', 'xdg-settings']) {
    const source = (process.env.PATH || '').split(path.delimiter).map(directory => path.join(directory, name)).find(file => fs.existsSync(file));
    if (!source) throw new Error(`Install ffmpeg, pulseaudio-utils and xdg-utils before AppImage packaging (missing ${name}).`);
    fs.copyFileSync(fs.realpathSync(source), path.join(executables, name));
    fs.chmodSync(path.join(executables, name), 0o755);
  }
  const seen = new Set();
  function linked(file) {
    const real = fs.realpathSync(file);
    if (seen.has(real)) return;
    seen.add(real);
    const descriptor = fs.openSync(real, 'r');
    const header = Buffer.alloc(20);
    const count = fs.readSync(descriptor, header, 0, header.length, 0);
    fs.closeSync(descriptor);
    if (count !== header.length || header.subarray(0, 4).toString('hex') !== '7f454c46') return;
    // Both supported hosts use ELF64 little-endian headers. Skip foreign
    // classes/byte orders before decoding e_machine at its ELF header offset.
    if (header[4] !== 2 || header[5] !== 1) return;
    const machine = header.readUInt16LE(18);
    if (machine !== (process.arch === 'arm64' ? 183 : 62)) return;
    const inspection = spawnSync('ldd', [real], { encoding: 'utf8', env: { ...process.env, LC_ALL: 'C' } });
    if (inspection.error) throw inspection.error;
    const result = `${inspection.stdout || ''}\n${inspection.stderr || ''}`;
    if (/not a dynamic executable|statically linked/.test(result)) return;
    if (inspection.status !== 0) throw new Error(`Cannot resolve native libraries for ${real}:\n${result}`);
    if (result.includes('=> not found')) throw new Error(`Unresolved native AppImage library for ${real}:\n${result}`);
    for (const line of result.split('\n')) {
      const source = line.match(/=>\s+(\/[^\s]+)\s+\(/)?.[1];
      if (!source || source.startsWith(appdir + path.sep) || hostLibrary.test(path.basename(source))) continue;
      const output = path.join(destination, path.basename(source));
      if (!fs.existsSync(output)) fs.copyFileSync(fs.realpathSync(source), output);
    }
  }
  function walk(directory) {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const file = path.join(directory, entry.name);
      if (entry.isDirectory()) walk(file);
      else if (entry.isFile()) linked(file);
    }
  }
  walk(path.join(appdir, 'usr/lib/paseo-sessions'));
  walk(executables);
  // Preserve the distribution's license notices for the redistributed system
  // libraries and audio programs rather than silently stripping their notices.
  const notices = path.join(appdir, 'usr/share/doc/paseo-bundled-dependencies');
  fs.mkdirSync(notices, { recursive: true });
  for (const file of fs.readdirSync(destination)) {
    const source = capture('dpkg-query', ['-S', `*/${file}`]).split('\n')[0];
    const packageName = source.slice(0, source.indexOf(': ')).split(':')[0];
    const copyright = path.join('/usr/share/doc', packageName, 'copyright');
    if (fs.existsSync(copyright)) fs.copyFileSync(copyright, path.join(notices, `${packageName}.copyright`));
  }
  for (const packageName of ['ffmpeg', 'pulseaudio-utils', 'xdg-utils']) {
    fs.copyFileSync(path.join('/usr/share/doc', packageName, 'copyright'), path.join(notices, `${packageName}.copyright`));
  }
}
