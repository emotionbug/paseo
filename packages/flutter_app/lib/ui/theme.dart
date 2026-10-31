import 'dart:io';
import 'dart:ui' show lerpDouble;

import 'package:flutter/material.dart';

import '../core/json.dart';

@immutable
class PaseoColors extends ThemeExtension<PaseoColors> {
  const PaseoColors({
    required this.surface0,
    required this.surface1,
    required this.surface2,
    required this.surface3,
    required this.surface4,
    required this.sidebar,
    required this.selected,
    required this.muted,
    required this.extraMuted,
    required this.accent,
    required this.accentBright,
    required this.added,
    required this.removed,
    required this.success,
    required this.danger,
    required this.warning,
    required this.running,
    required this.borderAccent,
  });
  final Color surface0, surface1, surface2, surface3, surface4;
  final Color sidebar, selected, muted, extraMuted, accent, accentBright;
  final Color added, removed, success, danger, warning, running;
  /// Original `borderAccent`: outline of composer, cards and outline buttons.
  final Color borderAccent;
  Color get workspace => surface1;

  @override
  PaseoColors copyWith({
    Color? surface0, Color? surface1, Color? surface2, Color? surface3,
    Color? surface4, Color? sidebar, Color? selected, Color? muted,
    Color? extraMuted, Color? accent, Color? accentBright, Color? added,
    Color? removed, Color? success, Color? danger, Color? warning, Color? running,
    Color? borderAccent,
  }) => PaseoColors(
    surface0: surface0 ?? this.surface0,
    surface1: surface1 ?? this.surface1,
    surface2: surface2 ?? this.surface2,
    surface3: surface3 ?? this.surface3,
    surface4: surface4 ?? this.surface4,
    sidebar: sidebar ?? this.sidebar,
    selected: selected ?? this.selected,
    muted: muted ?? this.muted,
    extraMuted: extraMuted ?? this.extraMuted,
    accent: accent ?? this.accent,
    accentBright: accentBright ?? this.accentBright,
    added: added ?? this.added,
    removed: removed ?? this.removed,
    success: success ?? this.success,
    danger: danger ?? this.danger,
    warning: warning ?? this.warning,
    running: running ?? this.running,
    borderAccent: borderAccent ?? this.borderAccent,
  );

  @override
  PaseoColors lerp(covariant PaseoColors? other, double t) => other == null
      ? this
      : PaseoColors(
          surface0: Color.lerp(surface0, other.surface0, t)!,
          surface1: Color.lerp(surface1, other.surface1, t)!,
          surface2: Color.lerp(surface2, other.surface2, t)!,
          surface3: Color.lerp(surface3, other.surface3, t)!,
          surface4: Color.lerp(surface4, other.surface4, t)!,
          sidebar: Color.lerp(sidebar, other.sidebar, t)!,
          selected: Color.lerp(selected, other.selected, t)!,
          muted: Color.lerp(muted, other.muted, t)!,
          extraMuted: Color.lerp(extraMuted, other.extraMuted, t)!,
          accent: Color.lerp(accent, other.accent, t)!,
          accentBright: Color.lerp(accentBright, other.accentBright, t)!,
          added: Color.lerp(added, other.added, t)!,
          removed: Color.lerp(removed, other.removed, t)!,
          success: Color.lerp(success, other.success, t)!,
          danger: Color.lerp(danger, other.danger, t)!,
          warning: Color.lerp(warning, other.warning, t)!,
          running: Color.lerp(running, other.running, t)!,
          borderAccent: Color.lerp(borderAccent, other.borderAccent, t)!,
        );

  static PaseoColors of(BuildContext context) =>
      Theme.of(context).extension<PaseoColors>()!;
}

