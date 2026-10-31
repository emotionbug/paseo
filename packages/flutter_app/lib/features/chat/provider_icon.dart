import 'package:flutter/material.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import 'provider_icon_data.dart';

/// Original `components/provider-icons.ts` resolution: built-in glyphs win,
/// then the host snapshot's sanitized SVG, then the vendored catalog, then Bot.
class ProviderIcon extends StatelessWidget {
  const ProviderIcon({
    super.key,
    required this.provider,
    this.snapshotSvg,
    this.size = 16,
    this.color,
  });

  final String provider;
  final String? snapshotSvg;
  final double size;
  final Color? color;

  static String? svgFor(String provider, [String? snapshotSvg]) =>
      builtinProviderIconSvgs[provider] ??
      (snapshotSvg != null && snapshotSvg.isNotEmpty ? snapshotSvg : null) ??
      catalogProviderIconSvgs[provider];

  @override
  Widget build(BuildContext context) {
    final tint = color ?? IconTheme.of(context).color ??
        Theme.of(context).colorScheme.onSurfaceVariant;
    final svg = svgFor(provider, snapshotSvg);
    final bot = Icon(LucideIcons.bot300, size: size, color: tint);
    if (svg == null) return bot;
    return SvgPicture.string(
      svg,
      width: size,
      height: size,
      theme: SvgTheme(currentColor: tint),
      errorBuilder: (_, _, _) => bot,
    );
  }
}
