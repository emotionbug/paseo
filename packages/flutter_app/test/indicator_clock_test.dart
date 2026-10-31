import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/ui/indicator_clock.dart';

class _Probe extends StatefulWidget {
  const _Probe();
  @override
  State<_Probe> createState() => _ProbeState();
}

class _ProbeState extends State<_Probe> with IndicatorClockState<_Probe> {
  int ticks = 0;
  @override
  void onIndicatorTick() => ticks++;
  @override
  Widget build(BuildContext context) => const SizedBox();
}

void main() {
  testWidgets('indicators tick by window state: focused, behind, minimized', (tester) async {
    await tester.pumpWidget(const _Probe());
    final probe = tester.state<_ProbeState>(find.byType(_Probe));
    Future<int> ticksIn(AppLifecycleState state) async {
      tester.binding.handleAppLifecycleStateChanged(state);
      final before = probe.ticks;
      await tester.pump(const Duration(seconds: 1));
      return probe.ticks - before;
    }

    expect(await ticksIn(AppLifecycleState.resumed), 10);
    expect(await ticksIn(AppLifecycleState.inactive), 4);
    expect(await ticksIn(AppLifecycleState.hidden), 0);
    expect(await ticksIn(AppLifecycleState.inactive), 4);
    expect(await ticksIn(AppLifecycleState.resumed), 10);
    await tester.pumpWidget(const SizedBox());
  });
}