/// Independent appearance axes: changing UI size must not resize content/code.
@immutable
class PaseoTypography extends ThemeExtension<PaseoTypography> {
  const PaseoTypography({
    required this.uiBaseFontSize,
    required this.contentFontSize,
    required this.codeFontSize,
    required this.contentMaxWidth,
    required this.monoFontFamily,
    required this.monoFontFamilyFallback,
  });
  final double uiBaseFontSize, contentFontSize, codeFontSize, contentMaxWidth;
  final String monoFontFamily;
  final List<String> monoFontFamilyFallback;

  double uiSize(double authoredSize) =>
      (authoredSize * uiBaseFontSize / 14).roundToDouble();

  TextStyle get codeStyle => TextStyle(
    fontFamily: monoFontFamily,
    fontFamilyFallback: monoFontFamilyFallback,
    fontSize: codeFontSize,
    height: 1.5,
    letterSpacing: 0,
  );

  @override
  PaseoTypography copyWith({
    double? uiBaseFontSize, double? contentFontSize, double? codeFontSize,
    double? contentMaxWidth, String? monoFontFamily,
    List<String>? monoFontFamilyFallback,
  }) => PaseoTypography(
    uiBaseFontSize: uiBaseFontSize ?? this.uiBaseFontSize,
    contentFontSize: contentFontSize ?? this.contentFontSize,
    codeFontSize: codeFontSize ?? this.codeFontSize,
    contentMaxWidth: contentMaxWidth ?? this.contentMaxWidth,
    monoFontFamily: monoFontFamily ?? this.monoFontFamily,
    monoFontFamilyFallback: monoFontFamilyFallback ?? this.monoFontFamilyFallback,
  );

  @override
  PaseoTypography lerp(covariant PaseoTypography? other, double t) => other == null
      ? this
      : PaseoTypography(
          uiBaseFontSize: lerpDouble(uiBaseFontSize, other.uiBaseFontSize, t)!,
          contentFontSize: lerpDouble(contentFontSize, other.contentFontSize, t)!,
          codeFontSize: lerpDouble(codeFontSize, other.codeFontSize, t)!,
          contentMaxWidth: lerpDouble(contentMaxWidth, other.contentMaxWidth, t)!,
          monoFontFamily: t < .5 ? monoFontFamily : other.monoFontFamily,
          monoFontFamilyFallback: t < .5
              ? monoFontFamilyFallback : other.monoFontFamilyFallback,
        );

  static PaseoTypography of(BuildContext context) =>
      Theme.of(context).extension<PaseoTypography>()!;
}

// Original built-in Paseo palettes; the primary action is monochrome, not accent.
class _Palette {
  const _Palette({
    required this.surfaces,
    required this.sidebar,
    required this.muted,
    required this.extraMuted,
    required this.border,
    required this.accent,
    required this.accentBright,
    required this.destructive,
  });
  final (int, int, int, int, int) surfaces;
  final int sidebar, muted, extraMuted, border, accent, accentBright, destructive;
}

