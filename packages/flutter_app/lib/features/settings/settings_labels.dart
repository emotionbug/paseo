import 'package:flutter/widgets.dart';

import '../../ui/localization.dart';

String settingsLabel(BuildContext context, String label) {
  const keys = <String, String>{
    'Settings': 'settings.title',
    'Appearance & language': 'settings.sections.appearance',
    'Chat': 'settings.sections.chat',
    'Sidebar': 'settings.sections.sidebar',
    'Keyboard shortcuts': 'settings.sections.shortcuts',
    'Editor': 'settings.sections.editor',
    'Providers': 'settings.providers.title',
    'Agent profiles': 'settings.host.agentProfiles.sectionTitle',
    'Metadata generation': 'settings.metadataGeneration.title',
    'Language': 'settings.general.language.label',
    'Theme': 'settings.appearance.theme.title',
    'Provider': 'settings.host.agentProfiles.providerLabel',
    'Model': 'settings.host.agentProfiles.modelLabel',
    'Mode': 'settings.host.agentProfiles.modeLabel',
    'Thinking': 'settings.host.agentProfiles.thinkingLabel',
    'Name': 'settings.host.agentProfiles.nameLabel',
    'Save': 'common.actions.save',
    'Cancel': 'common.actions.cancel',
    'Close': 'common.actions.close',
    'Copy': 'common.actions.copy',
    'Schedules': 'sidebar.sections.schedules',
    'Refresh': 'usage.refresh',
    'system': 'settings.general.language.options.system',
    'ar': 'settings.general.language.options.ar',
    'en': 'settings.general.language.options.en',
    'es': 'settings.general.language.options.es',
    'fr': 'settings.general.language.options.fr',
    'ja': 'settings.general.language.options.ja',
    'ko': 'settings.general.language.options.ko',
    'pt-BR': 'settings.general.language.options.ptBR',
    'ru': 'settings.general.language.options.ru',
    'zh-CN': 'settings.general.language.options.zhCN',
  };
  final key = keys[label];
  if (key == null) return label;
  final translated = tr(context, key);
  return translated == key ? label : translated;
}
