import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createHash } from 'node:crypto';
import { run, capture, json } from './common.mjs';
import { assertNativeArchitecture } from './native-build.mjs';
import { collectSherpaNotices, writeNotices } from './third-party-notices.mjs';

// GitHub's official v1.12.28 release asset SHA-256 metadata. CPU builds include TTS.
// https://api.github.com/repos/k2-fsa/sherpa-onnx/releases/tags/v1.12.28
import sherpaAssets from './sherpa-assets.json' with { type: 'json' };
export { sherpaAssets };
const sha256 = data => createHash('sha256').update(data).digest('hex');
export async function copySherpaRuntime(runtime) {
  const asset = sherpaAssets[`${process.platform}-${process.arch}`];
  if (!asset) throw new Error('No pinned Sherpa ONNX distribution for this native target.');
  const [name, checksum] = asset;
  const cache = path.join(process.env.XDG_CACHE_HOME || path.join(os.homedir(), '.cache'), 'paseo-flutter-packaging', 'sherpa-onnx-1.12.28');
  fs.mkdirSync(cache, { recursive: true });
  const archive = path.join(cache, name);
  let data = fs.existsSync(archive) ? fs.readFileSync(archive) : null;
  if (!data || sha256(data) !== checksum) {
    const response = await fetch(`https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.12.28/${name}`);
    if (!response.ok) throw new Error(`Sherpa ONNX download failed: HTTP ${response.status}`);
    data = Buffer.from(await response.arrayBuffer());
    if (sha256(data) !== checksum) throw new Error(`Sherpa ONNX checksum mismatch: ${name}`);
    fs.writeFileSync(archive, data);
  }
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'paseo-sherpa-'));
  const destination = path.join(runtime, 'lib');
  fs.mkdirSync(destination, { recursive: true });
  try {
    run('tar', ['-xf', archive, '-C', temporary]);
    const libraries = new Map();
    function collect(directory) {
      for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
        const source = path.join(directory, entry.name);
        if (entry.isDirectory()) collect(source);
        else if (/\.(dll|dylib)$|\.so(?:\.[\d.]+)?$/.test(entry.name)) {
          const real = fs.realpathSync(source);
          if (!real.startsWith(temporary + path.sep)) throw new Error(`External library link in Sherpa archive: ${source}`);
          const hash = sha256(fs.readFileSync(real));
          if (libraries.has(entry.name) && libraries.get(entry.name) !== hash) throw new Error(`Conflicting Sherpa library: ${entry.name}`);
          libraries.set(entry.name, hash);
          fs.copyFileSync(real, path.join(destination, entry.name));
        } else if (/^(LICENSE|COPYING|NOTICE)(?:[._-].*)?$/i.test(entry.name)) {
          const relative = path.relative(temporary, source);
          const output = path.join(destination, 'licenses', relative);
          fs.mkdirSync(path.dirname(output), { recursive: true });
          fs.copyFileSync(source, output);
        }
      }
    }
    collect(temporary);
    const api = process.platform === 'win32' ? 'sherpa-onnx-c-api.dll' : process.platform === 'darwin' ? 'libsherpa-onnx-c-api.dylib' : 'libsherpa-onnx-c-api.so';
    if (!libraries.has(api) || ![...libraries.keys()].some(name => /onnxruntime/.test(name))) throw new Error('Incomplete Sherpa C API/ONNX Runtime library distribution.');
    for (const name of libraries.keys()) {
      const file = path.join(destination, name);
      if (process.platform === 'linux') run('patchelf', ['--set-rpath', '$ORIGIN', file]);
      if (process.platform === 'darwin') {
        const cpu = process.arch === 'x64' ? 'x86_64' : 'arm64';
        const architectures = capture('lipo', ['-archs', file]).split(/\s+/);
        if (!architectures.includes(cpu)) throw new Error(`Sherpa library lacks ${cpu}: ${name}`);
        if (architectures.length > 1) {
          run('lipo', [file, '-thin', cpu, '-output', `${file}.thin`]);
          fs.renameSync(`${file}.thin`, file);
        }
        const dependencies = capture('otool', ['-L', file]).split('\n').slice(1).map(line => line.trim().split(' (')[0]);
        run('install_name_tool', ['-id', `@loader_path/${name}`, file]);
        for (const dependency of dependencies) {
          if (libraries.has(path.basename(dependency))) run('install_name_tool', ['-change', dependency, `@loader_path/${path.basename(dependency)}`, file]);
        }
      }
      assertNativeArchitecture(file);
    }
    const notices = collectSherpaNotices(sherpaAssets);
    writeNotices(destination, notices, 'SPEECH-THIRD-PARTY-NOTICES');
    console.warn('Speech libraries retain unresolved redistribution/source-delivery requirements; see SPEECH-THIRD-PARTY-NOTICES.txt.');
    json(path.join(destination, 'sherpa-provenance.json'), {
      version: '1.12.28', asset: name, sha256: checksum, platform: process.platform, arch: process.arch,
      libraries: [...libraries.keys()].sort().map(name => ({ name, sha256: sha256(fs.readFileSync(path.join(destination, name))) })),
    });
  } finally {
    fs.rmSync(temporary, { recursive: true, force: true });
  }
}
