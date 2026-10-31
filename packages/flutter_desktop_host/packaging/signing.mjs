import fs from 'node:fs';
import path from 'node:path';
import { randomBytes } from 'node:crypto';
import { root, run, capture } from './common.mjs';

const support = path.join(root, 'packages/flutter_desktop_host/packaging');
export async function signMac(bundle, stage, { application = true } = {}) {
  let keychain;
  let originalKeychains;
  let identity = process.env.CSC_NAME || '-';
  try {
    if (process.env.CSC_LINK) {
      originalKeychains = capture('security', ['list-keychains', '-d', 'user']).match(/"([^"]+)"/g)?.map(item => item.slice(1, -1)) || [];
      keychain = path.join(stage, 'signing.keychain-db');
      const password = randomBytes(24).toString('hex');
      const certificate = path.join(stage, 'certificate.p12');
      const source = process.env.CSC_LINK;
      if (/^https:\/\//.test(source)) {
        const response = await fetch(source);
        if (!response.ok) throw new Error(`Certificate download failed: ${response.status}`);
        fs.writeFileSync(certificate, Buffer.from(await response.arrayBuffer()), { mode: 0o600 });
      } else if (fs.existsSync(source)) fs.copyFileSync(source, certificate);
      else fs.writeFileSync(certificate, Buffer.from(source, 'base64'), { mode: 0o600 });
      run('security', ['create-keychain', '-p', password, keychain]);
      run('security', ['set-keychain-settings', '-lut', '21600', keychain]);
      run('security', ['unlock-keychain', '-p', password, keychain]);
      run('security', ['import', certificate, '-k', keychain, '-P', process.env.CSC_KEY_PASSWORD || '', '-T', '/usr/bin/codesign']);
      run('security', ['set-key-partition-list', '-S', 'apple-tool:,apple:,codesign:', '-s', '-k', password, keychain]);
      run('security', ['list-keychains', '-d', 'user', '-s', keychain, ...originalKeychains]);
      const identities = capture('security', ['find-identity', '-v', '-p', 'codesigning', keychain]);
      const match = identities.match(/([A-F0-9]{40}) "Developer ID Application:[^"]+"/);
      if (!match) throw new Error('Certificate contains no Developer ID Application identity.');
      identity = match[1];
    }
    const entitlements = path.join(support, 'entitlements.plist');
    const options = ['--force', '--sign', identity, '--options', 'runtime', '--entitlements', entitlements];
    if (identity !== '-') options.push('--timestamp');
    const paths = [];
    function collect(directory) {
      for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
        const file = path.join(directory, entry.name);
        if (entry.isSymbolicLink()) continue;
        if (entry.isDirectory()) { collect(file); if (/\.(framework|app|xpc)$/.test(file)) paths.push(file); }
        else if (entry.isFile()) {
          const descriptor = fs.openSync(file, 'r');
          const magic = Buffer.alloc(4);
          const count = fs.readSync(descriptor, magic, 0, 4, 0);
          fs.closeSync(descriptor);
          if (count === 4 && ['feedface', 'feedfacf', 'cefaedfe', 'cffaedfe', 'cafebabe', 'bebafeca', 'cafebabf'].includes(magic.toString('hex'))) paths.push(file);
        }
      }
    }
    collect(bundle);
    for (const file of paths) run('codesign', [...options, file]);
    if (application) {
      run('codesign', [...options, bundle]);
      run('codesign', ['--verify', '--deep', '--strict', bundle]);
    } else {
      for (const file of paths) run('codesign', ['--verify', '--strict', file]);
    }
    const notaryValues = ['APPLE_ID', 'APPLE_APP_SPECIFIC_PASSWORD', 'APPLE_TEAM_ID'];
    const configured = notaryValues.filter(name => process.env[name]);
    if (configured.length && configured.length !== notaryValues.length) throw new Error('Notarization requires APPLE_ID, APPLE_APP_SPECIFIC_PASSWORD and APPLE_TEAM_ID together.');
    if (configured.length) {
      if (identity === '-') throw new Error('Notarization requires Developer ID signing, not ad-hoc signing.');
      const upload = path.join(stage, 'notary.zip');
      run('ditto', ['-c', '-k', '--sequesterRsrc', '--keepParent', bundle, upload]);
      run('xcrun', ['notarytool', 'submit', upload, '--apple-id', process.env.APPLE_ID, '--password', process.env.APPLE_APP_SPECIFIC_PASSWORD, '--team-id', process.env.APPLE_TEAM_ID, '--wait']);
      if (application) run('xcrun', ['stapler', 'staple', bundle]);
    }
  } finally {
    if (keychain) {
      if (originalKeychains) run('security', ['list-keychains', '-d', 'user', '-s', ...originalKeychains]);
      if (fs.existsSync(keychain)) run('security', ['delete-keychain', keychain]);
    }
  }
}
