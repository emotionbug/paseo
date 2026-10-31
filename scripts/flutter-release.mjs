import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { getReleaseInfoFromSourceTag } from './release-version-utils.mjs';

export const targets = [
  { runner: 'ubuntu-24.04', platform: 'linux', arch: 'x64', desktop: true },
  { runner: 'ubuntu-24.04-arm', platform: 'linux', arch: 'arm64', desktop: true },
  { runner: 'macos-14', platform: 'macos', arch: 'arm64', desktop: true },
  { runner: 'macos-15-intel', platform: 'macos', arch: 'x64', desktop: true },
  { runner: 'windows-latest', platform: 'windows', arch: 'x64', desktop: true },
  { runner: 'windows-11-arm', platform: 'windows', arch: 'arm64', desktop: true },
  { runner: 'ubuntu-24.04', platform: 'android', arch: 'universal', desktop: false },
  { runner: 'macos-14', platform: 'ios', arch: 'arm64', desktop: false },
];
export const targetId = target => `${target.platform}-${target.arch}`;

export function planRelease({ sourceTag, platform = 'all', mode = 'build', publish = true, recoveryRunId = '', event = 'workflow_dispatch' }) {
  const info = getReleaseInfoFromSourceTag(sourceTag.replace(/^app-/, ''));
  const channel = info.isBeta ? 'beta' : 'stable';
  if (!['build', 'publish-only'].includes(mode)) throw new Error('Unknown release mode.');
  if (!['all', 'desktop', 'linux', 'macos', 'windows', 'android', 'ios', 'macos-arm64', 'macos-x64'].includes(platform)) throw new Error('Unknown native platform selection.');
  if (mode === 'publish-only' && (platform !== 'all' || !publish)) throw new Error('publish-only requires platform=all and publish=true.');
  if (event === 'push' || mode === 'publish-only') {
    platform = /^desktop-(macos|linux|windows)-v/.exec(sourceTag)?.[1] ??
      (sourceTag.startsWith('desktop-') ? 'desktop' : sourceTag.startsWith('android-') ? 'android' : 'all');
  }
  const selected = targets.filter(target => platform === 'all' ||
    (platform === 'desktop' && target.desktop) || target.platform === platform || targetId(target) === platform);
  const complementary = platform === 'macos-arm64' ? 'macos-x64' : platform === 'macos-x64' ? 'macos-arm64' : null;
  let recovery = [];
  if (mode === 'publish-only') {
    recovery = selected.map(targetId);
  } else if (complementary && publish) recovery = [complementary];
  if (recovery.length && !/^[1-9][0-9]*$/.test(recoveryRunId)) throw new Error('An exact prior recovery_run_id is required.');
  if (!recovery.length && recoveryRunId) throw new Error('recovery_run_id is only accepted for publish-only or a publishing single-architecture macOS rebuild.');
  return { sourceTag, releaseTag: info.releaseTag, version: info.version, channel,
    prerelease: info.isPrerelease, publish: publish && !info.isSmokeTag, mode,
    matrix: { include: selected }, recovery,
    expected: mode === 'publish-only' ? recovery : [...selected.map(targetId), ...recovery],
    createRelease: platform === 'all' || platform === 'desktop' || platform === 'android',
  };
}

function digest(file, algorithm = 'sha256', encoding = 'hex') {
  return new Promise((resolve, reject) => {
    const hash = createHash(algorithm);
    const stream = fs.createReadStream(file);
    stream.on('error', reject);
    stream.on('data', chunk => hash.update(chunk));
    stream.on('end', () => resolve(hash.digest(encoding)));
  });
}

export function daemonAssets(identity) {
  const format = identity.target.startsWith('windows-') ? 'zip' : 'tar.gz';
  return {
    archive: `Paseo-Daemon-${identity.version}-${identity.target}.${format}`,
    manifest: `daemon-${identity.channel}-${identity.target}.json`,
    format,
  };
}

