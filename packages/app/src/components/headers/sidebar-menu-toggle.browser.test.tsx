import React, { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { WindowSidebarMenuToggle } from "./sidebar-menu-toggle";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

vi.mock("./header-toggle-button", () => ({
  HeaderToggleButton: ({ children }: { children: ReactNode }) => (
    <button type="button">{children}</button>
  ),
  headerIconSlotStyle: { slot: {} },
}));

vi.mock("@/stores/panel-store", () => ({
  selectIsAgentListOpen: () => false,
  usePanelStore: (selector: (state: { toggleAgentListForLayout: () => void }) => unknown) =>
    selector({ toggleAgentListForLayout: vi.fn() }),
}));

describe("WindowSidebarMenuToggle", () => {
  let container: HTMLDivElement;
  let root: Root;
  let consoleError: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    consoleError.mockRestore();
  });

  test("consumes Unistyles icon props instead of forwarding them to the DOM", () => {
    act(() => root.render(<WindowSidebarMenuToggle />));

    expect(container.querySelector("[uniprops]")).toBeNull();
    expect(consoleError.mock.calls.flat().join(" ")).not.toContain("uniProps");
  });
});
