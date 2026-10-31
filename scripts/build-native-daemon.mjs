import fs from 'node:fs';
import path from 'node:path';
import { root, run, capture, json, version, platform, arch, requireNode24 } from '../packages/flutter_desktop_host/packaging/common.mjs';
import { releaseIdentity, nativeBuildDirectory, nativeExecutableName, digest, assertNativeArchitecture } from '../packages/flutter_desktop_host/packaging/native-build.mjs';
import { copySherpaRuntime } from '../packages/flutter_desktop_host/packaging/sherpa-runtime.mjs';
import { cargoMetadata, collectCargoNotices, writeNotices } from '../packages/flutter_desktop_host/packaging/third-party-notices.mjs';

requireNode24();
const args = process.argv.slice(2);
if (args.some(arg => arg !== '--release') || args.length > 1) throw new Error('Usage: node scripts/build-native-daemon.mjs [--release]');
const profile = args.includes('--release') ? 'release' : 'debug';
const release = process.env.PASEO_NATIVE_RELEASE === '1';
if (release && profile !== 'release') throw new Error('PASEO_NATIVE_RELEASE=1 requires --release.');
if (!platform) throw new Error(`Unsupported native host ${process.platform}`);
if (process.env.CARGO_BUILD_TARGET) throw new Error('Build native runtimes on their target host; CARGO_BUILD_TARGET cross-compilation is not supported.');
const checkoutSha = capture('git', ['rev-parse', 'HEAD']);
const identity = release ? releaseIdentity() : {
  version, platform, arch, sourceSha: checkoutSha, channel: null, repository: null,
};
if (release && identity.sourceSha !== checkoutSha) throw new Error('PASEO_BUILD_SOURCE_SHA must identify the checked-out release source.');
const directory = nativeBuildDirectory(profile);
const executable = path.join(directory, nativeExecutableName);
// Remove the old claim before compilation: an interrupted build cannot leave a
// different executable labeled with stale verified release provenance.
fs.rmSync(path.join(directory, 'paseo-build.json'), { force: true });
const env = { ...process.env, PASEO_BUILD_SOURCE_SHA: identity.sourceSha };
if (!release) {
  delete env.PASEO_NATIVE_RELEASE;
  delete env.PASEO_UPDATE_REPOSITORY;
  delete env.PASEO_RELEASE_CHANNEL;
}
run(process.env.CARGO || 'cargo', ['build', '--locked', '-p', 'paseo-daemon', '--bin', 'paseo', ...(profile === 'release' ? ['--release'] : [])], { env });
assertNativeArchitecture(executable);
for (const member of ['lib', 'skills', 'plugin-worker']) fs.rmSync(path.join(directory, member), { recursive: true, force: true });
await copySherpaRuntime(directory);
fs.cpSync(path.join(root, 'skills'), path.join(directory, 'skills'), { recursive: true, dereference: true });
run(process.execPath, [path.join(root, 'scripts/build-plugin-worker.mjs'), path.join(directory, 'plugin-worker')]);
writeNotices(directory, collectCargoNotices(cargoMetadata()), 'RUST-THIRD-PARTY-NOTICES');
json(path.join(directory, 'paseo-build.json'), {
  schemaVersion: 1, runtime: 'rust', ...identity, release, profile,
  executableSha256: digest(executable),
});
console.log(executable);