export function validateReleaseManifest(manifest, identity, runtime) {
  const target = targets.find(target => target.desktop && targetId(target) === identity.target);
  const appId = runtime === 'rust' ? 'sh.paseo.daemon' : 'sh.paseo.sessions.desktop';
  if (!target || !['rust', 'flutter'].includes(runtime) || manifest.schemaVersion !== 1 ||
      manifest.runtime !== runtime || manifest.appId !== appId || manifest.version !== identity.version ||
      manifest.channel !== identity.channel || manifest.platform !== target.platform ||
      manifest.arch !== target.arch || manifest.sourceSha !== identity.sha) {
    throw new Error(`Release manifest identity mismatch: ${runtime} ${identity.target}`);
  }
  const entries = runtime === 'rust' ? [manifest.artifact] : manifest.artifacts || [manifest.artifact];
  const required = runtime === 'rust' ? [daemonAssets(identity).archive] :
    (target.platform === 'linux' ? ['AppImage', 'deb'] : target.platform === 'macos' ? ['zip'] : ['exe'])
      .map(extension => `Paseo-Sessions-Flutter-${identity.version}-${identity.target}.${extension}`);
  if (!Array.isArray(entries) || entries.length !== required.length ||
      required.some(name => entries.filter(entry => entry?.name === name).length !== 1)) {
    throw new Error(`Incomplete ${runtime} manifest artifact inventory: ${identity.target}`);
  }
  for (const entry of entries) {
    const format = runtime === 'rust' ? daemonAssets(identity).format : path.extname(entry.name).slice(1).toLowerCase();
    if (entry.format !== format || !Number.isSafeInteger(entry.size) || entry.size <= 0 ||
        typeof entry.sha512 !== 'string' || !/^[A-Za-z0-9+/]{86}==$/.test(entry.sha512)) {
      throw new Error(`Invalid ${runtime} artifact integrity: ${entry.name}`);
    }
  }
  return entries;
}

async function validateDesktopManifests(directory, identity) {
  for (const [runtime, name] of [['flutter', `flutter-${identity.channel}-${identity.target}.json`], ['rust', daemonAssets(identity).manifest]]) {
    const manifest = JSON.parse(fs.readFileSync(path.join(directory, name), 'utf8'));
    for (const entry of validateReleaseManifest(manifest, identity, runtime)) {
      const file = path.join(directory, entry.name);
      if (fs.statSync(file).size !== entry.size || await digest(file, 'sha512', 'base64') !== entry.sha512) {
        throw new Error(`Manifest artifact integrity failure: ${entry.name}`);
      }
    }
  }
}

export async function recordProvenance(directory, identity) {
  const target = targets.find(target => targetId(target) === identity.target);
  if (!target) throw new Error(`Unknown target ${identity.target}`);
  const stem = `Paseo-Sessions-Flutter-${identity.version}-${identity.target}`;
  let names;
  if (target.desktop) {
    const metadataName = `packages-${identity.target}.json`;
    const metadata = JSON.parse(fs.readFileSync(path.join(directory, metadataName), 'utf8'));
    if (metadata.version !== identity.version || metadata.platform !== target.platform || metadata.arch !== target.arch) throw new Error('Mismatched package metadata.');
    const daemon = daemonAssets(identity);
    names = [...metadata.artifacts, metadataName, `flutter-${identity.channel}-${identity.target}.json`, daemon.archive, daemon.manifest];
    await validateDesktopManifests(directory, identity);
  } else {
    names = fs.readdirSync(directory).filter(name => name.startsWith(`${stem}.`) || name.startsWith(`${stem}-`));
    if (names.length !== 1) throw new Error(`Expected exactly one mobile artifact for ${identity.target}`);
  }
  const files = [];
  for (const name of names) {
    if (path.basename(name) !== name) throw new Error('Invalid artifact basename.');
    const file = path.join(directory, name);
    files.push({ name, size: fs.statSync(file).size, sha256: await digest(file) });
  }
  const record = { schemaVersion: 1, workflow: '.github/workflows/desktop-release.yml', ...identity, files };
  fs.writeFileSync(path.join(directory, `provenance-${identity.target}.json`), `${JSON.stringify(record, null, 2)}\n`);
}

