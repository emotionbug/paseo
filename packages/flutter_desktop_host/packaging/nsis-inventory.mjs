import fs from 'node:fs';
import path from 'node:path';

const inventoryName = '.paseo-sessions-owned-files';
const quote = value => value.replaceAll('$', '$$').replaceAll('"', '$\\"');

export function writeNsisInventory(payload, directory) {
  const files = [];
  const directories = [];
  function visit(relative = '') {
    for (const entry of fs.readdirSync(path.join(payload, relative), { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      const member = path.join(relative, entry.name);
      if (entry.isSymbolicLink()) throw new Error(`Windows installer payload must not contain links: ${member}`);
      if (entry.isDirectory()) {
        directories.push(member.replaceAll(path.sep, '\\'));
        visit(member);
      } else if (entry.isFile()) files.push(member.replaceAll(path.sep, '\\'));
      else throw new Error(`Unsupported installer payload member: ${member}`);
    }
  }
  visit();
  files.push('Uninstall.exe', inventoryName);
  for (const member of files) {
    if (/[\r\n]/.test(member) || member.split('\\').includes('..')) throw new Error(`Invalid installer member: ${member}`);
  }
  const manifest = path.join(directory, 'installed-files.txt');
  fs.writeFileSync(manifest, files.join('\r\n') + '\r\n');
  const include = path.join(directory, 'installed-files.nsh');
  const source = ['!macro PaseoCheckOwnedFiles'];
  for (const member of files) {
    const escaped = quote(member);
    source.push(`  IfFileExists "$INSTDIR\\${escaped}" 0 +3`, `    Push "${escaped}"`, '    Call PaseoCheckOwnedFile');
  }
  source.push('!macroend', '', '!macro PaseoRemoveOwnedFiles');
  for (const member of files) source.push(`  Delete "$INSTDIR\\${quote(member)}"`);
  // Never recurse: anything the package did not install remains untouched,
  // including unrelated files added beneath a package directory after install.
  directories.sort((a, b) => b.split('\\').length - a.split('\\').length || b.localeCompare(a));
  for (const member of directories) source.push(`  RMDir "$INSTDIR\\${quote(member)}"`);
  source.push('  RMDir "$INSTDIR"', '!macroend', '');
  fs.writeFileSync(include, source.join('\n'));
  return { manifest, include };
}
