import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/features/chat/permission_card.dart';
import 'package:paseo_sessions/ui/theme.dart';

Future<List<JsonMap>> _pump(
  WidgetTester tester,
  JsonMap request, {
  Future<void> Function(JsonMap response)? onRespond,
}) async {
  tester.view.physicalSize = const Size(412, 915);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final responses = <JsonMap>[];
  await tester.pumpWidget(
    MaterialApp(
      theme: paseoTheme(brightness: Brightness.dark, preferences: {}),
      home: Scaffold(
        body: SingleChildScrollView(
          child: PermissionCard(
            request: request,
            onRespond: (response) async {
              responses.add(response);
              await onRespond?.call(response);
            },
          ),
        ),
      ),
    ),
  );
  return responses;
}

final _singleQuestion = {
  'questions': [
    {
      'question': 'Pick one',
      'header': 'Choice',
      'options': [
        {'label': 'A'},
        {'label': 'B'},
      ],
    },
  ],
};

void main() {
  testWidgets('tool permission shows detail and responds with the action id', (tester) async {
    final responses = await _pump(tester, {
      'id': 'p1',
      'kind': 'tool',
      'name': 'shell',
      'title': 'Run command',
      'description': 'Lists files',
      'detail': {'type': 'shell', 'command': 'ls -la', 'output': ''},
    });
    expect(tester.takeException(), isNull);
    expect(find.text('Run command'), findsOneWidget);
    expect(find.text('Lists files'), findsOneWidget);
    await tester.tap(find.text('agentStream.permission.accept'));
    // The pressed action keeps its spinner until the card goes away.
    await tester.pump();
    expect(responses, [
      {'behavior': 'allow', 'selectedActionId': 'accept'},
    ]);
  });

  testWidgets('permission without detail falls back to the raw input', (tester) async {
    final responses = await _pump(tester, {
      'id': 'p2',
      'kind': 'tool',
      'name': 'mcp__x',
      'input': {'path': '/tmp'},
    });
    expect(tester.takeException(), isNull);
    await tester.tap(find.text('agentStream.permission.deny'));
    await tester.pump();
    expect(responses.single, {
      'behavior': 'deny',
      'selectedActionId': 'reject',
      'message': 'Denied by user',
    });
  });

  testWidgets('plan permission renders a plan card with the footer', (tester) async {
    final responses = await _pump(tester, {
      'id': 'p3',
      'kind': 'plan',
      'input': {'plan': '# Plan\n\n1. Step'},
    });
    expect(tester.takeException(), isNull);
    expect(find.text('agentStream.permission.plan'), findsOneWidget);
    await tester.tap(find.text('agentStream.permission.implement'));
    await tester.pump();
    expect(responses.single['selectedActionId'], 'accept');
  });

  testWidgets('question form advances through tabs and submits answers', (tester) async {
    final input = {
      'questions': [
        {
          'question': 'Which color?',
          'header': 'Color',
          'options': [
            {'label': 'Red', 'description': 'Warm'},
            {'label': 'Blue', 'preview': 'blue preview'},
          ],
          'multiSelect': false,
        },
        {'question': 'Why?', 'header': 'Reason', 'options': [], 'multiSelect': false},
      ],
    };
    final responses = await _pump(tester, {'id': 'q1', 'kind': 'question', 'input': input});
    expect(tester.takeException(), isNull);
    expect(find.text('Which color?'), findsOneWidget);
    await tester.tap(find.text('Blue'));
    await tester.pumpAndSettle();
    // Single-select advances to the next question.
    expect(find.text('Why?'), findsOneWidget);
    await tester.enterText(find.byType(TextField), 'contrast');
    await tester.pumpAndSettle();
    await tester.tap(find.text('message.question.submit'));
    await tester.pumpAndSettle();
    expect(responses.single, {
      'behavior': 'allow',
      'updatedInput': {
        ...input,
        'answers': {'Color': 'Blue', 'Reason': 'contrast'},
      },
    });
  });

  testWidgets('an in-flight response disables every action', (tester) async {
    final pending = Completer<void>();
    final responses = await _pump(
      tester,
      {'id': 'p4', 'kind': 'tool', 'name': 'shell'},
      onRespond: (_) => pending.future,
    );
    await tester.tap(find.text('agentStream.permission.deny'));
    await tester.pump();
    // The pressed button shows a spinner; the other one ignores taps.
    expect(find.text('agentStream.permission.deny'), findsNothing);
    await tester.tap(find.text('agentStream.permission.accept'));
    await tester.pump();
    expect(responses, hasLength(1));
    pending.complete();
    await tester.pump();
  });

  testWidgets('a failed permission response re-enables the actions', (tester) async {
    final responses = await _pump(
      tester,
      {'id': 'p5', 'kind': 'tool', 'name': 'shell'},
      onRespond: (_) async => throw TimeoutException('no resolution'),
    );
    await tester.tap(find.text('agentStream.permission.accept'));
    await tester.pump();
    await tester.pump();
    // Plain permissions only re-enable; the question form is the one with an alert.
    expect(find.text('message.question.responseFailed'), findsNothing);
    await tester.tap(find.text('agentStream.permission.deny'));
    await tester.pump();
    expect(responses.map((r) => r['selectedActionId']), ['accept', 'reject']);
  });

  testWidgets('an empty plan is not rendered as a proposed plan', (tester) async {
    await _pump(tester, {
      'id': 'p6',
      'kind': 'tool',
      'name': 'shell',
      'input': {'plan': ''},
    });
    expect(find.text('agentStream.permission.proposedPlan'), findsNothing);
  });

  testWidgets('a non-empty input plan is rendered as a proposed plan', (tester) async {
    await _pump(tester, {
      'id': 'p7',
      'kind': 'tool',
      'name': 'shell',
      'input': {'plan': 'Do it'},
    });
    expect(find.text('agentStream.permission.proposedPlan'), findsOneWidget);
  });

  testWidgets('a failed question response shows an error and allows retry', (tester) async {
    var fail = true;
    final responses = await _pump(
      tester,
      {'id': 'q2', 'kind': 'question', 'input': _singleQuestion},
      onRespond: (_) async {
        if (fail) throw TimeoutException('no resolution');
      },
    );
    await tester.tap(find.text('A'));
    await tester.pump();
    await tester.tap(find.text('message.question.submit'));
    await tester.pump();
    await tester.pump();
    expect(find.text('message.question.responseFailed'), findsOneWidget);
    fail = false;
    await tester.tap(find.text('message.question.submit'));
    await tester.pump();
    // A new attempt clears the previous failure.
    expect(find.text('message.question.responseFailed'), findsNothing);
    expect(responses, hasLength(2));
    expect(jsonMap(responses.last['updatedInput'])['answers'], {'Choice': 'A'});
  });

  testWidgets('dismiss denies with the first non-empty dismiss label', (tester) async {
    final responses = await _pump(tester, {
      'id': 'q3',
      'kind': 'question',
      'input': {
        'questions': [
          {
            'question': 'Pick one',
            'header': 'Choice',
            'options': [
              {'label': 'A'},
            ],
            'dismissLabel': '',
          },
          {
            'question': 'Second',
            'header': 'Two',
            'options': [
              {'label': 'C'},
            ],
            'dismissLabel': 'Skip',
          },
        ],
      },
    });
    await tester.tap(find.text('Skip'));
    await tester.pump();
    expect(responses.single, {'behavior': 'deny', 'message': 'Dismissed by user'});
  });

  testWidgets('dismissing optional free-text questions submits empty answers', (tester) async {
    final input = {
      'questions': [
        {'question': 'Comment?', 'header': 'Response', 'options': [], 'allowEmpty': true},
      ],
    };
    final responses = await _pump(tester, {'id': 'q4', 'kind': 'question', 'input': input});
    await tester.tap(find.text('common.actions.dismiss'));
    await tester.pump();
    expect(responses.single, {
      'behavior': 'allow',
      'updatedInput': {
        ...input,
        'answers': {'Response': ''},
      },
    });
  });

  testWidgets('multi-select keeps checked options and appends the other answer', (tester) async {
    final input = {
      'questions': [
        {
          'question': 'Which fruits?',
          'header': 'Fruits',
          'multiSelect': true,
          'allowOther': true,
          'options': [
            {'label': 'Apple'},
            {'label': 'Banana'},
            {'label': 'Cherry'},
          ],
        },
      ],
    };
    final responses = await _pump(tester, {'id': 'q5', 'kind': 'question', 'input': input});
    await tester.tap(find.text('Apple'));
    await tester.pump();
    await tester.tap(find.text('Cherry'));
    await tester.pump();
    await tester.enterText(find.byType(TextField), ' durian ');
    await tester.pump();
    await tester.tap(find.text('message.question.submit'));
    await tester.pump();
    expect(jsonMap(responses.single['updatedInput'])['answers'], {
      'Fruits': 'Apple, Cherry, durian',
    });
  });
}
