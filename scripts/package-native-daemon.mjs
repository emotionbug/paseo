import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { release, version, platform, arch, run, json, requireNode24 } from '../packages/flutter_desktop_host/packaging/common.mjs';
import { releaseIdentity, nativeExecutableName, digest } from '../packages/flutter_desktop_host/packaging/native-build.mjs';
import { bundleRuntime } from '../packages/flutter_desktop_host/packaging/runtime.mjs';
import { signMac } from '../packages/flutter_desktop_host/packaging/signing.mjs';
import { copyWindowsRuntime } from '../packages/flutter_desktop_host/packaging/windows-runtime.mjs';

requireNode24();
if (process.argv.length !== 2) throw new Error('Usage: node scripts/package-native-daemon.mjs (native release host required)');
const identity = releaseIdentity();
const stage = fs.mkdtempSync(path.join(os.tmpdir(), 'paseo-daemon-package-'));
const runtime = path.join(stage, 'runtime');
const format = platform === 'windows' ? 'zip' : 'tar.gz';
const name = `Paseo-Daemon-${version}-${platform}-${arch}.${format}`;
const archive = path.join(release, name);
fs.mkdirSync(release, { recursive: true });
try {
  await bundleRuntime(runtime, { installationKind: 'standalone-archive' });
  if (platform === 'macos') await signMac(runtime, stage, { application: false });
  if (platform === 'windows') {
    copyWindowsRuntime(runtime, { standalone: true });
    if (process.env.WIN_CSC_LINK) {
      const certificate = path.join(stage, 'windows.pfx');
      if (fs.existsSync(process.env.WIN_CSC_LINK)) fs.copyFileSync(process.env.WIN_CSC_LINK, certificate);
      else fs.writeFileSync(certificate, Buffer.from(process.env.WIN_CSC_LINK, 'base64'), { mode: 0o600 });
      run(process.env.SIGNTOOL || 'signtool.exe', ['sign', '/fd', 'SHA256', '/td', 'SHA256', '/tr', 'http://timestamp.digicert.com', '/f', certificate, '/p', process.env.WIN_CSC_KEY_PASSWORD || '', path.join(runtime, nativeExecutableName)]);
    }
  }
  // The native updater accepts only regular files/directories. No symlink,
  // special-file, or tar hard-link representation may enter the payload.
  function inventory(directory) {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const file = path.join(directory, entry.name);
      if (entry.isDirectory()) inventory(file);
      else if (!entry.isFile() || fs.statSync(file).nlink !== 1) throw new Error(`Unsupported archive member: ${file}`);
    }
  }
  inventory(runtime);
  fs.rmSync(archive, { force: true });
  if (platform === 'windows') run('tar', ['--format=zip', '-cf', archive, '-C', runtime, '.']);
  else run('tar', ['-czf', archive, '-C', runtime, '.'], { env: { ...process.env, COPYFILE_DISABLE: '1' } });
  const manifest = {
    schemaVersion: 1, runtime: 'rust', appId: 'sh.paseo.daemon',
    channel: identity.channel, platform, arch, version, sourceSha: identity.sourceSha,
    artifact: { name, format, size: fs.statSync(archive).size, sha512: digest(archive, 'sha512', 'base64') },
  };
  json(path.join(release, `daemon-${identity.channel}-${platform}-${arch}.json`), manifest);
  console.log(archive);
} finally {
  fs.rmSync(stage, { recursive: true, force: true });
}
