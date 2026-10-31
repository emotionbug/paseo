import fs from 'node:fs';
import path from 'node:path';
import { capture } from './common.mjs';

export function copyWindowsRuntime(destination, { standalone = false } = {}) {
  let redist = process.env.VCToolsRedistDir;
  if (!redist) {
    const vswhere = path.join(process.env['ProgramFiles(x86)'] || 'C:/Program Files (x86)', 'Microsoft Visual Studio/Installer/vswhere.exe');
    const installation = capture(vswhere, ['-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath']);
    if (!installation) throw new Error('Visual Studio C++ tools not found; set VCToolsRedistDir.');
    const directory = path.join(installation, 'VC/Redist/MSVC');
    const versions = fs.readdirSync(directory).filter(name => /^\d+\./.test(name)).sort((a, b) => b.localeCompare(a, undefined, { numeric: true }));
    if (!versions.length) throw new Error('Visual C++ redistributable files are unavailable.');
    redist = path.join(directory, versions[0]);
  }
  const architecture = path.join(redist, process.arch);
  const crt = fs.readdirSync(architecture).find(name => /^Microsoft\.VC\d+\.CRT$/.test(name));
  if (!crt) throw new Error(`No ${process.arch} Visual C++ CRT in ${redist}`);
  for (const name of fs.readdirSync(path.join(architecture, crt))) {
    if (!name.toLowerCase().endsWith('.dll')) continue;
    const directories = standalone ? [destination, path.join(destination, 'lib')] : [destination, path.join(destination, 'runtime'), path.join(destination, 'runtime/lib'), path.join(destination, 'runtime/neovim/bin')];
    for (const directory of directories) {
      fs.copyFileSync(path.join(architecture, crt, name), path.join(directory, name));
    }
  }
}
