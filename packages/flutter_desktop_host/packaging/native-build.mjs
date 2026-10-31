import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { root, version, platform, arch } from './common.mjs';

export const nativeExecutableName = process.platform === 'win32' ? 'paseo.exe' : 'paseo';
export function releaseIdentity(env = process.env) {
  if (env.PASEO_NATIVE_RELEASE !== '1') throw new Error('Release packaging requires PASEO_NATIVE_RELEASE=1. Local builds are not verified releases.');
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(env.PASEO_UPDATE_REPOSITORY || '')) throw new Error('Set PASEO_UPDATE_REPOSITORY explicitly to the release owner/repository.');
  if (!['stable', 'beta'].includes(env.PASEO_RELEASE_CHANNEL)) throw new Error('Set PASEO_RELEASE_CHANNEL to stable or beta.');
  if (!/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/i.test(env.PASEO_BUILD_SOURCE_SHA || '')) throw new Error('Set PASEO_BUILD_SOURCE_SHA to the exact release commit.');
  return { version, platform, arch, sourceSha: env.PASEO_BUILD_SOURCE_SHA, channel: env.PASEO_RELEASE_CHANNEL, repository: env.PASEO_UPDATE_REPOSITORY };
}
export function nativeBuildDirectory(profile) {
  if (!['debug', 'release'].includes(profile)) throw new Error(`Unknown native build profile: ${profile}`);
  return path.join(path.resolve(root, process.env.CARGO_TARGET_DIR || 'target'), profile);
}
export function digest(file, algorithm = 'sha256', encoding = 'hex') {
  return createHash(algorithm).update(fs.readFileSync(file)).digest(encoding);
}
export function assertNativeArchitecture(file) {
  const bytes = fs.readFileSync(file);
  let matches = false;
  if (platform === 'linux' && bytes.length >= 64 && bytes.subarray(0, 6).equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46, 2, 1]))) {
    matches = bytes.readUInt16LE(18) === (arch === 'x64' ? 62 : 183);
  } else if (platform === 'macos' && bytes.length >= 32 && bytes.readUInt32LE(0) === 0xfeedfacf) {
    matches = bytes.readUInt32LE(4) === (arch === 'x64' ? 0x01000007 : 0x0100000c);
  } else if (platform === 'windows' && bytes.length >= 64 && bytes.toString('ascii', 0, 2) === 'MZ') {
    const pe = bytes.readUInt32LE(0x3c);
    matches = pe + 26 <= bytes.length && bytes.readUInt32LE(pe) === 0x4550 &&
      bytes.readUInt16LE(pe + 4) === (arch === 'x64' ? 0x8664 : 0xaa64) &&
      bytes.readUInt16LE(pe + 24) === 0x20b;
  }
  if (!matches) throw new Error(`Expected a native ${platform}-${arch} binary: ${file}`);
}
export function readNativeBuild({ requireRelease = false, profile = requireRelease || process.env.PASEO_NATIVE_RELEASE === '1' ? 'release' : 'debug' } = {}) {
  const directoryForProfile = nativeBuildDirectory(profile);
  const executable = path.resolve(process.env.PASEO_NATIVE_EXECUTABLE || path.join(directoryForProfile, nativeExecutableName));
  const directory = path.dirname(executable);
  assertNativeArchitecture(executable);
  const identity = JSON.parse(fs.readFileSync(path.join(directory, 'paseo-build.json'), 'utf8'));
  if (identity.runtime !== 'rust' || identity.version !== version || identity.platform !== platform || identity.arch !== arch || identity.executableSha256 !== digest(executable)) throw new Error('Native build identity does not match executable/version/host target. Run scripts/build-native-daemon.mjs.');
  if (identity.profile !== profile) throw new Error(`Native binary was not built with the requested ${profile} profile. Run scripts/build-native-daemon.mjs${profile === 'release' ? ' --release' : ''}.`);
  if (requireRelease || process.env.PASEO_NATIVE_RELEASE === '1') {
    const expected = releaseIdentity();
    if (profile !== 'release' || !identity.release || Object.keys(expected).some(key => identity[key] !== expected[key])) throw new Error('Native binary was not built with this exact release provenance. Rebuild with scripts/build-native-daemon.mjs --release.');
  }
  return { executable, directory, identity };
}
