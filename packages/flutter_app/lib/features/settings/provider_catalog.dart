import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import '../../ui/command_center.dart' show PaseoMatchScore, paseoCompareMatchScores, paseoScoreTextFields;
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../chat/provider_icon.dart';
import 'settings_forms.dart';
import 'settings_host_widgets.dart';
import 'usage_settings.dart';

// Canonical ACP catalog. Update package pins with npm run acp:version-drift:update.
const acpProviderCatalog = <Map<String, dynamic>>[
  {
    "id": "agoragentic-acp",
    "title": "Agoragentic",
    "description": "Agent marketplace with 174+ AI capabilities. Browse, invoke, and pay for agent services settled in USDC on Base L2.",
    "version": "1.3.6",
    "installLink": "https://agoragentic.com",
    "command": ["npx", "-y", "agoragentic-mcp@1.3.6", "--acp"],
  },
  {
    "id": "amp-acp",
    "title": "Amp",
    "description": "ACP wrapper for Amp - the frontier coding agent",
    "version": "0.7.0",
    "installLink": "https://github.com/tao12345666333/amp-acp",
    "command": ["amp-acp"],
  },
  {
    "id": "auggie",
    "title": "Auggie CLI",
    "description": "Augment Code's powerful software agent, backed by industry-leading context engine",
    "version": "0.36.0",
    "installLink": "https://www.augmentcode.com/",
    "command": ["npx", "-y", "@augmentcode/auggie@0.36.0", "--acp"],
    "env": {"AUGMENT_DISABLE_AUTO_UPDATE": "1"},
  },
  {
    "id": "autohand",
    "title": "Autohand Code",
    "description": "Autohand Code - AI coding agent powered by Autohand AI",
    "version": "0.2.1",
    "installLink": "https://www.autohand.ai/cli/",
    "command": ["npx", "-y", "@autohandai/autohand-acp@0.2.1"],
  },
  {
    "id": "cline",
    "title": "Cline",
    "description": "Autonomous coding agent CLI - capable of creating/editing files, running commands, using the browser, and more",
    "version": "3.0.73",
    "installLink": "https://cline.bot/cli",
    "command": ["npx", "-y", "cline@3.0.73", "--acp"],
  },
  {
    "id": "codebuddy-code",
    "title": "Codebuddy Code",
    "description": "Tencent Cloud's official intelligent coding tool",
    "version": "manual",
    "installLink": "https://www.codebuddy.cn/cli/",
    "command": ["codebuddy", "--acp"],
  },
  {
    "id": "codewhale",
    "title": "CodeWhale",
    "description": "Terminal coding agent for DeepSeek V4 and open models",
    "version": "0.8.55",
    "installLink": "https://codewhale.net/",
    "command": ["codewhale", "serve", "--acp"],
  },
  {
    "id": "cortex-code",
    "title": "Cortex Code",
    "description": "Snowflake's Cortex Code coding agent",
    "version": "1.0.73",
    "installLink":
        "https://docs.snowflake.com/en/user-guide/cortex-code/cortex-code-cli",
    "command": ["cortex", "acp", "serve"],
  },
  {
    "id": "corust-agent",
    "title": "Corust Agent",
    "description": "Co-building with a seasoned Rust partner.",
    "version": "0.5.1",
    "installLink": "https://github.com/Corust-ai/corust-agent-release/releases",
    "command": ["corust-agent-acp"],
  },
  {
    "id": "crow-cli",
    "title": "crow-cli",
    "description": "Minimal ACP Native Coding Agent",
    "version": "0.1.23",
    "installLink": "https://crow-ai.dev/",
    "command": ["crow-cli", "acp"],
  },
  {
    "id": "cursor",
    "title": "Cursor",
    "description": "Cursor's coding agent",
    "version": "2026.03.30",
    "installLink": "https://docs.cursor.com/en/cli/overview",
    "command": ["cursor-agent", "acp"],
  },
  {
    "id": "deepagents",
    "title": "DeepAgents",
    "description": "Batteries-included AI coding and general purpose agent powered by LangChain.",
    "version": "0.1.36",
    "installLink":
        "https://docs.langchain.com/oss/javascript/deepagents/overview",
    "command": ["npx", "-y", "deepagents-acp@0.1.36"],
  },
  {
    "id": "devin",
    "title": "Devin CLI",
    "description": "Cognition's Devin for Terminal via Agent Client Protocol",
    "version": "manual",
    "installLink": "https://cli.devin.ai/docs",
    "command": ["devin", "acp"],
  },
  {
    "id": "dimcode",
    "title": "DimCode",
    "description": "A coding agent that puts leading models at your command.",
    "version": "0.5.21",
    "installLink": "https://dimcode.dev/docs/acp.html",
    "command": ["npx", "-y", "dimcode@0.5.21", "acp"],
  },
  {
    "id": "dirac",
    "title": "Dirac",
    "description": "Reduces API costs by more than 50%, produces better and faster work. Uses Hash anchored parallel edits, AST manipulation and a whole lot of neat optimizations. Fully Open Source.",
    "version": "0.5.23",
    "installLink": "https://dirac.run",
    "command": ["npx", "-y", "dirac-cli@0.5.23", "--acp"],
  },
  {
    "id": "factory-droid",
    "title": "Factory Droid",
    "description": "Factory Droid - AI coding agent powered by Factory AI",
    "version": "0.249.0",
    "installLink": "https://factory.ai/product/cli",
    "command": [
      "npx",
      "-y",
      "droid@0.249.0",
      "exec",
      "--output-format",
      "acp-daemon",
    ],
    "env": {
      "DROID_DISABLE_AUTO_UPDATE": "true",
      "FACTORY_DROID_AUTO_UPDATE_ENABLED": "false",
    },
    "params": {"supportsMcpServers": false},
  },
  {
    "id": "fast-agent",
    "title": "fast-agent",
    "description":
        "Code and build agents with comprehensive multi-provider support",
    "version": "0.10.47",
    "installLink": "https://fast-agent.ai/acp/",
    "command": [
      "uvx",
      "--from",
      "fast-agent-acp==0.10.47",
      "fast-agent-acp",
      "-x",
    ],
  },
  {
    "id": "gemini",
    "title": "Gemini CLI",
    "description": "Google's official CLI for Gemini",
    "version": "0.65.0",
    "installLink": "https://geminicli.com",
    "command": ["npx", "-y", "@google/gemini-cli@0.65.0", "--acp"],
  },
  {
    "id": "gjc",
    "title": "Gajae Code",
    "description": "Runs on the Claude/Codex/Gemini subscription you already pay for. Plan-before-mutation workflows, evidence-gated execution, and approval prompts for shell and destructive edits.",
    "version": "manual",
    "installLink": "https://gajae-code.com",
    "command": ["gjc", "acp"],
    "env": {"GJC_ACP_PERMISSION_MODE": "prompt"},
  },
  {
    "id": "glm-acp-agent",
    "title": "GLM Agent",
    "description": "ACP agent powered by Zhipu AI's GLM Coding Plan models (glm-5.3, glm-5.3-flash, glm-5-turbo, glm-4.7). Supports streaming, tool calls, mid-session model switching, image input via Z.AI Coding Plan Vision MCP, and session load/fork/resume with on-disk persistence.",
    "version": "1.15.0",
    "installLink": "https://github.com/stefandevo/glm-acp-agent",
    "command": ["npx", "-y", "glm-acp-agent@1.15.0"],
  },
  {
    "id": "goose",
    "title": "goose",
    "description": "A local, extensible, open source AI agent that automates engineering tasks",
    "version": "1.33.1",
    "installLink": "https://block.github.io/goose/",
    "command": ["goose", "acp"],
  },
  {
    "id": "grok",
    "title": "Grok",
    "description": "xAI's Grok Build agentic coding CLI with plan mode and parallel subagents. Requires a SuperGrok or X Premium+ subscription.",
    "version": "0.2.11",
    "installLink": "https://docs.x.ai/build/overview",
    "command": ["grok", "agent", "stdio"],
  },
  {
    "id": "hermes",
    "title": "Hermes",
    "description": "Nous Research self-improving AI agent",
    "version": "manual",
    "installLink":
        "https://hermes-agent.nousresearch.com/docs/user-guide/features/acp",
    "command": ["hermes", "acp"],
  },
  {
    "id": "hoplite",
    "bundledProvider": true,
    "title": "Hoplite",
    "description": "Cloud coding agent through the local hoplite acp bridge",
    "version": "manual",
    "installLink": "https://hoplite.sh/docs/cli#install",
    "command": ["hoplite", "acp"],
  },
  {
    "id": "junie",
    "title": "Junie",
    "description": "AI Coding Agent by JetBrains",
    "version": "1468.30.0",
    "installLink": "https://junie.jetbrains.com/docs/junie-cli-acp.html",
    "command": ["junie", "--acp", "true"],
  },
  {
    "id": "kilo",
    "title": "Kilo",
    "description": "The open source coding agent",
    "version": "7.2.40",
    "installLink": "https://kilo.ai/docs/code-with-ai/platforms/cli",
    "command": ["kilo", "acp"],
  },
  {
    "id": "kiro",
    "title": "Kiro CLI",
    "description": "Amazon's AI coding agent with native ACP support",
    "version": "manual",
    "installLink": "https://kiro.dev/docs/cli/acp/",
    "command": ["kiro-cli", "acp"],
  },
  {
    "id": "kimi",
    "title": "Kimi Code CLI",
    "description": "Moonshot AI's open-source terminal coding agent",
    "version": "0.11.0",
    "installLink": "https://github.com/MoonshotAI/kimi-code",
    "command": ["kimi", "acp"],
  },
  {
    "id": "minimax-code",
    "title": "MiniMax Code",
    "description": "MiniMax's coding agent for the terminal",
    "version": "0.6.13",
    "installLink": "https://agent.minimax.io",
    "command": ["npx", "-y", "@minimax-ai/code@0.6.13", "acp"],
  },
  {
    "id": "minion-code",
    "title": "Minion Code",
    "description": "An enhanced AI code assistant built on the Minion framework with rich development tools",
    "version": "0.1.45",
    "installLink": "https://github.com/femto/minion-code",
    "command": ["uvx", "--from", "minion-code==0.1.45", "minion-code", "acp"],
  },
  {
    "id": "mistral-vibe",
    "title": "Mistral Vibe",
    "description": "Mistral's open-source coding assistant",
    "version": "2.9.3",
    "installLink": "https://github.com/mistralai/mistral-vibe",
    "command": ["vibe-acp"],
  },
  {
    "id": "nova",
    "title": "Nova",
    "description": "Nova by Compass AI - a fully-fledged software engineer at your command",
    "version": "1.1.59",
    "installLink": "https://www.compassap.ai/portfolio/nova.html",
    "command": ["npx", "-y", "@compass-ai/nova@1.1.59", "acp"],
  },
  {
    "id": "poolside",
    "title": "Poolside",
    "description": "Poolside's coding agent",
    "version": "1.0.0",
    "installLink": "https://docs.poolside.ai/cli/pool",
    "command": ["pool", "acp"],
  },
  {
    "id": "qoder",
    "title": "Qoder CLI",
    "description": "AI coding assistant with agentic capabilities",
    "version": "1.1.78",
    "installLink": "https://qoder.com",
    "command": ["npx", "-y", "@qoder-ai/qodercli@1.1.78", "--acp"],
  },
  {
    "id": "qwen-code",
    "title": "Qwen Code",
    "description": "Alibaba's Qwen coding assistant",
    "version": "0.26.0",
    "installLink": "https://qwenlm.github.io/qwen-code-docs/en/users/overview",
    "command": [
      "npx",
      "-y",
      "@qwen-code/qwen-code@0.26.0",
      "--acp",
      "--experimental-skills",
    ],
  },
  {
    "id": "sigit",
    "title": "siGit Code",
    "description": "Local-first coding agent. Runs entirely on your machine with optional on-device LLM inference via Onde.",
    "version": "1.0.3",
    "installLink": "https://github.com/getsigit/sigit",
    "command": ["sigit"],
  },
  {
    "id": "stakpak",
    "title": "Stakpak",
    "description":
        "Open-source DevOps agent in Rust with enterprise-grade security",
    "version": "0.3.80",
    "installLink": "https://stakpak.dev/",
    "command": ["stakpak", "acp"],
  },
  {
    "id": "traecli",
    "title": "TRAE CLI",
    "description":
        "ByteDance's official TRAE coding agent with native ACP support",
    "version": "manual",
    "installLink": "https://docs.trae.cn/cli_get-started-with-trae-cli",
    "command": ["traecli", "acp", "serve"],
  },
  {
    "id": "vtcode",
    "title": "VT Code",
    "description": "An open-source coding agent with LLM-native code understanding and robust shell safety. Supports multiple LLM providers with automatic failover and efficient context management.",
    "version": "0.96.14",
    "installLink":
        "https://github.com/vinhnx/VTCode/blob/main/docs/guides/zed-acp.md",
    "command": ["vtcode", "acp"],
    "env": {"VT_ACP_ENABLED": "1", "VT_ACP_ZED_ENABLED": "1"},
  },
];