const _darkPalettes = <String, _Palette>{
  'dark': _Palette(
    surfaces: (0xff181b1a, 0xff1e2120, 0xff272a29, 0xff434645, 0xff595b5b),
    sidebar: 0xff141716, muted: 0xffa1a5a4, extraMuted: 0xff717574,
    border: 0xff252b2a, accent: 0xff20744a, accentBright: 0xff7ccba0,
    destructive: 0xffc64f43,
  ),
  'zinc': _Palette(
    surfaces: (0xff18181b, 0xff1f1f22, 0xff27272a, 0xff3f3f46, 0xff52525b),
    sidebar: 0xff131316, muted: 0xffa1a1aa, extraMuted: 0xff71717a,
    border: 0xff27272a, accent: 0xffe4e4e7, accentBright: 0xfffafafa,
    destructive: 0xffc44a4a,
  ),
  'midnight': _Palette(
    surfaces: (0xff161820, 0xff1c1e27, 0xff252731, 0xff3c3e4c, 0xff535564),
    sidebar: 0xff121420, muted: 0xff9a9db0, extraMuted: 0xff6b6e82,
    border: 0xff242636, accent: 0xff3b6fcf, accentBright: 0xff7eaaeb,
    destructive: 0xffc44a52,
  ),
  'claude': _Palette(
    surfaces: (0xff1f1f1e, 0xff262523, 0xff2f2d2b, 0xff4a4745, 0xff605d5b),
    sidebar: 0xff1a1918, muted: 0xffada9a5, extraMuted: 0xff78746f,
    border: 0xff2c2a27, accent: 0xffd97757, accentBright: 0xffe89a7f,
    destructive: 0xffcf513e,
  ),
  'ghostty': _Palette(
    surfaces: (0xff282c34, 0xff2f333d, 0xff383c48, 0xff4a4f5e, 0xff5b6175),
    sidebar: 0xff21252d, muted: 0xffc8ccd8, extraMuted: 0xffa0a4b2,
    border: 0xff353a47, accent: 0xff89b4fa, accentBright: 0xffb4d0fc,
    destructive: 0xffc44a55,
  ),
  'pureBlack': _Palette(
    surfaces: (0xff000000, 0xff0a0a0a, 0xff111111, 0xff202020, 0xff2d2d2d),
    sidebar: 0xff000000, muted: 0xffa1a1aa, extraMuted: 0xff71717a,
    border: 0xff1c1c1c, accent: 0xff20744a, accentBright: 0xff7ccba0,
    destructive: 0xffc44a4a,
  ),
};
const _lightPalette = _Palette(
  surfaces: (0xffffffff, 0xfffafafa, 0xfff4f4f5, 0xffe4e4e7, 0xffd4d4d8),
  sidebar: 0xfff4f4f5, muted: 0xff71717a, extraMuted: 0xffa1a1aa,
  border: 0xffe4e4e7, accent: 0xff20744a, accentBright: 0xff239956,
  destructive: 0xffb04138,
);

const _borderAccents = <String, int>{
  'dark': 0xff2f3534,
  'zinc': 0xff303036,
  'midnight': 0xff2e3040,
  'claude': 0xff36332f,
  'ghostty': 0xff3f4454,
  'pureBlack': 0xff242424,
};

double _size(JsonMap preferences, String key, double fallback, double min, double max) {
  final value = preferences[key];
  return value is num && value.isFinite ? value.toDouble().clamp(min, max) : fallback;
}

List<String> _fontStack(String value) => value.split(',').map((name) {
  var family = name.trim();
  if (family.length > 1 &&
      ((family.startsWith('"') && family.endsWith('"')) ||
       (family.startsWith("'") && family.endsWith("'")))) {
    family = family.substring(1, family.length - 1);
  }
  return family;
}).where((family) => family.isNotEmpty &&
    !const {'normal', 'system-ui', '-apple-system', 'sans-serif'}.contains(family)).toList(growable: false);

