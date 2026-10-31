import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { getGitHubRelease, getReleaseLookupTag } from './github-release.mjs';
import { uploadWithRetry } from './upload-release-assets.mjs';
import { targets, targetId, daemonAssets, validateReleaseManifest } from './flutter-release.mjs';

const env = process.env;
const repo = env.GITHUB_REPOSITORY;
const tag = env.RELEASE_TAG;
const directory = 'artifacts/flutter';
if (!repo || !tag || !env.SOURCE_SHA) throw new Error('Release repository, tag and exact source are required.');
const gh = args => execFileSync('gh', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] });
let release = getGitHubRelease(repo, tag);
if (!release) {
  if (env.CREATE_RELEASE !== 'true') throw new Error('A platform-only repair requires an existing release.');
  try {
    gh(['release', 'create', tag, '--repo', repo, '--target', env.SOURCE_SHA,
      '--title', `Paseo ${tag}`, '--notes', '', '--draft', ...(env.IS_PRERELEASE === 'true' ? ['--prerelease'] : [])]);
  } catch (error) {
    if (!getGitHubRelease(repo, tag)) throw error;
  }
  release = getGitHubRelease(repo, tag);
}
if (!release) throw new Error('Release could not be resolved.');
const lookup = getReleaseLookupTag(release);
const expected = JSON.parse(env.EXPECTED_TARGETS);
const names = new Set();
const manifests = new Set();
for (const target of expected) {
  const record = JSON.parse(fs.readFileSync(path.join(directory, `provenance-${target}.json`), 'utf8'));
  for (const file of record.files) {
    if (file.name.startsWith('Paseo-Sessions-Flutter-') || file.name.startsWith('Paseo-Daemon-')) names.add(file.name);
  }
  if (targets.find(candidate => targetId(candidate) === target)?.desktop) {
    manifests.add(`flutter-${env.PASEO_RELEASE_CHANNEL}-${target}.json`);
    manifests.add(daemonAssets({ version: env.RELEASE_VERSION, channel: env.PASEO_RELEASE_CHANNEL, target }).manifest);
  }
}
// Upload binary assets before manifests: a failed upload must never advertise a
// new installer that is not yet present on the release.
if (await uploadWithRetry({ release: lookup, repo, files: [...names].map(name => path.join(directory, name)) })) throw new Error('Native artifact upload failed.');
if (manifests.size && await uploadWithRetry({ release: lookup, repo, files: [...manifests].map(name => path.join(directory, name)) })) throw new Error('Native manifest upload failed.');

// Android/iOS-only builds attach artifacts but never publish a draft on behalf
// of the desktop pipeline. Desktop publication requires all twelve distinct feeds.
if (manifests.size) {
  const fresh = getGitHubRelease(repo, tag);
  if (!fresh) throw new Error('Release disappeared during publication.');
  for (const target of targets.filter(target => target.desktop)) {
    const identity = { version: env.RELEASE_VERSION, channel: env.PASEO_RELEASE_CHANNEL, sha: env.SOURCE_SHA, target: targetId(target) };
    for (const [runtime, name] of [['flutter', `flutter-${identity.channel}-${identity.target}.json`], ['rust', daemonAssets(identity).manifest]]) {
      const asset = fresh.assets.find(asset => asset.name === name);
      if (!asset) throw new Error(`Missing ${name}; release remains a draft if not already published.`);
      const manifest = JSON.parse(gh(['api', `repos/${repo}/releases/assets/${asset.id}`, '-H', 'Accept: application/octet-stream']));
      for (const entry of validateReleaseManifest(manifest, identity, runtime)) {
        if (!fresh.assets.some(asset => asset.name === entry.name && asset.size === entry.size)) throw new Error(`Missing installer referenced by ${name}`);
      }
    }
  }
  gh(['release', 'edit', lookup, '--tag', tag, '--draft=false', '--repo', repo,
    ...(env.IS_PRERELEASE === 'true' ? ['--prerelease'] : [])]);
}
