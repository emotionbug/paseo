import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export function restampManifest(manifest, hours) {
  if (!Number.isFinite(hours) || hours < 0) throw new Error('rollout_hours must be a nonnegative number.');
  if (manifest.schemaVersion !== 1 || manifest.runtime !== 'flutter' || manifest.appId !== 'sh.paseo.sessions.desktop' ||
      !['stable', 'beta'].includes(manifest.channel) || !['linux', 'macos', 'windows'].includes(manifest.platform) ||
      !['x64', 'arm64'].includes(manifest.arch) || typeof manifest.version !== 'string' || !manifest.version ||
      typeof manifest.releaseDate !== 'string' || !Number.isFinite(Date.parse(manifest.releaseDate))) {
    throw new Error('Invalid native desktop rollout manifest.');
  }
  return { ...manifest, rolloutHours: hours };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [hours, ...files] = process.argv.slice(2);
  if (!hours?.trim() || !files.length) throw new Error('Usage: node scripts/stamp-flutter-rollout.mjs <hours> <manifest...>');
  // Validate every manifest before changing any of them. Keep the original date,
  // source, version and asset hashes so an ongoing ramp does not restart.
  const updates = files.map(file => [file, restampManifest(JSON.parse(fs.readFileSync(file, 'utf8')), Number(hours))]);
  for (const [file, manifest] of updates) fs.writeFileSync(file, `${JSON.stringify(manifest, null, 2)}\n`);
}