/// Original `buildAcpProviderConfigPatch`: bundled providers are enabled in
/// place; registry entries become `extends: acp` providers.
JsonMap buildAcpProviderConfigPatch(JsonMap entry, {required bool supportsHoplite}) {
  final id = stringValue(entry['id']);
  if (entry['bundledProvider'] == true) {
    if (!supportsHoplite) throw StateError('Update the host to add Hoplite.');
    return {
      'providers': {
        id: {'enabled': true},
      },
    };
  }
  return {
    'providers': {
      id: {
        'extends': 'acp',
        'label': entry['title'],
        'description': entry['description'],
        'command': [...jsonList(entry['command'])],
        'env': {...jsonMap(entry['env'])},
        if (entry['params'] != null) 'params': {...jsonMap(entry['params'])},
      },
    },
  };
}

/// Original `rankProviderCatalog`: alphabetical without a query; otherwise
/// the pickers' `scoreTextFields` over the title and id, best match first,
/// ties alphabetical.
List<JsonMap> rankProviderCatalog(Iterable<JsonMap> entries, String query) {
  int byTitle(JsonMap a, JsonMap b) => stringValue(a['title'])
      .toLowerCase()
      .compareTo(stringValue(b['title']).toLowerCase());
  if (query.trim().isEmpty) return entries.toList()..sort(byTitle);

  final scored = <(JsonMap, PaseoMatchScore)>[];
  for (final entry in entries) {
    final score = paseoScoreTextFields(query, [stringValue(entry['title']), stringValue(entry['id'])]);
    if (score != null) scored.add((entry, score));
  }
  scored.sort((a, b) {
    final byScore = paseoCompareMatchScores(a.$2, b.$2);
    return byScore != 0 ? byScore : byTitle(a.$1, b.$1);
  });
  return [for (final (entry, _) in scored) entry];
}

