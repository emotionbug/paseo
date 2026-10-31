import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { signMac } from '../packages/flutter_desktop_host/packaging/signing.mjs';
import { root, release, version, platform, arch, appId, artifactStem, run, capture, requireNode24, json } from '../packages/flutter_desktop_host/packaging/common.mjs';
import { bundleLinuxLibraries } from '../packages/flutter_desktop_host/packaging/linux-libraries.mjs';
import { copyWindowsRuntime } from '../packages/flutter_desktop_host/packaging/windows-runtime.mjs';
import { writeNsisInventory } from '../packages/flutter_desktop_host/packaging/nsis-inventory.mjs';

requireNode24();
const requested = process.argv[2] || platform;
if (requested !== platform || process.argv.length > 3) throw new Error('Usage: node scripts/package-flutter-desktop.mjs [linux|macos|windows] (native host required)');
const metadata = JSON.parse(fs.readFileSync(path.join(release, `build-${platform}-${arch}.json`), 'utf8'));
if (metadata.version !== version || metadata.mode !== 'release' || metadata.platform !== platform || metadata.arch !== arch) throw new Error('Build a matching release with scripts/build-flutter.mjs first.');
const input = path.resolve(root, metadata.output);
const support = path.join(root, 'packages/flutter_desktop_host/packaging');
const stage = fs.mkdtempSync(path.join(os.tmpdir(), 'paseo-flutter-package-'));
const artifacts = [];
function copy(source, destination) { fs.mkdirSync(path.dirname(destination), { recursive: true }); fs.cpSync(source, destination, { recursive: true }); }
function text(file, contents, mode = 0o644) { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, contents, { mode }); }
function artifact(extension) { const output = path.join(release, `${artifactStem()}.${extension}`); artifacts.push(output); return output; }
const desktopEntry = command => `[Desktop Entry]\nType=Application\nName=Paseo Sessions\nComment=Native client for local AI coding agents\nExec=${command} %U\nIcon=sh.paseo.Paseo\nTerminal=false\nCategories=Development;\nMimeType=x-scheme-handler/paseo;\nStartupWMClass=sh.paseo.sessions.desktop\n`;
const icon = path.join(root, 'packages/flutter_desktop_host/packaging/assets/icon.png');

