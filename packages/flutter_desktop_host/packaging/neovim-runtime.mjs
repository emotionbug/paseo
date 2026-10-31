import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createHash } from 'node:crypto';
import { run, json } from './common.mjs';
import { assertNativeArchitecture } from './native-build.mjs';

// Neovim v0.12.6 official release assets, including all runtime files/licenses.
const assets = {
  'linux-x64': ['nvim-linux-x86_64.tar.gz', '474430d53e6264f6d6dd18db42d6dc9df3a1b56ca9e88a325bbf860e1a811d87'],
  'linux-arm64': ['nvim-linux-arm64.tar.gz', '8f1f64a0bdb97247034038c3823c6cbad5bdf9ecd5751b85494b71c3ee04c815'],
  'darwin-x64': ['nvim-macos-x86_64.tar.gz', '481424f1dbc85f637d57e47d27d042aa9c883e5a4fe9ee64fc4ee9573b221998'],
  'darwin-arm64': ['nvim-macos-arm64.tar.gz', '1dbd148222b051ba6c307c4d17a318ed083df018a4f6a5a0ce46955ffc9ab87b'],
  'win32-x64': ['nvim-win64.zip', '4e12b7103b601ffc8f25954b4da100f339faa73dccb69a5b135e897fa3d893ed'],
  'win32-arm64': ['nvim-win-arm64.zip', '2e58d259ffcc6145e5112ea02330126c6b57da09087bb9265fb2fa9aa10b485c'],
};
export async function copyNeovimRuntime(runtime) {
  const asset = assets[`${process.platform}-${process.arch}`];
  if (!asset) throw new Error('No pinned Neovim distribution for this target.');
  const [name, sha256] = asset;
  const cache = path.join(process.env.XDG_CACHE_HOME || path.join(os.homedir(), '.cache'), 'paseo-flutter-packaging', 'neovim-0.12.6');
  fs.mkdirSync(cache, { recursive: true });
  const archive = path.join(cache, name);
  let data = fs.existsSync(archive) ? fs.readFileSync(archive) : null;
  if (!data || createHash('sha256').update(data).digest('hex') !== sha256) {
    const response = await fetch(`https://github.com/neovim/neovim/releases/download/v0.12.6/${name}`);
    if (!response.ok) throw new Error(`Neovim download failed: HTTP ${response.status}`);
    data = Buffer.from(await response.arrayBuffer());
    if (createHash('sha256').update(data).digest('hex') !== sha256) throw new Error(`Neovim checksum mismatch: ${name}`);
    fs.writeFileSync(archive, data);
  }
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'paseo-neovim-'));
  try {
    run('tar', ['-xf', archive, '-C', temporary]);
    const directory = name.replace(/\.(tar\.gz|zip)$/, '');
    const destination = path.join(runtime, 'neovim');
    fs.cpSync(path.join(temporary, directory), destination, { recursive: true, dereference: true });
    const executable = path.join(destination, 'bin', process.platform === 'win32' ? 'nvim.exe' : 'nvim');
    if (!fs.existsSync(executable) || !fs.existsSync(path.join(destination, 'share/nvim/runtime/doc/uganda.txt'))) throw new Error('Incomplete official Neovim runtime/license distribution.');
    assertNativeArchitecture(executable);
    json(path.join(destination, 'paseo-provenance.json'), { version: '0.12.6', asset: name, sha256 });
  } finally {
    fs.rmSync(temporary, { recursive: true, force: true });
  }
}
