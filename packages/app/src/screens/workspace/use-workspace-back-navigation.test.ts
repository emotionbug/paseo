/** @vitest-environment jsdom */
import { act, renderHook, cleanup } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { BackHandler, Platform } from "react-native";
import { usePanelStore } from "@/stores/panel-store";
import { useWorkspaceBackNavigation } from "./use-workspace-back-navigation";

const backPress = vi.hoisted(() => ({ listeners: [] as Array<() => boolean> }));
vi.mock("react-native", async (importOriginal) => {
  const original = await importOriginal<{ Platform: typeof Platform }>();
  return {
    ...original,
    Platform: { ...original.Platform, OS: "android" },
    BackHandler: {
      exitApp: vi.fn(),
      addEventListener: (_event: string, listener: () => boolean) => {
        backPress.listeners.push(listener);
        return {
          remove: () => {
            backPress.listeners = backPress.listeners.filter((entry) => entry !== listener);
          },
        };
      },
    },
  };
});

function pressBack() {
  let handled = false;
  act(() => {
    handled = backPress.listeners.toReversed().some((listener) => listener());
  });
  return handled;
}

beforeEach(() => {
  vi.clearAllMocks();
  Platform.OS = "android";
  usePanelStore.setState({ mobilePanel: { target: "agent", revision: 0 } });
});
afterEach(() => {
  cleanup();
  backPress.listeners = [];
});

describe("compact workspace Android Back", () => {
  it("returns from a selected session to the sidebar before exiting", () => {
    renderHook(() => useWorkspaceBackNavigation(true, true));
    act(() => {
      usePanelStore.getState().showMobileAgentList();
      usePanelStore.getState().showMobileAgent();
    });
    expect(pressBack()).toBe(true);
    expect(usePanelStore.getState().mobilePanel.target).toBe("agent-list");
    expect(BackHandler.exitApp).not.toHaveBeenCalled();
    expect(pressBack()).toBe(true);
    expect(BackHandler.exitApp).toHaveBeenCalledTimes(1);
  });

  it("dismisses the explorer before returning to the sidebar", () => {
    renderHook(() => useWorkspaceBackNavigation(true, true));
    act(() => {
      usePanelStore.getState().showMobileFileExplorer();
    });
    pressBack();
    expect(usePanelStore.getState().mobilePanel.target).toBe("agent");
    expect(BackHandler.exitApp).not.toHaveBeenCalled();
    pressBack();
    expect(usePanelStore.getState().mobilePanel.target).toBe("agent-list");
    expect(BackHandler.exitApp).not.toHaveBeenCalled();
  });

  it("leaves a later overlay's dismissal ahead of workspace navigation", () => {
    renderHook(() => useWorkspaceBackNavigation(true, true));
    const overlay = BackHandler.addEventListener("hardwareBackPress", () => true);
    pressBack();
    expect(usePanelStore.getState().mobilePanel.target).toBe("agent");
    overlay.remove();
    pressBack();
    expect(usePanelStore.getState().mobilePanel.target).toBe("agent-list");
  });

  it("releases Back while another route is focused", () => {
    const { rerender } = renderHook(({ focused }) => useWorkspaceBackNavigation(focused, true), {
      initialProps: { focused: true },
    });
    rerender({ focused: false });
    expect(pressBack()).toBe(false);
    expect(usePanelStore.getState().mobilePanel.target).toBe("agent");
    expect(BackHandler.exitApp).not.toHaveBeenCalled();
    rerender({ focused: true });
    pressBack();
    expect(usePanelStore.getState().mobilePanel.target).toBe("agent-list");
  });

  it.each(["ios", "web"] as const)("does not intercept %s navigation", (platform) => {
    Platform.OS = platform;
    renderHook(() => useWorkspaceBackNavigation(true, true));
    expect(pressBack()).toBe(false);
    expect(BackHandler.exitApp).not.toHaveBeenCalled();
  });

  it("does not swallow Back in wide layouts", () => {
    renderHook(() => useWorkspaceBackNavigation(true, false));
    expect(pressBack()).toBe(false);
    expect(BackHandler.exitApp).not.toHaveBeenCalled();
  });
});
