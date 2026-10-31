import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { createHash } from 'node:crypto';
import { planRelease, recordProvenance, validateProvenance, targets, targetId, daemonAssets, validateReleaseManifest } from './flutter-release.mjs';
import { restampManifest } from './stamp-flutter-rollout.mjs';

const request = { sourceTag: 'v1.2.3', publish: true, event: 'workflow_dispatch' };

function writeDesktopFixture(directory, identity) {
  const target = targets.find(target => targetId(target) === identity.target);
  const entries = (target.platform === 'linux' ? ['AppImage', 'deb'] : target.platform === 'macos' ? ['zip'] : ['exe'])
    .map(extension => writeArtifact(`Paseo-Sessions-Flutter-${identity.version}-${identity.target}.${extension}`, extension.toLowerCase()));
  const daemon = daemonAssets(identity);
  const artifact = writeArtifact(daemon.archive, daemon.format);
  const common = { schemaVersion: 1, version: identity.version, channel: identity.channel,
    platform: target.platform, arch: target.arch, sourceSha: identity.sha };
  const flutter = { ...common, runtime: 'flutter', appId: 'sh.paseo.sessions.desktop', artifacts: entries };
  const native = { ...common, runtime: 'rust', appId: 'sh.paseo.daemon', artifact };
  fs.writeFileSync(path.join(directory, `packages-${identity.target}.json`), JSON.stringify({ version: identity.version,
    platform: target.platform, arch: target.arch, artifacts: entries.map(entry => entry.name) }));
  fs.writeFileSync(path.join(directory, `flutter-${identity.channel}-${identity.target}.json`), JSON.stringify(flutter));
  fs.writeFileSync(path.join(directory, daemon.manifest), JSON.stringify(native));
  return { flutter, native, entries, daemon };

  function writeArtifact(name, format) {
    const bytes = Buffer.from(`fixture bytes for ${name}`);
    fs.writeFileSync(path.join(directory, name), bytes);
    return { name, format, size: bytes.length, sha512: createHash('sha512').update(bytes).digest('base64') };
  }
}

test('normal native releases cover all eight architecture targets', () => {
  const plan = planRelease(request);
  assert.deepEqual(plan.expected, targets.map(targetId));
  assert.equal(plan.expected.length, 8);
  assert.equal(plan.channel, 'stable');
  assert.equal(plan.publish, true);
});
test('platform tag pushes retain their intended scope and canonical release tag', () => {
  for (const platform of ['linux', 'macos', 'windows']) {
    const plan = planRelease({ ...request, event: 'push', sourceTag: `desktop-${platform}-v1.2.3-beta.1` });
    assert.deepEqual(plan.expected.sort(), [`${platform}-arm64`, `${platform}-x64`]);
    assert.equal(plan.releaseTag, 'v1.2.3-beta.1');
    assert.equal(plan.channel, 'beta');
    assert.equal(plan.createRelease, false);
  }
  assert.equal(planRelease({ ...request, event: 'push', sourceTag: 'desktop-v1.2.3' }).expected.length, 6);
  assert.deepEqual(planRelease({ ...request, event: 'push', sourceTag: 'android-v1.2.3' }).expected, ['android-universal']);
  assert.equal(planRelease({ ...request, event: 'push', sourceTag: 'app-v1.2.3' }).expected.length, 8);
});
test('recovery requires explicit publication and an exact prior run', () => {
  assert.throws(() => planRelease({ ...request, mode: 'publish-only' }), /recovery_run_id/);
  assert.throws(() => planRelease({ ...request, mode: 'publish-only', publish: false, recoveryRunId: '123' }), /publish=true/);
  assert.throws(() => planRelease({ ...request, mode: 'publish-only', platform: 'macos', recoveryRunId: '123' }), /platform=all/);
  assert.throws(() => planRelease({ ...request, recoveryRunId: '123' }), /only accepted/);
  assert.deepEqual(planRelease({ ...request, mode: 'publish-only', recoveryRunId: '123' }).recovery, targets.map(targetId));
  assert.equal(planRelease({ ...request, sourceTag: 'desktop-v1.2.3', mode: 'publish-only', recoveryRunId: '123' }).recovery.length, 6);
  assert.deepEqual(planRelease({ ...request, sourceTag: 'android-v1.2.3', mode: 'publish-only', recoveryRunId: '123' }).recovery, ['android-universal']);
});
test('single macOS architecture repairs require exact complementary provenance only when publishing', () => {
  assert.throws(() => planRelease({ ...request, platform: 'macos-x64' }), /recovery_run_id/);
  const plan = planRelease({ ...request, platform: 'macos-x64', recoveryRunId: '123' });
  assert.deepEqual(plan.matrix.include.map(targetId), ['macos-x64']);
  assert.deepEqual(plan.recovery, ['macos-arm64']);
  assert.deepEqual(planRelease({ ...request, platform: 'macos-x64', publish: false }).recovery, []);
});
test('artifact recovery rejects changed bytes and mismatched source, channel, tag and run', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'paseo-native-provenance-'));
  const identity = { sha: 'a'.repeat(40), sourceTag: 'v1.2.3', version: '1.2.3', channel: 'stable', runId: '123', target: 'android-universal' };
  const filename = path.join(directory, 'Paseo-Sessions-Flutter-1.2.3-android-universal-debug-signed.apk');
  try {
    fs.writeFileSync(filename, 'test artifact bytes');
    await recordProvenance(directory, identity);
    await validateProvenance(directory, identity);
    for (const [key, value] of Object.entries({ sha: 'b'.repeat(40), sourceTag: 'android-v1.2.3', version: '1.2.4', channel: 'beta', runId: '124' })) {
      await assert.rejects(validateProvenance(directory, { ...identity, [key]: value }), /Provenance mismatch/);
    }
    fs.writeFileSync(filename, 'altered artifact');
    await assert.rejects(validateProvenance(directory, identity), /integrity failure/);
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});
test('desktop provenance requires the full platform installer set', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'paseo-native-provenance-'));
  const identity = { sha: 'a'.repeat(40), sourceTag: 'v1.2.3', version: '1.2.3', channel: 'stable', runId: '123', target: 'linux-x64' };
  try {
    const fixture = writeDesktopFixture(directory, identity);
    fixture.flutter.artifacts.pop();
    fs.writeFileSync(path.join(directory, 'flutter-stable-linux-x64.json'), JSON.stringify(fixture.flutter));
    await assert.rejects(recordProvenance(directory, identity), /Incomplete flutter manifest/);
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});
test('native rollout adjustments retain the original date and installer integrity', () => {
  const manifest = { schemaVersion: 1, runtime: 'flutter', appId: 'sh.paseo.sessions.desktop', channel: 'stable', platform: 'linux', arch: 'x64', version: '1.2.3', releaseDate: '2026-10-01T00:00:00.000Z', rolloutHours: 36, sourceSha: 'a'.repeat(40), artifact: { name: 'installer', sha512: 'digest' } };
  assert.deepEqual(restampManifest(manifest, 0), { ...manifest, rolloutHours: 0 });
  assert.equal(manifest.rolloutHours, 36);
  for (const hours of [-1, NaN, Infinity]) assert.throws(() => restampManifest(manifest, hours), /nonnegative/);
  assert.throws(() => restampManifest({ ...manifest, runtime: 'electron' }, 1), /Invalid native/);
});

