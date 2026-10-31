import 'package:flutter/material.dart';

/// Unknown keys are retained so a newer client never loses another feature's data.
class AppPreferences {
  AppPreferences([Map<String, dynamic>? initial])
    : values = Map<String, dynamic>.from(initial ?? const {});

  final Map<String, dynamic> values;
  dynamic operator [](String key) => values[key];
  void operator []=(String key, dynamic value) => values[key] = value;

  /// Original THEME_OPTIONS literals are stored verbatim: 'dark' and the dark
  /// variants (zinc/midnight/claude/ghostty/pureBlack, plus native dark-*) are
  /// dark; 'light'/light-* are light. 'auto' follows the system, as does
  /// 'plugin': without its contributed theme the original fell back to 'auto'.
  ThemeMode get themeMode {
    final value = values['theme'] ?? values['themeMode'];
    if (value is String) {
      if (value == 'dark' || value.startsWith('dark-') ||
          const {'zinc', 'midnight', 'claude', 'ghostty', 'pureBlack'}.contains(value)) {
        return ThemeMode.dark;
      }
      if (value == 'light' || value.startsWith('light-')) return ThemeMode.light;
    }
    return ThemeMode.system;
  }

  set themeMode(ThemeMode value) => values['theme'] = value.name;
  String get locale {
    final value = values['language'] ?? values['locale'];
    return value is String ? value : 'en';
  }

  set locale(String value) => values['language'] = value;
  double get fontScale {
    final value = values['fontScale'];
    return value is num && value.isFinite ? value.toDouble().clamp(0.5, 3) : 1;
  }

  set fontScale(double value) => values['fontScale'] = value.clamp(0.5, 3);
}