/// Original provider sheet `rankModels`: input order without a query, else
/// `scoreTextFields` best match first (stable within equal scores).
List<JsonMap> _rankModels(List<JsonMap> items, String query, List<String> Function(JsonMap) fields) {
  if (query.trim().isEmpty) return items;
  final scored = <(JsonMap, PaseoMatchScore, int)>[];
  for (var i = 0; i < items.length; i++) {
    final score = paseoScoreTextFields(query, fields(items[i]));
    if (score != null) scored.add((items[i], score, i));
  }
  scored.sort((a, b) {
    final byScore = paseoCompareMatchScores(a.$2, b.$2);
    return byScore != 0 ? byScore : a.$3 - b.$3;
  });
  return [for (final (item, _, _) in scored) item];
}

/// Original `resolveProviderDiscoveredModels`: the sheet lists the provider's
/// selectable models; while its snapshot refreshes with none, the last list
/// for the same host and provider stays on screen.
({List<JsonMap> models, ({String serverId, String provider, List<JsonMap> models})? cache})
resolveProviderDiscoveredModels({
  required String serverId,
  required String provider,
  required List<JsonMap> currentModels,
  required bool providerSnapshotRefreshing,
  required ({String serverId, String provider, List<JsonMap> models})? previousCache,
}) {
  final selectable = currentModels.where((model) => model['isSelectable'] != false).toList();
  if (selectable.isNotEmpty) {
    return (models: selectable, cache: (serverId: serverId, provider: provider, models: selectable));
  }
  if (providerSnapshotRefreshing &&
      previousCache?.serverId == serverId &&
      previousCache?.provider == provider) {
    return (models: previousCache!.models, cache: previousCache);
  }
  return (models: const [], cache: previousCache);
}

/// Original `components/provider-catalog-list.tsx`: a search rail (12px below)
/// over a wrapping grid of catalog tiles. Columns fit 300px tiles with 12px
/// gaps (at most four); a single column stretches tiles to the full row.
class ProviderCatalogList extends StatefulWidget {
  const ProviderCatalogList({
    super.key,
    required this.installedIds,
    required this.installingId,
    required this.onInstall,
  });
  final Set<String> installedIds;
  final String? installingId;
  final ValueChanged<JsonMap> onInstall;
  @override
  State<ProviderCatalogList> createState() => _ProviderCatalogListState();
}

class _ProviderCatalogListState extends State<ProviderCatalogList> {
  static const _gap = 12.0, _minTile = 300.0, _maxColumns = 4;
  String search = '';

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final entries = rankProviderCatalog(
      acpProviderCatalog.where((entry) => !widget.installedIds.contains(entry['id'])),
      search,
    );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.only(bottom: 12),
          child: SettingsSearchField(
            placeholder: tr(context, 'providerCatalog.search'),
            clearLabel: tr(context, 'providerCatalog.clearSearch'),
            onChanged: (value) => setState(() => search = value),
          ),
        ),
        if (entries.isEmpty)
          Container(
            constraints: const BoxConstraints(minHeight: 96),
            alignment: Alignment.center,
            padding: const EdgeInsets.all(16),
            decoration: BoxDecoration(
              color: colors.surface1,
              border: Border.all(color: theme.dividerColor),
              borderRadius: BorderRadius.circular(8),
            ),
            child: Text(
              tr(context, 'providerCatalog.noProviders'),
              style: theme.textTheme.bodyMedium!.copyWith(
                color: colors.muted,
                fontSize: type.uiSize(14),
              ),
            ),
          )
        else
          LayoutBuilder(
            builder: (context, constraints) {
              final width = constraints.maxWidth;
              final fits = ((width + _gap) / (_minTile + _gap)).floor();
              final columns = fits.clamp(1, _maxColumns);
              final tile = columns == 1
                  ? width
                  : ((width - _gap * (columns - 1)) / columns).floorToDouble();
              return Wrap(
                spacing: _gap,
                runSpacing: _gap,
                children: [
                  for (final entry in entries)
                    SizedBox(
                      width: tile,
                      child: _CatalogTile(
                        entry: entry,
                        installing: widget.installingId == entry['id'],
                        onInstall: () => widget.onInstall(entry),
                      ),
                    ),
                ],
              );
            },
          ),
      ],
    );
  }
}

/// Original `CatalogTile`: settings card, 12px padding and gaps; a 36px
/// surface2 icon frame (6px radius) with the 24px registry glyph or a 20px
/// PackagePlus, the base-size name over an xs ACP badge and install link
/// (8px gaps), and an xs outline Add button.
class _CatalogTile extends StatelessWidget {
  const _CatalogTile({
    required this.entry,
    required this.installing,
    required this.onInstall,
  });
  final JsonMap entry;
  final bool installing;
  final VoidCallback onInstall;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final id = stringValue(entry['id']);
    final title = stringValue(entry['title'], id);
    final foreground = theme.colorScheme.onSurface;
    final link = stringValue(entry['installLink']);
    return Container(
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: colors.surface1,
        border: Border.all(color: theme.dividerColor),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        spacing: 12,
        children: [
          Container(
            width: 36,
            height: 36,
            alignment: Alignment.center,
            decoration: BoxDecoration(
              color: colors.surface2,
              borderRadius: BorderRadius.circular(6),
            ),
            child: ProviderIcon.svgFor(id) == null
                ? Icon(LucideIcons.packagePlus300, size: 20, color: foreground)
                : ProviderIcon(provider: id, size: 24, color: foreground),
          ),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              spacing: 4,
              children: [
                Text(
                  title,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.bodyMedium!.copyWith(
                    fontSize: type.uiSize(14),
                    fontWeight: FontWeight.normal,
                    color: foreground,
                  ),
                ),
                Wrap(
                  spacing: 8,
                  runSpacing: 4,
                  crossAxisAlignment: WrapCrossAlignment.center,
                  children: [
                    StatusBadge(label: tr(context, 'providerCatalog.types.acp'), xs: true),
                    if (link.isNotEmpty)
                      SettingsExternalLink(
                        label: tr(context, 'providerCatalog.actions.installInstructions'),
                        url: link,
                        semanticLabel: tr(
                          context,
                          'providerCatalog.actions.installInstructionsFor',
                          args: {'provider': title},
                        ),
                      ),
                  ],
                ),
              ],
            ),
          ),
          PaseoButton(
            label: tr(
              context,
              installing ? 'providerCatalog.actions.adding' : 'providerCatalog.actions.add',
            ),
            semanticLabel: tr(
              context,
              'providerCatalog.actions.addProvider',
              args: {'provider': title},
            ),
            size: PaseoControlSize.xs,
            variant: PaseoButtonVariant.outline,
            loading: installing,
            onPressed: installing ? null : onInstall,
          ),
        ],
      ),
    );
  }
}