export async function validateProvenance(directory, expected) {
  const record = JSON.parse(fs.readFileSync(path.join(directory, `provenance-${expected.target}.json`), 'utf8'));
  for (const [key, value] of Object.entries({ schemaVersion: 1, workflow: '.github/workflows/desktop-release.yml', ...expected })) {
    if (record[key] !== value) throw new Error(`Provenance mismatch for ${expected.target}: ${key}`);
  }
  if (!Array.isArray(record.files) || record.files.length === 0) throw new Error('Missing artifact hashes.');
  const names = new Set();
  for (const file of record.files) {
    if (typeof file.name !== 'string' || path.basename(file.name) !== file.name || names.has(file.name)) throw new Error('Invalid artifact inventory.');
    names.add(file.name);
    const filename = path.join(directory, file.name);
    if (fs.statSync(filename).size !== file.size || await digest(filename) !== file.sha256) throw new Error(`Artifact integrity failure: ${file.name}`);
  }
  const target = targets.find(target => targetId(target) === expected.target);
  const stem = `Paseo-Sessions-Flutter-${expected.version}-${expected.target}`;
  if (!target) throw new Error('Unknown target.');
  if (target.desktop) {
    const required = target.platform === 'linux' ? ['AppImage', 'deb'] : target.platform === 'macos' ? ['zip'] : ['exe'];
    for (const extension of required) if (!names.has(`${stem}.${extension}`)) throw new Error(`Missing ${extension} package for ${expected.target}`);
    const daemon = daemonAssets(expected);
    for (const name of [`packages-${expected.target}.json`, `flutter-${expected.channel}-${expected.target}.json`, daemon.archive, daemon.manifest]) if (!names.has(name)) throw new Error(`Missing ${name}`);
    await validateDesktopManifests(directory, expected);
  } else if (target.platform === 'android') {
    if (!names.has(`${stem}.apk`) && !names.has(`${stem}-debug-signed.apk`)) throw new Error('Missing Android APK.');
  } else if (!names.has(`${stem}.ipa`) && !names.has(`${stem}-unsigned.app.zip`)) throw new Error('Missing iOS artifact.');
  return record;
}

async function main() {
  const [command] = process.argv.slice(2);
  const env = process.env;
  if (command === 'plan') {
    const plan = planRelease({ sourceTag: env.SOURCE_TAG, platform: env.INPUT_PLATFORM || 'all', mode: env.RECOVERY_MODE || 'build', publish: env.SHOULD_PUBLISH === 'true', recoveryRunId: env.RECOVERY_RUN_ID || '', event: env.GITHUB_EVENT_NAME });
    const packageVersion = JSON.parse(fs.readFileSync('package.json', 'utf8')).version;
    if (plan.version !== packageVersion) throw new Error(`Tag version ${plan.version} differs from source package version ${packageVersion}`);
    const hours = Number(env.ROLLOUT_HOURS);
    if (!Number.isFinite(hours) || hours < 0) throw new Error('rollout_hours must be nonnegative.');
    for (const [key, value] of Object.entries(plan)) console.log(`${key}=${typeof value === 'object' ? JSON.stringify(value) : value}`);
    return;
  }
  const identity = { sha: env.SOURCE_SHA, sourceTag: env.SOURCE_TAG, version: env.RELEASE_VERSION,
    channel: env.PASEO_RELEASE_CHANNEL, runId: env.GITHUB_RUN_ID };
  if (!/^[a-f0-9]{40}$/.test(identity.sha || '') || !/^[1-9][0-9]*$/.test(identity.runId || '')) throw new Error('Missing exact source/run identity.');
  const directory = 'artifacts/flutter';
  if (command === 'record') await recordProvenance(directory, { ...identity, target: env.TARGET_ID });
  else if (command === 'validate') {
    const recovered = JSON.parse(env.RECOVERY_TARGETS || '[]');
    for (const target of JSON.parse(env.EXPECTED_TARGETS)) {
      await validateProvenance(directory, { ...identity, target, runId: recovered.includes(target) ? env.RECOVERY_RUN_ID : identity.runId });
    }
  } else throw new Error('Usage: node scripts/flutter-release.mjs plan|record|validate');
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main();