ThemeData paseoTheme({
  required Brightness brightness,
  required JsonMap preferences,
}) {
  final dark = brightness == Brightness.dark;
  final mobile = Platform.isAndroid || Platform.isIOS;
  final name = switch (preferences.text('theme')) {
    'dark-zinc' => 'zinc',
    'dark-midnight' => 'midnight',
    'dark-claude' => 'claude',
    'dark-ghostty' => 'ghostty',
    'dark-pure-black' => 'pureBlack',
    final name => name,
  };
  final palette = dark ? _darkPalettes[name] ?? _darkPalettes['dark']! : _lightPalette;
  final foreground = dark ? const Color(0xfffafafa) : const Color(0xff1a1a1e);
  final primary = dark ? foreground : const Color(0xff18181b);
  final surface0 = Color(palette.surfaces.$1);
  final surface1 = Color(palette.surfaces.$2);
  final surface2 = Color(palette.surfaces.$3);
  final surface3 = Color(palette.surfaces.$4);
  final surface4 = Color(palette.surfaces.$5);
  final border = Color(palette.border);
  final borderAccent = Color(
    dark ? _borderAccents[name] ?? _borderAccents['dark']! : 0xffececf1,
  );
  final muted = Color(palette.muted);
  final accent = Color(palette.accent);
  final uiFonts = _fontStack(preferences.text('uiFontFamily'));
  final monoFonts = _fontStack(preferences.text('monoFontFamily'));
  final typography = PaseoTypography(
    uiBaseFontSize: _size(preferences, 'uiBaseFontSize', mobile ? 15 : 14, 10, 21),
    contentFontSize: _size(preferences, 'contentFontSize', mobile ? 16 : 15, 10, 21),
    codeFontSize: _size(preferences, 'codeFontSize', 12, 9, 22),
    contentMaxWidth: _size(preferences, 'contentMaxWidth', 820, 600, 4000),
    monoFontFamily: monoFonts.firstOrNull ??
        (Platform.isMacOS || Platform.isIOS ? 'Menlo' : Platform.isWindows ? 'Consolas' : 'monospace'),
    monoFontFamilyFallback: monoFonts.length > 1 ? monoFonts.sublist(1) : const [],
  );
  final colors = ColorScheme(
    brightness: brightness,
    primary: primary,
    onPrimary: dark ? surface0 : const Color(0xfffafafa),
    primaryContainer: surface2,
    onPrimaryContainer: foreground,
    secondary: surface2,
    onSecondary: foreground,
    secondaryContainer: surface3,
    onSecondaryContainer: foreground,
    tertiary: accent,
    onTertiary: name == 'zinc' && dark ? surface0 : Colors.white,
    error: Color(palette.destructive),
    onError: Colors.white,
    surface: surface0,
    onSurface: foreground,
    onSurfaceVariant: muted,
    surfaceContainerLowest: surface0,
    surfaceContainerLow: surface1,
    surfaceContainer: surface2,
    surfaceContainerHigh: surface3,
    surfaceContainerHighest: surface4,
    outline: border,
    outlineVariant: border,
    surfaceTint: Colors.transparent,
    inverseSurface: foreground,
    onInverseSurface: surface0,
  );
  final base = ThemeData(
    useMaterial3: true,
    // The original native app uses each font's natural line height. Material
    // typography geometry would merge ~1.43 heights into every text style;
    // keep only each script's baseline, which Material widgets require.
    typography: Typography.material2021(
      englishLike: _baselineGeometry(TextBaseline.alphabetic),
      dense: _baselineGeometry(TextBaseline.ideographic),
      tall: _baselineGeometry(TextBaseline.alphabetic),
    ),
    brightness: brightness,
    colorScheme: colors,
    fontFamily: uiFonts.firstOrNull,
    fontFamilyFallback: uiFonts.length > 1 ? uiFonts.sublist(1) : const [],
  );
  final text = TextStyle(
    fontFamily: uiFonts.firstOrNull,
    fontFamilyFallback: uiFonts.length > 1 ? uiFonts.sublist(1) : const [],
    color: foreground,
    letterSpacing: 0,
  );
  TextStyle ui(double size, [FontWeight weight = FontWeight.normal]) =>
      text.copyWith(fontSize: typography.uiSize(size), fontWeight: weight);
  // Original control-geometry: field buttons/inputs are 44px on compact
  // (button radius 16, field radius 8) and 32px on desktop (12 / 6).
  final buttonShape = RoundedRectangleBorder(
    borderRadius: BorderRadius.circular(mobile ? 16 : 12),
  );
  final fieldRadius = BorderRadius.circular(mobile ? 8 : 6);
  OutlineInputBorder fieldBorder(Color color) =>
      OutlineInputBorder(borderRadius: fieldRadius, borderSide: BorderSide(color: color));
  final buttonPadding = EdgeInsets.symmetric(horizontal: mobile ? 16 : 12);
  // Original pressed state is 0.85 opacity, not an overlay tint.
  const noOverlay = WidgetStatePropertyAll<Color?>(Colors.transparent);
  return base.copyWith(
    scaffoldBackgroundColor: dark ? surface1 : surface0,
    canvasColor: surface0,
    dividerColor: border,
    visualDensity: VisualDensity.standard,
    textTheme: TextTheme(
      displayLarge: ui(26), displayMedium: ui(22), displaySmall: ui(20),
      headlineLarge: ui(26, FontWeight.w600),
      headlineMedium: ui(22, FontWeight.w600),
      headlineSmall: ui(20, FontWeight.w600),
      titleLarge: ui(18, FontWeight.w600),
      titleMedium: ui(16, FontWeight.w600),
      titleSmall: ui(14, FontWeight.w600),
      bodyLarge: text.copyWith(fontSize: typography.contentFontSize),
      bodyMedium: ui(14), bodySmall: ui(12),
      labelLarge: ui(14, FontWeight.w500),
      labelMedium: ui(12, FontWeight.w500), labelSmall: ui(12),
    ),
    iconTheme: IconThemeData(color: muted, size: 20),
    splashFactory: NoSplash.splashFactory,
    appBarTheme: AppBarTheme(
      backgroundColor: dark ? surface1 : surface0,
      foregroundColor: foreground,
      elevation: 0, scrolledUnderElevation: 0,
      toolbarHeight: mobile ? 56 : 36,
      centerTitle: false,
      titleTextStyle: ui(16, FontWeight.w600),
    ),
    dividerTheme: DividerThemeData(color: border, thickness: 1, space: 1),
    inputDecorationTheme: InputDecorationTheme(
      filled: true, fillColor: surface2, hoverColor: Colors.transparent,
      isDense: true,
      hintStyle: text.copyWith(color: muted),
      contentPadding: EdgeInsets.symmetric(
        horizontal: mobile ? 16 : 12,
        vertical: ((mobile ? 44 : 32) - 20 - 2) / 2,
      ),
      // Original `FormTextInput`: errors never recolor the chrome; `Field`
      // shows them as one palette red-300 12px line under the control.
      errorStyle: text.copyWith(
        color: const Color(0xfffca5a5),
        fontSize: typography.uiSize(12),
        height: 1.4,
      ),
      errorMaxLines: 1,
      border: WidgetStateInputBorder.resolveWith((states) {
        if (states.contains(WidgetState.focused) ||
            states.contains(WidgetState.hovered)) {
          return fieldBorder(borderAccent);
        }
        return fieldBorder(Colors.transparent);
      }),
      floatingLabelStyle: text.copyWith(color: muted),
      labelStyle: text.copyWith(color: muted),
    ),
    filledButtonTheme: FilledButtonThemeData(style: FilledButton.styleFrom(
      backgroundColor: accent,
      foregroundColor: name == 'zinc' && dark ? surface0 : Colors.white,
      disabledBackgroundColor: accent.withValues(alpha: .5),
      disabledForegroundColor: (name == 'zinc' && dark ? surface0 : Colors.white)
          .withValues(alpha: .5),
      minimumSize: Size(0, mobile ? 44 : 32),
      padding: buttonPadding, elevation: 0,
      shape: buttonShape, textStyle: ui(14),
    ).copyWith(overlayColor: noOverlay)),
    // Original disabled buttons fade as a whole to 0.5 opacity.
    outlinedButtonTheme: OutlinedButtonThemeData(style: OutlinedButton.styleFrom(
      foregroundColor: foreground,
      disabledForegroundColor: foreground.withValues(alpha: .5),
      minimumSize: Size(0, mobile ? 44 : 32), padding: buttonPadding,
      shape: buttonShape, textStyle: ui(14),
    ).copyWith(
      overlayColor: noOverlay,
      side: WidgetStateProperty.resolveWith((states) => BorderSide(
        color: states.contains(WidgetState.disabled)
            ? borderAccent.withValues(alpha: .5)
            : borderAccent,
      )),
    )),
    // Original ghost button: muted text that turns foreground on hover.
    textButtonTheme: TextButtonThemeData(style: TextButton.styleFrom(
      shape: buttonShape, padding: buttonPadding,
      minimumSize: Size(0, mobile ? 44 : 32),
      textStyle: ui(14),
    ).copyWith(
      overlayColor: noOverlay,
      foregroundColor: WidgetStateProperty.resolveWith((states) =>
          states.contains(WidgetState.disabled)
              ? muted.withValues(alpha: .5)
              : states.contains(WidgetState.hovered) ||
                    states.contains(WidgetState.pressed)
              ? foreground
              : muted),
    )),
    // Original `icon-button-chrome` large frame: 32px on compact, 26px on
    // desktop, 6px radius, muted glyph, `interactionHighlight` fill while
    // hovered/pressed, 0.5 opacity when disabled.
    iconButtonTheme: IconButtonThemeData(style: IconButton.styleFrom(
      foregroundColor: muted,
      disabledForegroundColor: muted.withValues(alpha: .5),
      minimumSize: Size.square(mobile ? 32 : 26), iconSize: 16,
      padding: EdgeInsets.zero,
      tapTargetSize: MaterialTapTargetSize.shrinkWrap,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(6)),
    ).copyWith(
      overlayColor: WidgetStateProperty.resolveWith((states) =>
          states.contains(WidgetState.hovered) ||
                  states.contains(WidgetState.pressed)
              ? (dark ? const Color(0x14ffffff) : const Color(0x0f000000))
              : Colors.transparent),
    )),
    cardTheme: CardThemeData(
      elevation: 0, color: surface1,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8), side: BorderSide(color: border)),
    ),
    dialogTheme: DialogThemeData(
      backgroundColor: dark ? surface2 : surface0,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(12)),
    ),
    bottomSheetTheme: BottomSheetThemeData(
      backgroundColor: surface0, surfaceTintColor: Colors.transparent,
      shape: const RoundedRectangleBorder(borderRadius: BorderRadius.vertical(top: Radius.circular(16))),
    ),
    tooltipTheme: const TooltipThemeData(waitDuration: Duration(milliseconds: 500)),
    // Original toast-host surface for any remaining SnackBar: a floating surface0 pill with a
    // 1px border, 16px radius, base-size foreground text (common.dart showToast is canonical).
    snackBarTheme: SnackBarThemeData(
      behavior: SnackBarBehavior.floating,
      backgroundColor: surface0,
      elevation: 2,
      contentTextStyle: ui(14).copyWith(color: foreground),
      actionTextColor: foreground,
      closeIconColor: muted,
      insetPadding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(16),
        side: BorderSide(color: border),
      ),
    ),
    extensions: [
      typography,
      PaseoColors(
        surface0: surface0, surface1: surface1, surface2: surface2,
        surface3: surface3, surface4: surface4,
        sidebar: Color(palette.sidebar), selected: dark ? surface2 : surface4,
        muted: muted, extraMuted: Color(palette.extraMuted),
        accent: accent, accentBright: Color(palette.accentBright),
        added: dark ? const Color(0xff4ade80) : const Color(0xff15803d),
        removed: dark ? const Color(0xffef4444) : const Color(0xffb91c1c),
        success: dark ? const Color(0xff6cb17b) : const Color(0xff3e704a),
        danger: dark ? const Color(0xffd8847b) : const Color(0xff9d433b),
        warning: dark ? const Color(0xffc09664) : const Color(0xff7b5d39),
        running: dark ? const Color(0xff5caaf6) : const Color(0xff268ae0),
        borderAccent: borderAccent,
      ),
    ],
  );
}

TextTheme _baselineGeometry(TextBaseline baseline) {
  final style = TextStyle(textBaseline: baseline);
  return TextTheme(
    displayLarge: style, displayMedium: style, displaySmall: style,
    headlineLarge: style, headlineMedium: style, headlineSmall: style,
    titleLarge: style, titleMedium: style, titleSmall: style,
    bodyLarge: style, bodyMedium: style, bodySmall: style,
    labelLarge: style, labelMedium: style, labelSmall: style,
  );
}
