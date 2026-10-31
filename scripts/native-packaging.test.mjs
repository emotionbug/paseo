import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { releaseIdentity, assertNativeArchitecture, nativeBuildDirectory, nativeExecutableName, readNativeBuild, digest } from '../packages/flutter_desktop_host/packaging/native-build.mjs';

const releaseEnv = {
  PASEO_NATIVE_RELEASE: '1',
  PASEO_UPDATE_REPOSITORY: 'example/paseo',
  PASEO_RELEASE_CHANNEL: 'stable',
  PASEO_BUILD_SOURCE_SHA: 'a'.repeat(40),
};

function nativeHeader(foreign = false) {
  const bytes = Buffer.alloc(256);
  const x64 = (process.arch === 'x64') !== foreign;
  if (process.platform === 'linux') {
    Buffer.from([0x7f, 0x45, 0x4c, 0x46, 2, 1]).copy(bytes);
    bytes.writeUInt16LE(x64 ? 62 : 183, 18);
  } else if (process.platform === 'darwin') {
    bytes.writeUInt32LE(0xfeedfacf, 0);
    bytes.writeUInt32LE(x64 ? 0x01000007 : 0x0100000c, 4);
  } else if (process.platform === 'win32') {
    bytes.write('MZ');
    bytes.writeUInt32LE(64, 0x3c);
    bytes.writeUInt32LE(0x4550, 64);
    bytes.writeUInt16LE(x64 ? 0x8664 : 0xaa64, 68);
    bytes.writeUInt16LE(0x20b, 88);
  }
  return bytes;
}

test('release identity requires an explicit repository and verified release inputs', () => {
  assert.equal(releaseIdentity(releaseEnv).repository, 'example/paseo');
  for (const name of Object.keys(releaseEnv)) {
    const env = { ...releaseEnv };
    delete env[name];
    assert.throws(() => releaseIdentity(env));
  }
  assert.throws(() => releaseIdentity({ ...releaseEnv, PASEO_NATIVE_RELEASE: '0' }));
  assert.throws(() => releaseIdentity({ ...releaseEnv, PASEO_UPDATE_REPOSITORY: 'https://github.com/example/paseo' }));
  assert.throws(() => releaseIdentity({ ...releaseEnv, PASEO_BUILD_SOURCE_SHA: 'main' }));
  assert.throws(() => releaseIdentity({ ...releaseEnv, PASEO_RELEASE_CHANNEL: 'latest' }));
});
test('native architecture validation rejects JavaScript shims and foreign CPU payloads', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'paseo-native-contract-'));
  const file = path.join(directory, 'payload');
  try {
    fs.writeFileSync(file, '#!/usr/bin/env node\n');
    assert.throws(() => assertNativeArchitecture(file), /Expected a native/);
    fs.writeFileSync(file, nativeHeader(true));
    assert.throws(() => assertNativeArchitecture(file), /Expected a native/);
  } finally { fs.rmSync(directory, { recursive: true, force: true }); }
});

test('native profiles select matching artifacts independently of publication provenance', t => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'paseo-native-profiles-'));
  const names = [...Object.keys(releaseEnv), 'CARGO_TARGET_DIR', 'PASEO_NATIVE_EXECUTABLE'];
  const previous = Object.fromEntries(names.map(name => [name, process.env[name]]));
  t.after(() => {
    for (const name of names) {
      if (previous[name] === undefined) delete process.env[name];
      else process.env[name] = previous[name];
    }
    fs.rmSync(directory, { recursive: true, force: true });
  });
  for (const name of names) delete process.env[name];
  process.env.CARGO_TARGET_DIR = directory;
  const publishedIdentity = releaseIdentity(releaseEnv);
  for (const profile of ['debug', 'release']) {
    const output = nativeBuildDirectory(profile);
    assert.equal(output, path.join(directory, profile));
    fs.mkdirSync(output);
    const executable = path.join(output, nativeExecutableName);
    fs.writeFileSync(executable, nativeHeader());
    fs.writeFileSync(path.join(output, 'paseo-build.json'), JSON.stringify({
      schemaVersion: 1, runtime: 'rust', ...publishedIdentity,
      channel: null, repository: null, release: false, profile,
      executableSha256: digest(executable),
    }));
    const build = readNativeBuild({ profile });
    assert.equal(build.executable, executable);
    assert.equal(build.identity.release, false);
  }
  assert.throws(() => nativeBuildDirectory('unknown'), /Unknown native build profile/);
  assert.equal(readNativeBuild().identity.profile, 'debug');
  assert.throws(() => readNativeBuild({ requireRelease: true }), /PASEO_NATIVE_RELEASE=1/);

  process.env.PASEO_NATIVE_EXECUTABLE = path.join(nativeBuildDirectory('debug'), nativeExecutableName);
  assert.throws(() => readNativeBuild({ profile: 'release' }), /requested release profile/);
  delete process.env.PASEO_NATIVE_EXECUTABLE;

  Object.assign(process.env, releaseEnv);
  assert.throws(() => readNativeBuild(), /exact release provenance/);
  const executable = path.join(nativeBuildDirectory('release'), nativeExecutableName);
  const manifest = path.join(nativeBuildDirectory('release'), 'paseo-build.json');
  fs.writeFileSync(manifest, JSON.stringify({
    schemaVersion: 1, runtime: 'rust', ...publishedIdentity,
    release: true, profile: 'release', executableSha256: digest(executable),
  }));
  assert.equal(readNativeBuild({ requireRelease: true }).identity.release, true);
  assert.throws(() => readNativeBuild({ profile: 'debug' }), /exact release provenance/);
  process.env.PASEO_BUILD_SOURCE_SHA = 'b'.repeat(40);
  assert.throws(() => readNativeBuild({ requireRelease: true }), /exact release provenance/);
});