try {
  if (platform === 'linux') {
    const appdir = path.join(stage, 'Paseo-Sessions.AppDir');
    copy(input, path.join(appdir, 'usr/lib/paseo-sessions'));
    bundleLinuxLibraries(appdir);
    text(path.join(appdir, 'AppRun'), '#!/bin/sh\nset -eu\nHERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"\nexport PATH="$HERE/usr/bin:$PATH"\nexport LD_LIBRARY_PATH="$HERE/usr/lib/paseo-dependencies${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"\nif [ "${1-}" = "--paseo-cli" ]; then\n  shift\n  exec "$HERE/usr/lib/paseo-sessions/runtime/paseo" "$@"\nfi\nexec "$HERE/usr/lib/paseo-sessions/paseo_sessions" "$@"\n', 0o755);
    text(path.join(appdir, 'sh.paseo.Paseo.desktop'), desktopEntry('paseo_sessions'));
    copy(icon, path.join(appdir, 'sh.paseo.Paseo.png'));
    copy(icon, path.join(appdir, '.DirIcon'));
    const appimageTool = process.env.APPIMAGETOOL || 'appimagetool';
    if (!process.env.APPIMAGE_RUNTIME_FILE) throw new Error('Set APPIMAGE_RUNTIME_FILE to a pinned AppImage type-2 runtime for this architecture; the release workflow provisions it.');
    run(appimageTool, ['--runtime-file', path.resolve(process.env.APPIMAGE_RUNTIME_FILE), appdir, artifact('AppImage')], {
      env: { ...process.env, ARCH: arch === 'x64' ? 'x86_64' : 'aarch64', APPIMAGE_EXTRACT_AND_RUN: '1' },
    });
    const deb = path.join(stage, 'deb');
    copy(input, path.join(deb, 'opt/paseo-sessions'));
    text(path.join(deb, 'usr/bin/paseo-sessions'), '#!/bin/sh\nexec /opt/paseo-sessions/paseo_sessions "$@"\n', 0o755);
    text(path.join(deb, 'usr/share/applications/sh.paseo.Paseo.desktop'), desktopEntry('/usr/bin/paseo-sessions'));
    copy(icon, path.join(deb, 'usr/share/icons/hicolor/512x512/apps/sh.paseo.Paseo.png'));
    const libcVersion = capture('getconf', ['GNU_LIBC_VERSION']).split(/\s+/)[1];
    if (!/^\d+\.\d+$/.test(libcVersion || '')) throw new Error('Cannot determine the Linux packaging glibc baseline.');
    text(path.join(deb, 'DEBIAN/control'), `Package: paseo-sessions\nVersion: ${version}\nArchitecture: ${arch === 'x64' ? 'amd64' : 'arm64'}\nMaintainer: Paseo contributors <hello@moboudra.com>\nSection: devel\nPriority: optional\nDepends: libgtk-3-0, libglib2.0-0, libstdc++6, libc6 (>= ${libcVersion}), libsecret-1-0, libayatana-appindicator3-1, libasound2t64 | libasound2, libpulse0, ffmpeg, pulseaudio-utils, xdg-utils\nDescription: Paseo Sessions native Flutter client\n`);
    text(path.join(deb, 'DEBIAN/postinst'), '#!/bin/sh\nset -e\nif command -v update-desktop-database >/dev/null 2>&1; then update-desktop-database /usr/share/applications; fi\nif command -v gtk-update-icon-cache >/dev/null 2>&1; then gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true; fi\n', 0o755);
    run('dpkg-deb', ['--root-owner-group', '--build', deb, artifact('deb')]);
  } else if (platform === 'windows') {
    const payload = path.join(stage, 'windows');
    copy(input, payload);
    copyWindowsRuntime(payload);
    const nsis = process.env.MAKENSIS || [path.join(process.env['ProgramFiles(x86)'] || 'C:/Program Files (x86)', 'NSIS/makensis.exe'), path.join(process.env.ProgramFiles || 'C:/Program Files', 'NSIS/makensis.exe')].find(file => fs.existsSync(file)) || 'makensis.exe';
    let certificate;
    if (process.env.WIN_CSC_LINK) {
      certificate = path.join(stage, 'windows.pfx');
      if (fs.existsSync(process.env.WIN_CSC_LINK)) fs.copyFileSync(process.env.WIN_CSC_LINK, certificate);
      else fs.writeFileSync(certificate, Buffer.from(process.env.WIN_CSC_LINK, 'base64'));
    }
    const sign = file => {
      if (certificate) run(process.env.SIGNTOOL || 'signtool.exe', ['sign', '/fd', 'SHA256', '/td', 'SHA256', '/tr', 'http://timestamp.digicert.com', '/f', certificate, '/p', process.env.WIN_CSC_KEY_PASSWORD || '', file]);
    };
    sign(path.join(payload, 'paseo_sessions.exe'));
    sign(path.join(payload, 'runtime/paseo.exe'));
    const inventory = writeNsisInventory(payload, stage);
    const installer = artifact('exe');
    run(nsis, ['/V3', `/DSOURCE=${payload}`, `/DINVENTORY_INCLUDE=${inventory.include}`, `/DINVENTORY_MANIFEST=${inventory.manifest}`, `/DOUTPUT=${installer}`, `/DVERSION=${version}`, `/DNUMERIC_VERSION=${version.split('-')[0]}.0`, `/DICON=${path.join(root, 'packages/flutter_desktop_host/packaging/assets/icon.ico')}`, path.join(support, 'installer.nsi')]);
    // Existing Sessions Windows releases permit unsigned NSIS installers.
    sign(installer);
  } else if (platform === 'macos') {
    const bundle = path.join(stage, 'Paseo Sessions.app');
    copy(input, bundle);
    const plist = path.join(bundle, 'Contents/Info.plist');
    const identifier = capture('/usr/libexec/PlistBuddy', ['-c', 'Print :CFBundleIdentifier', plist]);
    if (identifier !== appId) throw new Error(`Unexpected macOS bundle ID: ${identifier}`);
    run('/usr/libexec/PlistBuddy', ['-c', `Set :CFBundleShortVersionString ${version}`, plist]);
    run('/usr/libexec/PlistBuddy', ['-c', 'Set :LSMinimumSystemVersion 13.0', plist]);
    await signMac(bundle, stage);
    run('ditto', ['-c', '-k', '--sequesterRsrc', '--keepParent', bundle, artifact('zip')]);
  } else throw new Error(`Unsupported desktop platform: ${platform}`);
  json(path.join(release, `packages-${platform}-${arch}.json`), { version, platform, arch, artifacts: artifacts.map(file => path.basename(file)) });
  console.log(artifacts.join('\n'));
} finally {
  fs.rmSync(stage, { recursive: true, force: true });
}
