import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_localizations/flutter_localizations.dart';

import 'core/app_store.dart';
import 'core/client.dart';
import 'core/json.dart';
import 'core/scope.dart';
import 'features/audio/audio_service.dart';
import 'features/chat/chat_delivery.dart';
import 'features/chat/chat_drafts.dart';
import 'platform/desktop/desktop_host.dart';
import 'platform/mobile/mobile_services.dart';
import 'platform/mobile/mobile_services_host.dart';
import 'ui/client_activity.dart';
import 'ui/common.dart';
import 'ui/localization.dart';
import 'ui/shell.dart';
import 'ui/theme.dart';

class PaseoBootstrap extends StatefulWidget {
  const PaseoBootstrap({
    super.key,
    required this.store,
    required this.arguments,
    required this.coordinator,
  });
  final AppStore store;
  final List<String> arguments;
  final bool coordinator;
  @override
  State<PaseoBootstrap> createState() => _PaseoBootstrapState();
}

class _PaseoBootstrapState extends State<PaseoBootstrap> {
  final _messenger = GlobalKey<ScaffoldMessengerState>();
  final _delivery = <PaseoClient, VoidCallback>{};
  StreamSubscription<Object>? _desktopErrors;
  VoidCallback? _removeOpenHook;
  VoidCallback? _removeCloseHook;
  ClientActivity? _activity;
  bool _ready = false;
  Object? _startupError;

  @override
  void initState() {
    super.initState();
    unawaited(_initialize());
  }

  Future<void> _initialize() async {
    try {
      if (!widget.coordinator) {
        await ChatDrafts.instance.initialize();
        _removeOpenHook = widget.store.registerClientOpenHook((
          client,
          serverId,
        ) {
          _delivery[client] = ChatDelivery.attach(client, serverId);
        });
        _removeCloseHook = widget.store.registerClientCloseHook((client) async {
          _delivery.remove(client)?.call();
        });
        AudioService.initialize(widget.store);
      }
      await widget.store.initialize();
      if (DesktopHost.supported) {
        final host = await DesktopHost.initialize(
          widget.store,
          arguments: widget.arguments,
        );
        if (!widget.coordinator) {
          _desktopErrors = host.errors.listen((error) {
            if (!mounted) return;
            setState(() => _startupError = error);
          });
        }
      } else {
        await MobileServices.initialize(widget.store);
      }
    } catch (error) {
      _startupError = error;
    } finally {
      if (!widget.coordinator && mounted) {
        _activity = ClientActivity(
          widget.store,
          onError: (error) {
            if (mounted) setState(() => _startupError = error);
          },
        );
      }
      if (mounted) setState(() => _ready = true);
    }
  }

  @override
  void dispose() {
    _activity?.dispose();
    _removeOpenHook?.call();
    _removeCloseHook?.call();
    for (final release in _delivery.values) {
      release();
    }
    _delivery.clear();
    unawaited(_desktopErrors?.cancel());
    if (!widget.coordinator) {
      unawaited(AudioService.shutdown());
      if (!DesktopHost.supported) {
        unawaited(MobileServices.instance.disposeAsync());
      }
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (widget.coordinator) return const SizedBox.shrink();
    return PaseoApp(
      store: widget.store,
      activity: _activity,
      messengerKey: _messenger,
      home: !_ready
          ? const Scaffold(body: LoadingState(label: 'Connecting to Paseo…'))
          : MobileServicesHost(
              child: PaseoShell(
                startupError: _startupError,
                activity: _activity,
              ),
            ),
    );
  }
}

/// The same native widget tree is used by the app and the real-daemon smoke run.
class PaseoApp extends StatefulWidget {
  const PaseoApp({
    super.key,
    required this.store,
    this.home,
    this.messengerKey,
    this.activity,
  });
  final AppStore store;
  final Widget? home;
  final GlobalKey<ScaffoldMessengerState>? messengerKey;
  final ClientActivity? activity;
  @override
  State<PaseoApp> createState() => _PaseoAppState();
}

class _PaseoAppState extends State<PaseoApp> {
  late ThemeData _light;
  late ThemeData _dark;
  ThemeMode _mode = ThemeMode.system;
  Locale? _locale;
  String _themeKey = '';
  String _language = '';

  static const _themeFields = [
    'theme',
    'uiFontFamily',
    'monoFontFamily',
    'uiBaseFontSize',
    'contentFontSize',
    'codeFontSize',
    'contentMaxWidth',
  ];

  @override
  void initState() {
    super.initState();
    _appearance(force: true);
    widget.store.addListener(_appearanceChanged);
  }

  @override
  void didUpdateWidget(PaseoApp oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.store != widget.store) {
      oldWidget.store.removeListener(_appearanceChanged);
      widget.store.addListener(_appearanceChanged);
      _appearance(force: true);
    }
  }

  void _appearanceChanged() {
    if (_appearance()) setState(() {});
  }

  bool _appearance({bool force = false}) {
    final preferences = widget.store.preferences;
    final values = preferences.values;
    final themeKey = jsonEncode([for (final f in _themeFields) values[f]]);
    final language = values.text('language', 'system');
    final updateThemes = force || themeKey != _themeKey;
    if (!updateThemes && language == _language) return false;
    _themeKey = themeKey;
    _language = language;
    _mode = preferences.themeMode;
    final parts = language.split('-');
    _locale = language == 'system' || language == 'auto'
        ? null
        : Locale(parts.first, parts.length > 1 ? parts[1] : null);
    if (updateThemes) {
      _light = paseoTheme(brightness: Brightness.light, preferences: values);
      _dark = paseoTheme(brightness: Brightness.dark, preferences: values);
    }
    return true;
  }

  @override
  void dispose() {
    widget.store.removeListener(_appearanceChanged);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => PaseoScope(
    store: widget.store,
    child: MaterialApp(
      title: 'Paseo Sessions',
      debugShowCheckedModeBanner: false,
      scaffoldMessengerKey: widget.messengerKey,
      theme: _light,
      darkTheme: _dark,
      themeMode: _mode,
      locale: _locale,
      supportedLocales: supportedPaseoLocales,
      localizationsDelegates: const [
        PaseoLocalizations.delegate,
        GlobalMaterialLocalizations.delegate,
        GlobalWidgetsLocalizations.delegate,
        GlobalCupertinoLocalizations.delegate,
      ],
      builder: (context, child) => Listener(
        onPointerDown: (_) => widget.activity?.recordInput(),
        onPointerSignal: (_) => widget.activity?.recordInput(),
        child: child ?? const SizedBox.shrink(),
      ),
      home: widget.home ?? PaseoShell(activity: widget.activity),
    ),
  );
}
