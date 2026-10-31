import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../../ui/common.dart';
import '../../../ui/controls.dart';
import '../../../ui/localization.dart';
import '../../../ui/theme.dart';
import 'chrome_controls.dart';

TextStyle _base(BuildContext context, {Color? color, FontWeight? weight}) =>
    Theme.of(context).textTheme.bodyMedium!.copyWith(
      color: color ?? PaseoColors.of(context).muted,
      fontWeight: weight ?? FontWeight.normal,
    );

/// agent-panel.tsx renderChatAgentNonReadyView "boot": the large muted
/// spinner centred on surface0 while the agent is first resolved.
class AgentLoadingView extends StatelessWidget {
  const AgentLoadingView({super.key});

  @override
  Widget build(BuildContext context) => ColoredBox(
    color: PaseoColors.of(context).surface0,
    child: const Center(child: ChromeSpinner(large: true)),
  );
}

/// renderChatAgentNonReadyView "not_found": one muted centred line.
class AgentNotFoundView extends StatelessWidget {
  const AgentNotFoundView({super.key});

  @override
  Widget build(BuildContext context) => ColoredBox(
    color: PaseoColors.of(context).surface0,
    child: Center(
      child: Text(
        tr(context, 'agentPanel.states.notFound'),
        textAlign: TextAlign.center,
        style: _base(context),
      ),
    ),
  );
}

/// agent-panel.tsx AgentLoadErrorView: "Failed to load agent", the error 8px
/// under it, then a small secondary Retry button 16px below — all centred,
/// base-size muted text on surface0.
class AgentLoadErrorView extends StatelessWidget {
  const AgentLoadErrorView({super.key, required this.message, required this.onRetry});
  final String message;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) => ColoredBox(
    color: PaseoColors.of(context).surface0,
    child: Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            tr(context, 'agentPanel.states.failedToLoad'),
            textAlign: TextAlign.center,
            style: _base(context),
          ),
          Padding(
            padding: const EdgeInsets.only(top: 8),
            child: Text(message, textAlign: TextAlign.center, style: _base(context)),
          ),
          Padding(
            padding: const EdgeInsets.only(top: 16),
            child: PaseoButton(
              label: tr(context, 'common.actions.retry'),
              size: PaseoControlSize.sm,
              variant: PaseoButtonVariant.secondary,
              onPressed: onRetry,
            ),
          ),
        ],
      ),
    ),
  );
}

/// agent-panel.tsx AgentSessionUnavailableState: the host of an agent that has
/// never loaded is not reachable. While connecting it shows the large muted
/// spinner and "Connecting to …" / "We will show this agent once the host is
/// online."; offline it shows the semibold "Reconnecting to …" title, its
/// description and the last error in small muted text. Items are 12px apart
/// with 24px side padding.
class AgentSessionUnavailableState extends StatelessWidget {
  const AgentSessionUnavailableState({
    super.key,
    required this.serverLabel,
    required this.connecting,
    this.lastError,
  });
  final String serverLabel;
  final bool connecting;
  final String? lastError;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return ColoredBox(
      color: PaseoColors.of(context).surface0,
      child: Center(
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 24),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            spacing: 12,
            children: connecting
                ? [
                    const ChromeSpinner(large: true),
                    Text(
                      tr(context, 'agentPanel.unavailable.connecting',
                          args: {'serverLabel': serverLabel}),
                      textAlign: TextAlign.center,
                      style: _base(context),
                    ),
                    Padding(
                      padding: const EdgeInsets.only(top: 8),
                      child: Text(
                        tr(context, 'agentPanel.unavailable.showWhenOnline'),
                        textAlign: TextAlign.center,
                        style: _base(context),
                      ),
                    ),
                  ]
                : [
                    Text(
                      tr(context, 'agentPanel.unavailable.reconnectingTo',
                          args: {'serverLabel': serverLabel}),
                      textAlign: TextAlign.center,
                      style: _base(
                        context,
                        color: theme.colorScheme.onSurface,
                        weight: FontWeight.w600,
                      ),
                    ),
                    Text(
                      tr(context, 'agentPanel.unavailable.showAgainWhenReachable'),
                      textAlign: TextAlign.center,
                      style: _base(context),
                    ),
                    if (lastError != null && lastError!.isNotEmpty)
                      Text(
                        lastError!,
                        textAlign: TextAlign.center,
                        style: theme.textTheme.bodySmall!.copyWith(
                          fontSize: PaseoTypography.of(context).uiSize(12),
                          color: PaseoColors.of(context).muted,
                        ),
                      ),
                  ],
          ),
        ),
      ),
    );
  }
}