/// The daemon config and provider snapshot the providers UI renders. The host
/// settings page republishes every change so open provider sheets stay live.
typedef ProviderSettingsSnapshot = ({JsonMap config, List<JsonMap> providers});

enum _StatusTone { success, warning, danger, muted, loading }

typedef _ProviderStatus = ({_StatusTone tone, String label, int? modelCount});

/// Original `getProviderStatus`.
_ProviderStatus _providerStatus(
  BuildContext context,
  String status,
  bool enabled,
  int modelCount,
) {
  String t(String key) => tr(context, 'settings.providers.statuses.$key');
  if (!enabled) return (tone: _StatusTone.muted, label: t('disabled'), modelCount: null);
  if (status == 'loading') {
    return (tone: _StatusTone.loading, label: t('loading'), modelCount: null);
  }
  if (status == 'error') return (tone: _StatusTone.danger, label: t('error'), modelCount: null);
  if (status == 'ready') {
    return (
      tone: _StatusTone.success,
      label: t('available'),
      modelCount: modelCount > 0 ? modelCount : null,
    );
  }
  return (tone: _StatusTone.warning, label: t('notInstalled'), modelCount: null);
}

/// Original `filterSelectableModels(...).length`.
int _selectableModelCount(JsonMap entry) =>
    jsonList(entry['models']).map(jsonMap).where((m) => m['isSelectable'] != false).length;

/// Original `screens/settings/providers-section.tsx`: the host's provider rows
/// (section 16px below) and, once connected, the "Add provider" catalog 16px
/// further down. Rows open the provider settings sheet.
class ProvidersSection extends StatefulWidget {
  const ProvidersSection({
    super.key,
    required this.client,
    required this.snapshot,
    required this.connected,
    required this.loading,
    required this.supportsHoplite,
    required this.supportsRemoval,
    required this.patch,
    required this.refresh,
    this.onConfigure,
  });
  final PaseoClient client;
  final ValueListenable<ProviderSettingsSnapshot> snapshot;
  final bool connected, loading, supportsHoplite, supportsRemoval;
  final Future<void> Function(JsonMap patch) patch;
  final Future<void> Function(List<String> providers) refresh;

  /// The command/env editor (not in the original app), offered from the
  /// provider sheet footer when the connection may manage the daemon.
  final void Function(JsonMap entry)? onConfigure;

  @override
  State<ProvidersSection> createState() => _ProvidersSectionState();
}

class _ProvidersSectionState extends State<ProvidersSection> {
  String? pendingId, removingId, installingId;

  /// Original `isConfigOwnedProvider`: derived (`extends`) providers and
  /// opted-in bundled catalog providers live in config.json.
  bool _configOwned(JsonMap config, String id) {
    final providers = jsonMap(config['providers']);
    if (!providers.containsKey(id)) return false;
    return jsonMap(providers[id])['extends'] != null ||
        acpProviderCatalog.any((e) => e['id'] == id && e['bundledProvider'] == true);
  }

  Future<void> _toggle(String id, bool enabled) async {
    setState(() => pendingId = id);
    try {
      await widget.patch({
        'providers': {
          id: {'enabled': enabled},
        },
      });
    } catch (e) {
      if (mounted) showError(context, e);
    } finally {
      if (mounted && pendingId == id) setState(() => pendingId = null);
    }
  }

  Future<void> _remove(String id, String label) async {
    if (removingId != null) return;
    setState(() => removingId = id);
    try {
      final confirmed = await confirmAction(
        context,
        title: tr(context, 'settings.providers.remove.confirmTitle', args: {'name': label}),
        message: tr(context, 'settings.providers.remove.confirmMessage'),
        confirmLabel: tr(context, 'settings.providers.remove.confirm'),
        destructive: true,
      );
      if (confirmed) {
        await widget.patch({
          'removeProviders': [id],
        });
      }
    } catch (e) {
      if (mounted) showError(context, e);
    } finally {
      if (mounted && removingId == id) setState(() => removingId = null);
    }
  }

  Future<void> _install(JsonMap entry) async {
    if (installingId != null) return;
    final id = stringValue(entry['id']);
    setState(() => installingId = id);
    try {
      await widget.patch(
        buildAcpProviderConfigPatch(entry, supportsHoplite: widget.supportsHoplite),
      );
      await widget.refresh([id]);
    } catch (e) {
      if (mounted) showError(context, e);
    } finally {
      if (mounted && installingId == id) setState(() => installingId = null);
    }
  }

  @override
  Widget build(BuildContext context) {
    String t(String key) => tr(context, 'settings.providers.$key');
    return ValueListenableBuilder<ProviderSettingsSnapshot>(
      valueListenable: widget.snapshot,
      builder: (context, snapshot, _) {
        final entries = snapshot.providers;
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.only(bottom: 16),
              child: SettingsSection(
                title: t('title'),
                card: false,
                flush: true,
                children: [
                  if (!widget.connected)
                    SettingsEmptyCard(t('unavailable'))
                  else if (widget.loading)
                    SettingsEmptyCard(t('loading'))
                  else if (entries.isNotEmpty)
                    SettingsRowsCard(
                      children: [
                        for (final entry in entries)
                          _ProviderRow(
                            key: ValueKey(entry['provider']),
                            entry: entry,
                            toggling: pendingId == entry['provider'],
                            removing: removingId == entry['provider'],
                            canRemove: widget.supportsRemoval &&
                                entry['source'] == 'custom' &&
                                _configOwned(snapshot.config, stringValue(entry['provider'])),
                            onPress: () => showProviderSettingsSheet(
                              context,
                              client: widget.client,
                              provider: stringValue(entry['provider']),
                              snapshot: widget.snapshot,
                              patch: widget.patch,
                              refresh: widget.refresh,
                              onConfigure: widget.onConfigure,
                            ),
                            onToggle: (value) => _toggle(stringValue(entry['provider']), value),
                            onRemove: () => _remove(
                              stringValue(entry['provider']),
                              stringValue(entry['label'], stringValue(entry['provider'])),
                            ),
                          ),
                      ],
                    ),
                ],
              ),
            ),
            if (widget.connected)
              Padding(
                padding: const EdgeInsets.only(top: 16),
                child: SettingsSection(
                  title: t('addProvider'),
                  card: false,
                  children: [
                    ProviderCatalogList(
                      installedIds: {for (final e in entries) stringValue(e['provider'])},
                      installingId: installingId,
                      onInstall: _install,
                    ),
                  ],
                ),
              ),
          ],
        );
      },
    );
  }
}

