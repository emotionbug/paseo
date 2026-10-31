import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createHash } from 'node:crypto';

// Versioned GitHub asset IDs and upstream SHA256 digests, never a moving
// 'continuous' release. Sources: AppImage/appimagetool 1.9.1 release 263382884
// and AppImage/type2-runtime 20251108 release 260789861.
const pinned = {
  x64: [
    ['appimagetool', 'AppImage/appimagetool', 324406736, 'ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0'],
    ['runtime', 'AppImage/type2-runtime', 326011592, '2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d'],
  ],
  arm64: [
    ['appimagetool', 'AppImage/appimagetool', 324406707, 'f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158'],
    ['runtime', 'AppImage/type2-runtime', 326011362, '00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444'],
  ],
};
if (process.platform !== 'linux' || !pinned[process.arch]) throw new Error('AppImage tools require Linux x64 or arm64.');
const directory = path.join(process.env.XDG_CACHE_HOME || path.join(os.homedir(), '.cache'), 'paseo-flutter-packaging', process.arch);
fs.mkdirSync(directory, { recursive: true });
for (const [name, repository, asset, sha256] of pinned[process.arch]) {
  const output = path.join(directory, `${name}-${asset}`);
  let data = fs.existsSync(output) ? fs.readFileSync(output) : null;
  if (!data || createHash('sha256').update(data).digest('hex') !== sha256) {
    const headers = { Accept: 'application/octet-stream', 'X-GitHub-Api-Version': '2022-11-28' };
    if (process.env.GH_TOKEN) headers.Authorization = `Bearer ${process.env.GH_TOKEN}`;
    const response = await fetch(`https://api.github.com/repos/${repository}/releases/assets/${asset}`, { headers });
    if (!response.ok) throw new Error(`Cannot download pinned ${name}: HTTP ${response.status}`);
    data = Buffer.from(await response.arrayBuffer());
    if (createHash('sha256').update(data).digest('hex') !== sha256) throw new Error(`Pinned ${name} checksum mismatch.`);
    fs.writeFileSync(output, data, { mode: 0o755 });
  }
  fs.chmodSync(output, 0o755);
  const key = name === 'runtime' ? 'APPIMAGE_RUNTIME_FILE' : 'APPIMAGETOOL';
  if (process.env.GITHUB_ENV) fs.appendFileSync(process.env.GITHUB_ENV, `${key}=${output}\n`);
  console.log(`${key}=${output}`);
}
