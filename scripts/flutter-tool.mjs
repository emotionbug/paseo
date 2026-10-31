import { app, flutter, run } from '../packages/flutter_desktop_host/packaging/common.mjs';

const [command, ...args] = process.argv.slice(2);
if (!['analyze', 'test'].includes(command)) {
  throw new Error('Usage: node scripts/flutter-tool.mjs analyze|test [Flutter arguments]');
}
const sdk = flutter();
run(sdk, ['pub', 'get', '--enforce-lockfile'], { cwd: app });
run(sdk, [command, ...args], { cwd: app });