/// Original `ProviderRow`: a pressable 56px settings row (surface2 hover,
/// surface3 pressed) with a sm chevron (foreground on hover), the md provider
/// glyph, the base label · status, a 3-line red-300 error under it, and a
/// switch beside a 32px actions slot.
class _ProviderRow extends StatefulWidget {
  const _ProviderRow({
    super.key,
    required this.entry,
    required this.toggling,
    required this.removing,
    required this.canRemove,
    required this.onPress,
    required this.onToggle,
    required this.onRemove,
  });
  final JsonMap entry;
  final bool toggling, removing, canRemove;
  final VoidCallback onPress, onRemove;
  final ValueChanged<bool> onToggle;
  @override
  State<_ProviderRow> createState() => _ProviderRowState();
}

class _ProviderRowState extends State<_ProviderRow> {
  bool hovered = false, pressed = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final compact = isCompactLayout(context);
    final entry = widget.entry;
    final id = stringValue(entry['provider']);
    final label = stringValue(entry['label'], id);
    final enabled = entry['enabled'] != false;
    final status = stringValue(entry['status']);
    final rawError = stringValue(entry['error']).trim();
    final error = enabled && status == 'error' && rawError.isNotEmpty ? rawError : null;
    final providerStatus = _providerStatus(context, status, enabled, _selectableModelCount(entry));
    final base = theme.textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14));
    return Semantics(
      button: true,
      label: tr(context, 'settings.providers.providerDetails', args: {'name': label}),
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: (_) => setState(() => pressed = true),
          onTapCancel: () => setState(() => pressed = false),
          onTapUp: (_) => setState(() => pressed = false),
          onTap: widget.onPress,
          child: ColoredBox(
            color: pressed
                ? colors.surface3
                : hovered
                ? colors.surface2
                : Colors.transparent,
            child: ConstrainedBox(
              constraints: const BoxConstraints(minHeight: 56),
              child: Padding(
                padding: const EdgeInsets.all(16),
                child: Row(
                  spacing: 12,
                  children: [
                    Expanded(
                      child: Row(
                        spacing: 8,
                        children: [
                          Icon(
                            LucideIcons.chevronRight300,
                            size: densityIcon(context, 'sm'),
                            color: hovered ? theme.colorScheme.onSurface : colors.muted,
                          ),
                          ProviderIcon(
                            provider: id,
                            snapshotSvg: stringValue(entry['iconSvg']),
                            size: densityIcon(context, 'md'),
                            color: theme.colorScheme.onSurface,
                          ),
                          Expanded(
                            child: Column(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              mainAxisSize: MainAxisSize.min,
                              children: [
                                Row(
                                  spacing: 8,
                                  children: [
                                    Flexible(
                                      child: Text(
                                        label,
                                        maxLines: 1,
                                        overflow: TextOverflow.ellipsis,
                                        style: base,
                                      ),
                                    ),
                                    if (!compact)
                                      Text('·', style: base.copyWith(color: colors.muted)),
                                    _StatusIndicator(status: providerStatus, compact: compact),
                                  ],
                                ),
                                if (error != null && !compact)
                                  Padding(
                                    padding: const EdgeInsets.only(top: 4),
                                    child: Text(
                                      error,
                                      maxLines: 3,
                                      overflow: TextOverflow.ellipsis,
                                      style: theme.textTheme.bodySmall!.copyWith(
                                        color: paseoFieldErrorColor,
                                        fontSize: type.uiSize(12),
                                      ),
                                    ),
                                  ),
                              ],
                            ),
                          ),
                        ],
                      ),
                    ),
                    Row(
                      mainAxisSize: MainAxisSize.min,
                      spacing: 4,
                      children: [
                        PaseoSwitch(
                          value: enabled,
                          semanticLabel: tr(
                            context,
                            'settings.providers.enableProvider',
                            args: {'name': label},
                          ),
                          onChanged: widget.toggling || widget.removing ? null : widget.onToggle,
                        ),
                        SizedBox.square(
                          dimension: 32,
                          child: widget.canRemove
                              ? SettingsMenuButton(
                                  label: tr(
                                    context,
                                    'settings.providers.actions.menu',
                                    args: {'name': label},
                                  ),
                                  iconSize: densityIcon(context, 'sm'),
                                  enabled: !widget.removing,
                                  options: [
                                    PaseoMenuEntry.item(
                                      value: 'remove',
                                      label: tr(context, 'settings.providers.actions.remove'),
                                      icon: LucideIcons.trash2300,
                                      destructive: true,
                                    ),
                                  ],
                                  onSelected: (_) => widget.onRemove(),
                                )
                              : null,
                        ),
                      ],
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Original `StatusIndicator`: an 8px tone dot (10px spinner while loading)
/// and, outside compact layouts, the muted base label · model count, 6px gaps.
class _StatusIndicator extends StatelessWidget {
  const _StatusIndicator({required this.status, required this.compact});
  final _ProviderStatus status;
  final bool compact;
  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final muted = Theme.of(context).textTheme.bodyMedium!.copyWith(
      fontSize: type.uiSize(14),
      color: colors.muted,
    );
    final dot = switch (status.tone) {
      _StatusTone.success => colors.success,
      _StatusTone.warning => colors.warning,
      _StatusTone.danger => colors.danger,
      _ => colors.muted,
    };
    final count = status.modelCount;
    return Row(
      mainAxisSize: MainAxisSize.min,
      spacing: 6,
      children: [
        if (status.tone == _StatusTone.loading)
          const SettingsSpinner(size: 10)
        else
          Container(
            width: 8,
            height: 8,
            decoration: BoxDecoration(color: dot, shape: BoxShape.circle),
          ),
        if (!compact) ...[
          Text(status.label, style: muted),
          if (count != null) ...[
            Text('·', style: muted),
            Text(
              count == 1
                  ? tr(context, 'settings.providers.models.one')
                  : tr(context, 'settings.providers.models.many', args: {'count': count}),
              style: muted,
            ),
          ],
        ],
      ],
    );
  }
}

/// Original `ProviderDiagnosticSheet` (the provider settings sheet).
Future<void> showProviderSettingsSheet(
  BuildContext context, {
  required PaseoClient client,
  required String provider,
  required ValueListenable<ProviderSettingsSnapshot> snapshot,
  required Future<void> Function(JsonMap patch) patch,
  required Future<void> Function(List<String> providers) refresh,
  void Function(JsonMap entry)? onConfigure,
}) => showAdaptiveSheet<void>(
  context: context,
  builder: (context) => _ProviderSettingsSheet(
    client: client,
    provider: provider,
    snapshot: snapshot,
    patch: patch,
    refresh: refresh,
    onConfigure: onConfigure,
  ),
);

List<JsonMap> _additionalModels(JsonMap config, String provider) => jsonList(
  jsonMap(jsonMap(config['providers'])[provider])['additionalModels'],
).map(jsonMap).toList();

/// Original sheet search header: a muted md Search glyph and a borderless
/// base input (8px vertical padding), 24px inset, 12px above the rule.
class _SheetSearchRow extends StatelessWidget {
  const _SheetSearchRow({required this.placeholder, required this.onChanged});
  final String placeholder;
  final ValueChanged<String> onChanged;
  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(24, 0, 24, 12),
      child: Row(
        spacing: 8,
        children: [
          Icon(LucideIcons.search300, size: densityIcon(context, 'md'), color: colors.muted),
          Expanded(
            child: TextField(
              autocorrect: false,
              style: Theme.of(context).textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14)),
              decoration: InputDecoration(
                isDense: true,
                filled: false,
                hoverColor: Colors.transparent,
                hintText: placeholder,
                hintStyle: TextStyle(color: colors.muted),
                border: InputBorder.none,
                enabledBorder: InputBorder.none,
                focusedBorder: InputBorder.none,
                contentPadding: const EdgeInsets.symmetric(vertical: 8),
              ),
              onChanged: onChanged,
            ),
          ),
        ],
      ),
    );
  }
}

/// Original sheet `iconButton`: a 28px round glyph button, surface2 on hover
/// or press, 50% opacity while disabled.
class _SheetIconButton extends StatefulWidget {
  const _SheetIconButton({required this.child, required this.label, this.onPressed});
  final Widget child;
  final String label;
  final VoidCallback? onPressed;
  @override
  State<_SheetIconButton> createState() => _SheetIconButtonState();
}

class _SheetIconButtonState extends State<_SheetIconButton> {
  bool hovered = false, pressed = false;
  @override
  Widget build(BuildContext context) {
    final enabled = widget.onPressed != null;
    return Semantics(
      button: true,
      enabled: enabled,
      label: widget.label,
      child: MouseRegion(
        cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: enabled ? (_) => setState(() => pressed = true) : null,
          onTapCancel: () => setState(() => pressed = false),
          onTapUp: (_) => setState(() => pressed = false),
          onTap: widget.onPressed,
          child: Opacity(
            opacity: enabled ? 1 : .5,
            child: Container(
              width: 28,
              height: 28,
              alignment: Alignment.center,
              decoration: BoxDecoration(
                color: enabled && (hovered || pressed)
                    ? PaseoColors.of(context).surface2
                    : Colors.transparent,
                shape: BoxShape.circle,
              ),
              child: widget.child,
            ),
          ),
        ),
      ),
    );
  }
}

/// Original bordered surface1 `SurfaceCard` (8px radius).
BoxDecoration _surfaceCard(BuildContext context) => BoxDecoration(
  color: PaseoColors.of(context).surface1,
  border: Border.all(color: Theme.of(context).dividerColor),
  borderRadius: BorderRadius.circular(8),
);

class _ProviderSettingsSheet extends StatefulWidget {
  const _ProviderSettingsSheet({
    required this.client,
    required this.provider,
    required this.snapshot,
    required this.patch,
    required this.refresh,
    this.onConfigure,
  });
  final PaseoClient client;
  final String provider;
  final ValueListenable<ProviderSettingsSnapshot> snapshot;
  final Future<void> Function(JsonMap patch) patch;
  final Future<void> Function(List<String> providers) refresh;
  final void Function(JsonMap entry)? onConfigure;
  @override
  State<_ProviderSettingsSheet> createState() => _ProviderSettingsSheetState();
}

class _ProviderSettingsSheetState extends State<_ProviderSettingsSheet> {
  String query = '';
  String? deletingId;
  bool refreshing = false;
  ({String serverId, String provider, List<JsonMap> models})? _discoveredCache;

  Future<void> _refresh() async {
    if (refreshing) return;
    setState(() => refreshing = true);
    try {
      await widget.refresh([widget.provider]);
    } catch (e) {
      if (mounted) showError(context, e);
    } finally {
      if (mounted) setState(() => refreshing = false);
    }
  }

  Future<void> _delete(String modelId) async {
    setState(() => deletingId = modelId);
    try {
      await widget.patch({
        'providers': {
          widget.provider: {
            'additionalModels': _additionalModels(widget.snapshot.value.config, widget.provider)
                .where((model) => model['id'] != modelId)
                .toList(),
          },
        },
      });
      await widget.refresh([widget.provider]);
    } catch (e) {
      if (mounted) showError(context, e);
    } finally {
      if (mounted && deletingId == modelId) setState(() => deletingId = null);
    }
  }

  void _openAdd() => showAdaptiveSheet<void>(
    context: context,
    maxWidth: 420,
    builder: (context) => _AddCustomModelSheet(
      provider: widget.provider,
      snapshot: widget.snapshot,
      patch: widget.patch,
      refresh: widget.refresh,
    ),
  );

  void _openDiagnostic() => showAdaptiveSheet<void>(
    context: context,
    builder: (context) => _DiagnosticSheet(client: widget.client, provider: widget.provider),
  );

  @override
  Widget build(BuildContext context) => ValueListenableBuilder<ProviderSettingsSnapshot>(
    valueListenable: widget.snapshot,
    builder: (context, snapshot, _) {
      final entry = snapshot.providers
              .where((candidate) => candidate['provider'] == widget.provider)
              .firstOrNull ??
          const <String, dynamic>{};
      final additional = _additionalModels(snapshot.config, widget.provider);
      final status = stringValue(entry['status']);
      final snapshotRefreshing = status == 'loading';
      final resolved = resolveProviderDiscoveredModels(
        serverId: widget.client.host.id,
        provider: widget.provider,
        currentModels: jsonList(entry['models']).map(jsonMap).toList(),
        providerSnapshotRefreshing: snapshotRefreshing,
        previousCache: _discoveredCache,
      );
      _discoveredCache = resolved.cache;
      final discovered = resolved.models;
      // Original `providerErrorMessage` is tested for truthiness: an empty
      // error string shows no error state.
      final rawError = status == 'error'
          ? stringValue(entry['error'], tr(context, 'settings.providers.diagnostic.unknownError'))
          : '';
      final errorMessage = rawError.isEmpty ? null : rawError;
      final modelsRefreshing = refreshing || snapshotRefreshing;
      return SheetContents(
        title: stringValue(entry['label'], widget.provider),
        header: _SheetSearchRow(
          placeholder: tr(context, 'settings.providers.models.searchPlaceholder'),
          onChanged: (value) => setState(() => query = value),
        ),
        actions: [
          Expanded(child: _footer(context, stringValue(entry['fetchedAt']), entry, modelsRefreshing)),
        ],
        child: _body(
          context,
          discovered: discovered,
          additional: additional,
          snapshotRefreshing: snapshotRefreshing,
          errorMessage: errorMessage,
          modelsRefreshing: modelsRefreshing,
        ),
      );
    },
  );

  /// Original `renderProviderSheetFooter`: the muted 12px "Updated …" label
  /// beside secondary Add model / Diagnostic and primary Refresh sm buttons;
  /// compact layouts stack the label over full-width buttons (8px gaps).
  Widget _footer(BuildContext context, String fetchedAt, JsonMap entry, bool modelsRefreshing) {
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final compact = isCompactLayout(context);
    final time = fetchedAt.isEmpty ? null : settingsRelativeTime(context, fetchedAt, ago: true);
    final meta = Text(
      time == null ? '' : tr(context, 'settings.providers.models.updated', args: {'time': time}),
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: Theme.of(context).textTheme.bodySmall!.copyWith(
        color: colors.muted,
        fontSize: type.uiSize(12),
      ),
    );
    final buttons = [
      if (widget.onConfigure != null)
        PaseoButton(
          label: 'Configure',
          icon: LucideIcons.settings2300,
          size: PaseoControlSize.sm,
          expand: compact,
          onPressed: () => widget.onConfigure!(entry.isEmpty ? {'provider': widget.provider} : entry),
        ),
      PaseoButton(
        label: tr(context, 'settings.providers.models.addModel'),
        icon: LucideIcons.plus300,
        size: PaseoControlSize.sm,
        expand: compact,
        onPressed: _openAdd,
      ),
      PaseoButton(
        label: tr(context, 'settings.providers.diagnostic.button'),
        icon: LucideIcons.fileText300,
        size: PaseoControlSize.sm,
        expand: compact,
        onPressed: _openDiagnostic,
      ),
      PaseoButton(
        label: tr(
          context,
          modelsRefreshing
              ? 'settings.providers.diagnostic.refreshing'
              : 'settings.providers.diagnostic.refresh',
        ),
        icon: modelsRefreshing ? null : LucideIcons.rotateCw300,
        size: PaseoControlSize.sm,
        variant: PaseoButtonVariant.primary,
        expand: compact,
        onPressed: modelsRefreshing ? null : _refresh,
      ),
    ];
    if (compact) {
      return Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 8,
        children: [if (time != null) meta, ...buttons],
      );
    }
    return Row(spacing: 8, children: [Expanded(child: meta), ...buttons]);
  }

  /// Original `ProviderModalBody`.
  Widget _body(
    BuildContext context, {
    required List<JsonMap> discovered,
    required List<JsonMap> additional,
    required bool snapshotRefreshing,
    required String? errorMessage,
    required bool modelsRefreshing,
  }) {
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final muted = Theme.of(context).textTheme.bodyMedium!.copyWith(
      fontSize: type.uiSize(14),
      color: colors.muted,
    );
    Widget empty(List<Widget> children) => Padding(
      padding: const EdgeInsets.symmetric(vertical: 32),
      child: Column(mainAxisSize: MainAxisSize.min, spacing: 12, children: children),
    );
    final none = discovered.isEmpty && additional.isEmpty;
    if (none && snapshotRefreshing) {
      return empty([
        const SettingsSpinner(size: 20),
        Text(tr(context, 'settings.providers.models.loading'), style: muted),
      ]);
    }
    if (none && errorMessage != null) {
      return empty([
        Icon(LucideIcons.triangleAlert300, size: densityIcon(context, 'md'), color: colors.muted),
        Text(errorMessage, textAlign: TextAlign.center, style: muted),
        PaseoButton(
          label: tr(
            context,
            modelsRefreshing
                ? 'settings.providers.models.retrying'
                : 'settings.providers.models.retry',
          ),
          size: PaseoControlSize.sm,
          variant: PaseoButtonVariant.primary,
          onPressed: modelsRefreshing ? null : _refresh,
        ),
      ]);
    }
    final filteredDiscovered = _rankModels(
      discovered,
      query,
      (m) => [stringValue(m['label']), stringValue(m['id']), stringValue(m['description'])],
    );
    final filteredCustom = _rankModels(
      additional,
      query,
      (m) => [stringValue(m['label']), stringValue(m['id'])],
    );
    if (filteredDiscovered.isEmpty && filteredCustom.isEmpty && query.trim().isNotEmpty) {
      return empty([Text(tr(context, 'settings.providers.models.noSearchMatches'), style: muted)]);
    }
    if (none) {
      return empty([Text(tr(context, 'settings.providers.models.noneDetected'), style: muted)]);
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (filteredDiscovered.isNotEmpty)
          _ModelSection(
            title: tr(context, 'settings.providers.models.discovered'),
            rows: [for (final model in filteredDiscovered) _ModelRow(model: model)],
          ),
        if (filteredCustom.isNotEmpty)
          _ModelSection(
            title: tr(context, 'settings.providers.models.custom'),
            rows: [
              for (final model in filteredCustom)
                _ModelRow(
                  model: model,
                  deleting: deletingId == model['id'],
                  onDelete: () => _delete(stringValue(model['id'])),
                ),
            ],
          ),
      ],
    );
  }
}

