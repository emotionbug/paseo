import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/features/git/open_in_button.dart';
import 'package:paseo_sessions/ui/command_center.dart' show formatCompactTimeAgo;

void main() {
  test('forge pages use the remote host, canonical cloud hosts and each forge grammar', () {
    // An SSH alias of the cloud forge opens on its web host.
    expect(
      forgeWebUrl('github', remoteUrl: 'git@ssh.github.com:me/repo.git', branch: 'feat/x'),
      'https://github.com/me/repo/tree/feat/x',
    );
    expect(forgeFromRemoteUrl('git@github.com:me/repo.git'), 'github');
    expect(forgeFromRemoteUrl('https://git.example.com/me/repo.git'), isNull);
    // A self-hosted http(s) origin keeps its port; an active file opens its blob.
    expect(
      forgeWebUrl('gitlab',
          remoteUrl: 'https://git.example.com:8443/group/sub/repo.git',
          branch: 'main',
          path: 'src/a b.dart',
          lineStart: 3,
          lineEnd: 5),
      'https://git.example.com:8443/group/sub/repo/-/blob/main/src/a%20b.dart#L3-5',
    );
    expect(
      forgeWebUrl('codeberg', remoteUrl: 'https://codeberg.org/me/repo', branch: 'main', path: './docs/../README.md'),
      'https://codeberg.org/me/repo/src/branch/main/README.md',
    );
    // A detached HEAD, a path escaping the checkout or an unknown forge has no page.
    expect(forgeWebUrl('github', remoteUrl: 'git@github.com:me/repo.git', branch: 'HEAD'), isNull);
    expect(forgeWebUrl('github', remoteUrl: 'git@github.com:me/repo.git', branch: 'main', path: '../x'), isNull);
    expect(forgeWebUrl('bitbucket', remoteUrl: 'git@bitbucket.org:me/repo.git', branch: 'main'), isNull);
  });

  test('compact activity times never go sub-minute and become dates after a week', () {
    final now = DateTime(2026, 10, 31, 12);
    expect(formatCompactTimeAgo(now.subtract(const Duration(seconds: 59)), now), 'now');
    expect(formatCompactTimeAgo(now.subtract(const Duration(minutes: 59)), now), '59m');
    expect(formatCompactTimeAgo(now.subtract(const Duration(hours: 23, minutes: 59)), now), '23h');
    expect(formatCompactTimeAgo(now.subtract(const Duration(days: 6, hours: 23)), now), '6d');
    expect(formatCompactTimeAgo(DateTime(2026, 10, 15, 9), now), 'Oct 15');
  });
}
