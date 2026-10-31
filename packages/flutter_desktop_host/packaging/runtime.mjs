import fs from 'node:fs';
import path from 'node:path';
import { root, arch, platform, requireNode24, json } from './common.mjs';
import { copyNeovimRuntime } from './neovim-runtime.mjs';
import { nativeExecutableName, readNativeBuild, assertNativeArchitecture, digest } from './native-build.mjs';
import { bundleNativeLinuxLibraries } from './linux-libraries.mjs';
import { nativeThirdPartyNotices } from './third-party-notices.mjs';

export function desktopRuntimeMetadata(executable = nativeExecutableName) {
  return { schemaVersion: 1, executable, cliShimPath: executable, workingDirectory: '.', environment: {} };
}
export async function bundleRuntime(destination, { installationKind = 'desktop-bundled', profile } = {}) {
  requireNode24();
  if (!['desktop-bundled', 'standalone-archive', 'container'].includes(installationKind)) throw new Error(`Unknown native installation kind: ${installationKind}`);
  const build = readNativeBuild({ requireRelease: installationKind === 'standalone-archive', profile });
  fs.rmSync(destination, { recursive: true, force: true });
  fs.mkdirSync(destination, { recursive: true });
  fs.copyFileSync(build.executable, path.join(destination, nativeExecutableName));
  if (process.platform !== 'win32') fs.chmodSync(path.join(destination, nativeExecutableName), 0o755);
  for (const name of ['lib', 'skills', 'plugin-worker']) {
    const source = path.join(build.directory, name);
    if (!fs.existsSync(source)) throw new Error(`Missing native runtime assets: ${source}; run scripts/build-native-daemon.mjs.`);
    fs.cpSync(source, path.join(destination, name), { recursive: true, dereference: true });
  }
  for (const name of ['RUST-THIRD-PARTY-NOTICES.json', 'RUST-THIRD-PARTY-NOTICES.txt']) {
    fs.copyFileSync(path.join(build.directory, name), path.join(destination, name));
  }
  // Only the explicitly built worker/compiler asset closure is shipped; no npm
  // daemon closure or JavaScript runtime is copied from the source installation.
  const worker = path.join(destination, 'plugin-worker');
  for (const required of ['worker.mjs', 'node_modules/esbuild/package.json', 'node_modules/typescript/package.json', `node_modules/@esbuild/${process.platform}-${arch}/package.json`]) {
    if (!fs.existsSync(path.join(worker, required))) throw new Error(`Incomplete optional extension compiler assets: ${required}`);
  }
  const compilerPlatforms = fs.readdirSync(path.join(worker, 'node_modules/@esbuild'));
  if (compilerPlatforms.length !== 1 || compilerPlatforms[0] !== `${process.platform}-${arch}`) throw new Error('Plugin compiler contains a foreign architecture.');
  assertNativeArchitecture(path.join(worker, 'node_modules/@esbuild', `${process.platform}-${arch}`, process.platform === 'win32' ? 'esbuild.exe' : 'bin/esbuild'));
  if (installationKind === 'desktop-bundled') {
    await copyNeovimRuntime(destination);
    json(path.join(destination, 'desktop-runtime.json'), desktopRuntimeMetadata());
  }
  if (process.platform === 'linux') bundleNativeLinuxLibraries(destination);
  fs.copyFileSync(path.join(root, 'LICENSE'), path.join(destination, 'LICENSE'));
  fs.writeFileSync(path.join(destination, 'THIRD-PARTY-NOTICES.txt'), nativeThirdPartyNotices(destination));
  json(path.join(destination, 'paseo-runtime.json'), {
    schemaVersion: 1, runtime: 'rust', appId: 'sh.paseo.daemon', installationKind,
    version: build.identity.version, sourceSha: build.identity.sourceSha,
    platform, arch, release: build.identity.release,
  });
  json(path.join(destination, 'bundle-provenance.json'), {
    schemaVersion: 1, runtime: 'rust', ...build.identity,
    sourceExecutableSha256: build.identity.executableSha256,
    executableSha256: digest(path.join(destination, nativeExecutableName)),
    buildNodeVersion: process.version, installationKind,
  });
}