/// Original sheet section: muted 12px title and count header (4px inset, 8px
/// above the card), 16px below; every model row carries a 1px top border.
class _ModelSection extends StatelessWidget {
  const _ModelSection({required this.title, required this.rows});
  final String title;
  final List<Widget> rows;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final style = theme.textTheme.bodySmall!.copyWith(
      color: PaseoColors.of(context).muted,
      fontSize: PaseoTypography.of(context).uiSize(12),
    );
    final rule = BorderSide(color: theme.dividerColor);
    return Padding(
      padding: const EdgeInsets.only(bottom: 16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.only(left: 4, bottom: 8),
            child: Row(
              spacing: 8,
              children: [
                Expanded(child: Text(title, style: style)),
                Text('${rows.length}', style: style),
              ],
            ),
          ),
          DecoratedBox(
            decoration: _surfaceCard(context),
            child: ClipRRect(
              borderRadius: BorderRadius.circular(7),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  for (final row in rows)
                    DecoratedBox(
                      decoration: BoxDecoration(border: Border(top: rule)),
                      child: row,
                    ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// Original `DiscoveredModelRow` / `CustomModelRow`: 8/16 padding, 12px gaps,
/// base label, muted mono id, a muted 12px description; custom rows end in
/// a destructive 28px round trash button.
class _ModelRow extends StatelessWidget {
  const _ModelRow({required this.model, this.deleting = false, this.onDelete});
  final JsonMap model;
  final bool deleting;
  final VoidCallback? onDelete;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final id = stringValue(model['id']);
    final description = stringValue(model['description']);
    final label = Text(
      stringValue(model['label'], id),
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: theme.textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14)),
    );
    final mono = Text(
      id,
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: type.codeStyle.copyWith(color: colors.muted),
    );
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
      child: Row(
        spacing: 12,
        children: onDelete == null
            ? [
                Flexible(child: label),
                Flexible(child: mono),
                if (description.isNotEmpty)
                  Expanded(
                    flex: 2,
                    child: Text(
                      description,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.bodySmall!.copyWith(
                        color: colors.muted,
                        fontSize: type.uiSize(12),
                      ),
                    ),
                  ),
              ]
            : [
                Expanded(
                  child: Row(
                    spacing: 12,
                    children: [Flexible(child: label), Flexible(child: mono)],
                  ),
                ),
                _SheetIconButton(
                  label: tr(context, 'settings.providers.models.removeModel', args: {'id': id}),
                  onPressed: deleting ? null : onDelete,
                  child: Icon(
                    LucideIcons.trash2300,
                    size: densityIcon(context, 'sm'),
                    color: theme.colorScheme.error,
                  ),
                ),
              ],
      ),
    );
  }
}

/// Original `AddCustomModelSubSheet` (420px): medium base "Model ID" label,
/// the form input, a 12px destructive error and secondary Cancel / primary
/// Add sm buttons, end-aligned, 12px apart.
class _AddCustomModelSheet extends StatefulWidget {
  const _AddCustomModelSheet({
    required this.provider,
    required this.snapshot,
    required this.patch,
    required this.refresh,
  });
  final String provider;
  final ValueListenable<ProviderSettingsSnapshot> snapshot;
  final Future<void> Function(JsonMap patch) patch;
  final Future<void> Function(List<String> providers) refresh;
  @override
  State<_AddCustomModelSheet> createState() => _AddCustomModelSheetState();
}

class _AddCustomModelSheetState extends State<_AddCustomModelSheet> {
  final controller = TextEditingController();
  String? error;
  bool saving = false;

