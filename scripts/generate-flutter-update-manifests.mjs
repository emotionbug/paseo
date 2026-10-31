import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { release, version, appId, json } from '../packages/flutter_desktop_host/packaging/common.mjs';

const directory = path.resolve(process.argv[2] || release);
if (process.argv.length > 3) throw new Error('Usage: node scripts/generate-flutter-update-manifests.mjs [artifact-directory]');
const channel = process.env.PASEO_RELEASE_CHANNEL || (version.includes('-') ? 'beta' : 'stable');
if (!['stable', 'beta'].includes(channel)) throw new Error('PASEO_RELEASE_CHANNEL must be stable or beta.');
const rolloutHours = Number(process.env.ROLLOUT_HOURS || '36');
if (!Number.isFinite(rolloutHours) || rolloutHours < 0) throw new Error('ROLLOUT_HOURS must be a nonnegative number.');
const releaseDate = new Date(process.env.PASEO_RELEASE_DATE || (process.env.SOURCE_DATE_EPOCH ? Number(process.env.SOURCE_DATE_EPOCH) * 1000 : Date.now())).toISOString();
const releaseNotes = process.env.PASEO_RELEASE_NOTES_FILE ? fs.readFileSync(process.env.PASEO_RELEASE_NOTES_FILE, 'utf8') : process.env.PASEO_RELEASE_NOTES || '';
const metadataFiles = fs.readdirSync(directory).filter(file => /^packages-(linux|macos|windows)-(x64|arm64)\.json$/.test(file)).sort();
if (!metadataFiles.length) throw new Error('No Flutter package metadata found; run package-flutter-desktop.mjs first.');
for (const file of metadataFiles) {
  const metadata = JSON.parse(fs.readFileSync(path.join(directory, file), 'utf8'));
  const { platform, arch } = metadata;
  if (file !== `packages-${platform}-${arch}.json` || metadata.version !== version) throw new Error(`Mismatched package provenance: ${file}`);
  if (!Array.isArray(metadata.artifacts) || !metadata.artifacts.length) throw new Error(`No packaged artifacts in ${file}`);
  const artifacts = [];
  for (const name of metadata.artifacts) {
    if (typeof name !== 'string' || path.basename(name) !== name || !name.startsWith(`Paseo-Sessions-Flutter-${version}-${platform}-${arch}.`)) throw new Error(`Invalid Flutter asset name: ${name}`);
    const format = path.extname(name).slice(1).toLowerCase();
    if (!({ linux: ['appimage', 'deb'], macos: ['zip'], windows: ['exe'] })[platform]?.includes(format)) throw new Error(`Unsupported ${platform} package: ${name}`);
    const artifact = path.join(directory, name);
    const header = Buffer.alloc(12);
    const descriptor = fs.openSync(artifact, 'r');
    fs.readSync(descriptor, header, 0, header.length, 0);
    fs.closeSync(descriptor);
    const valid = format === 'appimage' ? header.subarray(0, 4).equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46])) && header[4] === 2 && header.subarray(8, 11).equals(Buffer.from([0x41, 0x49, 2])) :
      format === 'deb' ? header.subarray(0, 8).toString() === '!<arch>\n' :
      format === 'exe' ? header.subarray(0, 2).toString() === 'MZ' : header.subarray(0, 4).equals(Buffer.from([0x50, 0x4b, 3, 4]));
    if (!valid) throw new Error(`Artifact is not a real ${format} file: ${name}`);
    const hash = createHash('sha512');
    for await (const chunk of fs.createReadStream(artifact)) hash.update(chunk);
    artifacts.push({ name, format, size: fs.statSync(artifact).size, sha512: hash.digest('base64') });
  }
  const manifest = { schemaVersion: 1, runtime: 'flutter', appId, channel, platform, arch,
    version, releaseDate, rolloutHours, releaseNotes,
    ...(process.env.SOURCE_SHA ? { sourceSha: process.env.SOURCE_SHA } : {}),
    ...(artifacts.length === 1 ? { artifact: artifacts[0] } : { artifacts }) };
  const output = path.join(directory, `flutter-${channel}-${platform}-${arch}.json`);
  json(output, manifest);
  console.log(output);
}
