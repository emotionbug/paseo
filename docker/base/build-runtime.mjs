import fs from 'node:fs';
import path from 'node:path';
import { root, run, json, version, platform, arch, requireNode24 } from '../../packages/flutter_desktop_host/packaging/common.mjs';
import { releaseIdentity, nativeBuildDirectory, digest, assertNativeArchitecture } from '../../packages/flutter_desktop_host/packaging/native-build.mjs';
import { copySherpaRuntime } from '../../packages/flutter_desktop_host/packaging/sherpa-runtime.mjs';
import { bundleRuntime } from '../../packages/flutter_desktop_host/packaging/runtime.mjs';
import { cargoMetadata, collectCargoNotices, writeNotices } from '../../packages/flutter_desktop_host/packaging/third-party-notices.mjs';

requireNode24();
if (platform !== 'linux' || process.argv.length !== 3) throw new Error('Usage: node docker/base/build-runtime.mjs DESTINATION (Linux target host required)');
if (process.env.CARGO_BUILD_TARGET) throw new Error('Container builds must run on their target architecture, not cross-compile.');
if (process.env.PASEO_VERSION && process.env.PASEO_VERSION !== version) throw new Error('PASEO_VERSION must match the source package version.');
// Docker deliberately excludes .git. The caller supplies the checked-out source
// identity; published builds also require explicit repository/channel metadata.
const sourceSha = process.env.PASEO_BUILD_SOURCE_SHA;
if (!/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/i.test(sourceSha || '')) throw new Error('Pass --build-arg PASEO_BUILD_SOURCE_SHA=<source commit>.');
const release = process.env.PASEO_NATIVE_RELEASE === '1';
const identity = release ? releaseIdentity() : { version, platform, arch, sourceSha, channel: null, repository: null };
if (!release) {
  delete process.env.PASEO_NATIVE_RELEASE;
  delete process.env.PASEO_UPDATE_REPOSITORY;
  delete process.env.PASEO_RELEASE_CHANNEL;
}
const directory = nativeBuildDirectory('release');
const executable = path.join(directory, 'paseo');
fs.rmSync(path.join(directory, 'paseo-build.json'), { force: true });
run('cargo', ['build', '--locked', '--release', '-p', 'paseo-daemon', '--bin', 'paseo']);
assertNativeArchitecture(executable);
for (const name of ['lib', 'skills', 'plugin-worker']) fs.rmSync(path.join(directory, name), { recursive: true, force: true });
await copySherpaRuntime(directory);
fs.cpSync(path.join(root, 'skills'), path.join(directory, 'skills'), { recursive: true, dereference: true });
run(process.execPath, [path.join(root, 'scripts/build-plugin-worker.mjs'), path.join(directory, 'plugin-worker')]);
// Resolve notices while this build stage still has Cargo's registry/vendor
// sources and the copied workspace. The collector is locked, offline, and
// host-target-filtered; incomplete source/license inputs fail packaging.
writeNotices(directory, collectCargoNotices(cargoMetadata()), 'RUST-THIRD-PARTY-NOTICES');
json(path.join(directory, 'paseo-build.json'), {
  schemaVersion: 1, runtime: 'rust', ...identity, release, profile: 'release', executableSha256: digest(executable),
});
process.env.PASEO_NATIVE_EXECUTABLE = executable;
await bundleRuntime(path.resolve(process.argv[2]), { installationKind: 'container', profile: 'release' });
