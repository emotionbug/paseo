import { useEffect } from "react";
import { BackHandler, Platform } from "react-native";
import { usePanelStore } from "@/stores/panel-store";

export function useWorkspaceBackNavigation(isFocused: boolean, isCompact: boolean) {
  useEffect(() => {
    if (Platform.OS !== "android" || !isFocused || !isCompact) return;

    const subscription = BackHandler.addEventListener("hardwareBackPress", () => {
      // Read current intent so consecutive presses also work during panel motion.
      const panels = usePanelStore.getState();
      switch (panels.mobilePanel.target) {
        case "file-explorer":
          panels.showMobileAgent();
          break;
        case "agent":
          panels.showMobileAgentList();
          break;
        case "agent-list":
          // Do not pop a retained workspace route behind the sidebar.
          BackHandler.exitApp();
          break;
      }
      return true;
    });

    return () => subscription.remove();
  }, [isFocused, isCompact]);
}