  @override
  void dispose() {
    controller.dispose();
    super.dispose();
  }

  List<JsonMap> get _additional => _additionalModels(widget.snapshot.value.config, widget.provider);

  bool get _canAdd {
    final id = controller.text.trim();
    return id.isNotEmpty && !_additional.any((model) => model['id'] == id);
  }

  Future<void> _add() async {
    if (!_canAdd || saving) return;
    final id = controller.text.trim();
    setState(() {
      error = null;
      saving = true;
    });
    try {
      await widget.patch({
        'providers': {
          widget.provider: {
            'additionalModels': [
              ..._additional,
              {'id': id, 'label': id},
            ],
          },
        },
      });
      await widget.refresh([widget.provider]);
      if (mounted) Navigator.pop(context);
    } catch (e) {
      if (mounted) {
        final message = errorMessage(e);
        setState(
          () => error = message.isEmpty
              ? tr(context, 'settings.providers.models.failedToSave')
              : message,
        );
      }
    } finally {
      if (mounted) setState(() => saving = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final type = PaseoTypography.of(context);
    final base = theme.textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14));
    return SheetContents(
      title: tr(context, 'settings.providers.models.addCustomTitle'),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 12,
        children: [
          Text(
            tr(context, 'settings.providers.models.modelId'),
            style: base.copyWith(fontWeight: FontWeight.w500),
          ),
          TextField(
            controller: controller,
            autofocus: true,
            autocorrect: false,
            enabled: !saving,
            style: base,
            decoration: paseoInputDecoration(
              context,
              hintText: tr(context, 'settings.providers.models.modelIdPlaceholder'),
            ),
            onChanged: (_) => setState(() {}),
            onSubmitted: (_) => _add(),
          ),
          if (error != null)
            Text(
              error!,
              style: theme.textTheme.bodySmall!.copyWith(
                color: theme.colorScheme.error,
                fontSize: type.uiSize(12),
              ),
            ),
          Row(
            mainAxisAlignment: MainAxisAlignment.end,
            spacing: 8,
            children: [
              PaseoButton(
                label: tr(context, 'common.actions.cancel'),
                size: PaseoControlSize.sm,
                onPressed: saving ? null : () => Navigator.pop(context),
              ),
              PaseoButton(
                label: tr(
                  context,
                  saving ? 'settings.providers.models.adding' : 'settings.providers.models.add',
                ),
                size: PaseoControlSize.sm,
                variant: PaseoButtonVariant.primary,
                onPressed: !_canAdd || saving ? null : _add,
              ),
            ],
          ),
        ],
      ),
    );
  }
}