test('all six desktop targets retain complete standalone archives and distinct feeds in recovery', async () => {
  for (const target of targets.filter(target => target.desktop)) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'paseo-native-provenance-'));
    const identity = { sha: 'a'.repeat(40), sourceTag: 'v1.2.3', version: '1.2.3', channel: 'stable', runId: '123', target: targetId(target) };
    try {
      const fixture = writeDesktopFixture(directory, identity);
      await recordProvenance(directory, identity);
      const record = await validateProvenance(directory, identity);
      assert.ok(record.files.some(file => file.name === fixture.daemon.archive));
      assert.ok(record.files.some(file => file.name === fixture.daemon.manifest));
      assert.equal(fixture.daemon.format, target.platform === 'windows' ? 'zip' : 'tar.gz');
      assert.throws(() => validateReleaseManifest(fixture.flutter, identity, 'rust'), /identity mismatch/);
      assert.throws(() => validateReleaseManifest(fixture.native, identity, 'flutter'), /identity mismatch/);
      assert.throws(() => validateReleaseManifest({ ...fixture.native, sourceSha: 'b'.repeat(40) }, identity, 'rust'), /identity mismatch/);
      assert.throws(() => validateReleaseManifest({ ...fixture.native, artifact: { ...fixture.native.artifact, sha512: 'invalid' } }, identity, 'rust'), /artifact integrity/);
      fs.writeFileSync(path.join(directory, fixture.daemon.archive), 'changed archive');
      await assert.rejects(validateProvenance(directory, identity), /integrity failure/);
    } finally {
      fs.rmSync(directory, { recursive: true, force: true });
    }
  }
});

test('desktop recovery refuses old Flutter-only provenance and missing standalone contents', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'paseo-native-provenance-'));
  const identity = { sha: 'a'.repeat(40), sourceTag: 'v1.2.3', version: '1.2.3', channel: 'stable', runId: '123', target: 'linux-x64' };
  try {
    const fixture = writeDesktopFixture(directory, identity);
    await recordProvenance(directory, identity);
    const provenance = path.join(directory, `provenance-${identity.target}.json`);
    const record = JSON.parse(fs.readFileSync(provenance, 'utf8'));
    record.files = record.files.filter(file => file.name !== fixture.daemon.archive);
    fs.writeFileSync(provenance, JSON.stringify(record));
    await assert.rejects(validateProvenance(directory, identity), /Missing Paseo-Daemon/);
    fs.rmSync(path.join(directory, fixture.daemon.archive));
    await assert.rejects(recordProvenance(directory, identity), /ENOENT/);
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