/// agent-panel.tsx historySyncOverlay: surface0 over the whole dock with the
/// large muted spinner, while the first authoritative history loads.
class HistorySyncOverlay extends StatelessWidget {
  const HistorySyncOverlay({super.key});

  @override
  Widget build(BuildContext context) => Positioned.fill(
    child: ColoredBox(
      color: PaseoColors.of(context).surface0,
      child: const Center(child: ChromeSpinner(large: true)),
    ),
  );
}

/// agent-panel.tsx TimelineSyncErrorCallout: under the stream, inside the
/// contentMaxWidth rail (16px sides, 8px top) — a centred row on surface1
/// with a 1px danger border and 16px radius (8/16 padding, 12px gap) holding
/// "Couldn't refresh agent history." and a small secondary Retry that reads
/// "Retrying…" and disables while the retry runs.
class TimelineSyncErrorCallout extends StatelessWidget {
  const TimelineSyncErrorCallout({super.key, required this.retrying, required this.onRetry});
  final bool retrying;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
      child: Center(
        child: ConstrainedBox(
          constraints: BoxConstraints(maxWidth: PaseoTypography.of(context).contentMaxWidth),
          child: Container(
            width: double.infinity,
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
            decoration: BoxDecoration(
              color: colors.surface1,
              border: Border.all(color: colors.danger),
              borderRadius: BorderRadius.circular(16),
            ),
            child: Row(
              mainAxisAlignment: MainAxisAlignment.center,
              spacing: 12,
              children: [
                Flexible(
                  child: Text(
                    tr(context, 'agentPanel.states.timelineSyncFailed'),
                    style: _base(context),
                  ),
                ),
                PaseoButton(
                  label: retrying
                      ? tr(context, 'agentPanel.states.timelineSyncRetrying')
                      : tr(context, 'common.actions.retry'),
                  size: PaseoControlSize.sm,
                  variant: PaseoButtonVariant.secondary,
                  onPressed: retrying ? null : onRetry,
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// components/archived-agent-callout.tsx, in place of the composer for an
/// archived agent: inside the 16px-padded, contentMaxWidth input area, a
/// surface1 row with a 1px borderAccent and 16px radius (12/16 padding on
/// compact, 16/24 wider; 12px gap) holding "This agent is archived" and a
/// small secondary Unarchive button, disabled while offline or unarchiving;
/// a failure shows centred danger text 8px under it.
class ArchivedAgentCallout extends StatefulWidget {
  const ArchivedAgentCallout({
    super.key,
    required this.connected,
    required this.onUnarchive,
  });
  final bool connected;
  final Future<void> Function() onUnarchive;

  @override
  State<ArchivedAgentCallout> createState() => _ArchivedAgentCalloutState();
}

class _ArchivedAgentCalloutState extends State<ArchivedAgentCallout> {
  bool _unarchiving = false;
  String? _error;

  Future<void> _unarchive() async {
    if (!widget.connected || _unarchiving) return;
    setState(() {
      _unarchiving = true;
      _error = null;
    });
    try {
      await widget.onUnarchive();
    } catch (e) {
      // Success leaves the button disabled until the agent's archive state clears.
      if (mounted) {
        setState(() {
          _error = errorMessage(e);
          _unarchiving = false;
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final compact = isCompactLayout(context);
    return Container(
      constraints: const BoxConstraints(minHeight: 75),
      padding: const EdgeInsets.all(16),
      alignment: Alignment.center,
      child: ConstrainedBox(
        constraints: BoxConstraints(maxWidth: PaseoTypography.of(context).contentMaxWidth),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          spacing: 8,
          children: [
            Container(
              padding: EdgeInsets.symmetric(
                horizontal: compact ? 16 : 24,
                vertical: compact ? 12 : 16,
              ),
              decoration: BoxDecoration(
                color: colors.surface1,
                border: Border.all(color: colors.borderAccent),
                borderRadius: BorderRadius.circular(16),
              ),
              child: Row(
                mainAxisAlignment: MainAxisAlignment.center,
                spacing: 12,
                children: [
                  Flexible(
                    child: Text(
                      tr(context, 'agentPanel.archived.callout'),
                      style: _base(context),
                    ),
                  ),
                  PaseoButton(
                    label: tr(context, 'agentPanel.archived.unarchive'),
                    size: PaseoControlSize.sm,
                    variant: PaseoButtonVariant.secondary,
                    onPressed: !widget.connected || _unarchiving ? null : _unarchive,
                  ),
                ],
              ),
            ),
            if (_error != null)
              Text(
                _error!,
                textAlign: TextAlign.center,
                style: _base(context, color: colors.danger),
              ),
          ],
        ),
      ),
    );
  }
}

/// agent-panel.tsx archivingOverlay: an 86% near-black scrim over the panel
/// with the large foreground spinner, the semibold "Archiving agent..." title
/// and its muted subtitle, 12px apart with 32px side padding.
class ArchivingOverlay extends StatelessWidget {
  const ArchivingOverlay({super.key});

  @override
  Widget build(BuildContext context) {
    final foreground = Theme.of(context).colorScheme.onSurface;
    return Positioned.fill(
      child: ColoredBox(
        color: const Color(0xdb080a0e),
        child: Center(
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 32),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              spacing: 12,
              children: [
                ChromeSpinner(large: true, color: foreground),
                Text(
                  tr(context, 'agentPanel.states.archivingTitle'),
                  textAlign: TextAlign.center,
                  style: _base(context, color: foreground, weight: FontWeight.w600),
                ),
                Text(
                  tr(context, 'agentPanel.states.archivingSubtitle'),
                  textAlign: TextAlign.center,
                  style: _base(context),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// agent-stream/view.tsx emptyState: "Start chatting with this agent..." in
/// muted base text, centred in the contentMaxWidth wrapper with 48px of
/// vertical padding.
class StreamEmptyState extends StatelessWidget {
  const StreamEmptyState({super.key});

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 48),
    child: Center(
      child: Text(
        tr(context, 'agentStream.empty'),
        textAlign: TextAlign.center,
        style: _base(context),
      ),
    ),
  );
}

/// panels/pane-subtitle.tsx PaneSubtitle: one 12px muted line (12/4 padding)
/// over a bottom border, between a pane's header and its stream.
class PaneSubtitle extends StatelessWidget {
  const PaneSubtitle({super.key, required this.text});
  final String text;

  @override
  Widget build(BuildContext context) => Container(
    width: double.infinity,
    padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
    decoration: BoxDecoration(
      border: Border(bottom: BorderSide(color: Theme.of(context).dividerColor)),
    ),
    child: Text(
      text,
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: Theme.of(context).textTheme.bodySmall!.copyWith(
        fontSize: PaseoTypography.of(context).uiSize(12),
        color: PaseoColors.of(context).muted,
      ),
    ),
  );
}

/// strategy-native.tsx historyStartSlot: a 32px slot at the start of the
/// transcript that holds the small muted spinner while older history loads.
class HistoryStartSlot extends StatelessWidget {
  const HistoryStartSlot({super.key, required this.loading});
  final bool loading;

  @override
  Widget build(BuildContext context) => SizedBox(
    height: 32,
    child: loading ? const Center(child: ChromeSpinner()) : null,
  );
}

/// agent-stream/view.tsx scrollToBottomButton: a 48px surface2 circle with
/// the sm shadow and a 24px foreground ChevronDown, centred over the stream
/// and fading in/out over 200ms.
class JumpToLatestButton extends StatelessWidget {
  const JumpToLatestButton({super.key, required this.visible, required this.onPressed});
  final bool visible;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final dark = Theme.of(context).brightness == Brightness.dark;
    // heightFactor 1 and the fade/pointer gate inside it: every box from the
    // Positioned slot down is the 48px button, so its accessibility node is
    // the visible control, not the stream area the slot spans.
    return Center(
      heightFactor: 1,
      child: IgnorePointer(
        ignoring: !visible,
        child: AnimatedOpacity(
          opacity: visible ? 1 : 0,
          duration: const Duration(milliseconds: 200),
          child: Semantics(
            container: true,
            button: true,
            label: tr(context, 'agentStream.scrollToBottom'),
            excludeSemantics: true,
            child: GestureDetector(
              onTap: onPressed,
              child: MouseRegion(
                cursor: SystemMouseCursors.click,
                child: Container(
                  width: 48,
                  height: 48,
                  decoration: BoxDecoration(
                    color: PaseoColors.of(context).surface2,
                    shape: BoxShape.circle,
                    boxShadow: [
                      BoxShadow(
                        color: dark ? const Color(0x40000000) : const Color(0x14000000),
                        offset: const Offset(0, 2),
                        blurRadius: dark ? 4 : 8,
                      ),
                    ],
                  ),
                  child: Icon(
                    LucideIcons.chevronDown300,
                    size: 24,
                    color: Theme.of(context).colorScheme.onSurface,
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
