const PLUGIN_CLIENT_ID_PREFIX = "plugin:";

export function createPluginClientId(pluginId: string): string {
  return `${PLUGIN_CLIENT_ID_PREFIX}${pluginId}`;
}