/// Original `DiagnosticSubSheet`: copy and refresh 28px header glyphs over a
/// bordered surface1 card ("Running diagnostic…" / none) or a 480px-tall
/// scrollable mono code surface (12/16 padding, 18px lines).
class _DiagnosticSheet extends StatefulWidget {
  const _DiagnosticSheet({required this.client, required this.provider});
  final PaseoClient client;
  final String provider;
  @override
  State<_DiagnosticSheet> createState() => _DiagnosticSheetState();
}

class _DiagnosticSheetState extends State<_DiagnosticSheet> {
  String? diagnostic;
  bool loading = false;

  @override
  void initState() {
    super.initState();
    _fetch();
  }

  Future<void> _fetch() async {
    setState(() => loading = true);
    try {
      final result = await settingsRequest(
        widget.client,
        'provider_diagnostic_request',
        {'provider': widget.provider},
        responseType: 'provider_diagnostic_response',
      );
      if (mounted) setState(() => diagnostic = stringValue(result['diagnostic']));
    } catch (e) {
      if (mounted) {
        final message = errorMessage(e);
        setState(
          () => diagnostic = message.isEmpty
              ? tr(context, 'settings.providers.diagnostic.failedToFetch')
              : message,
        );
      }
    } finally {
      if (mounted) setState(() => loading = false);
    }
  }

  Future<void> _copy() async {
    final text = diagnostic;
    if (text == null || text.isEmpty) return;
    try {
      await Clipboard.setData(ClipboardData(text: text));
      if (mounted) {
        showCopiedToast(context, tr(context, 'settings.providers.diagnostic.copyLabel'));
      }
    } catch (_) {
      if (mounted) showError(context, tr(context, 'settings.providers.diagnostic.copyFailed'));
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final muted = theme.textTheme.bodyMedium!.copyWith(
      fontSize: type.uiSize(14),
      color: colors.muted,
    );
    final icon = densityIcon(context, 'sm');
    final text = diagnostic;
    Widget message(List<Widget> children) => DecoratedBox(
      decoration: _surfaceCard(context),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Row(spacing: 8, children: children),
      ),
    );
    final Widget body;
    if (loading && text == null) {
      body = message([
        const SettingsSpinner(size: 16),
        Text(tr(context, 'settings.providers.diagnostic.running'), style: muted),
      ]);
    } else if (text != null && text.isNotEmpty) {
      body = Container(
        constraints: const BoxConstraints(maxHeight: 480),
        decoration: _surfaceCard(context),
        clipBehavior: Clip.antiAlias,
        child: SingleChildScrollView(
          child: SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
            child: SelectableText(
              text,
              style: type.codeStyle.copyWith(
                color: theme.colorScheme.onSurface,
                height: 18 / type.codeFontSize,
              ),
            ),
          ),
        ),
      );
    } else {
      body = message([Text(tr(context, 'settings.providers.diagnostic.none'), style: muted)]);
    }
    return SheetContents(
      title: tr(context, 'settings.providers.diagnostic.title'),
      headerActions: [
        Row(
          mainAxisSize: MainAxisSize.min,
          spacing: 4,
          children: [
            _SheetIconButton(
              label: tr(context, 'settings.providers.diagnostic.copyAccessibility'),
              onPressed: text == null || text.isEmpty ? null : _copy,
              child: Icon(LucideIcons.copy300, size: icon, color: colors.muted),
            ),
            _SheetIconButton(
              label: tr(
                context,
                loading
                    ? 'settings.providers.diagnostic.refreshingAccessibility'
                    : 'settings.providers.diagnostic.refreshAccessibility',
              ),
              onPressed: loading ? null : _fetch,
              child: loading
                  ? SettingsSpinner(size: icon)
                  : Icon(LucideIcons.rotateCw300, size: icon, color: colors.muted),
            ),
          ],
        ),
      ],
      child: body,
    );
  }
}
