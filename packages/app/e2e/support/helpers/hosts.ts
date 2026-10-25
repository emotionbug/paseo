import { expect, type Page } from "@playwright/test";
import { identityForeground, type IdentityColorName } from "@/styles/identity-colors";
import { openSettings } from "./app";
import { buildSeededHost } from "./daemon-registry";
import { clickSettingsBackToWorkspace, openHostSection, selectSettingsHost } from "./settings";

const REGISTRY_KEY = "@paseo:daemon-registry";
const SEED_NONCE_KEY = "@paseo:e2e-seed-nonce";
const DISABLE_DEFAULT_SEED_ONCE_KEY = "@paseo:e2e-disable-default-seed-once";

// The multi-host UI (the command-center host label, the sidebar host filter) only renders once
// more than one host exists. The e2e harness runs a single real daemon, so we add an extra registry
// entry pointing at an unreachable endpoint: it stays offline, which is enough to make the UI treat
// the view as multi-host without standing up a second daemon.
//
// Must run AFTER the first navigation: the auto-seed fixture writes the registry + nonce on load,
// and reseeds on every navigation. We write the full registry here and set the fixture's
// disable-once flag, then reload — so the fixture skips its reset and the registry survives. This
// avoids depending on the (unspecified) ordering of multiple Playwright init scripts. Optionally
// relabels the seeded primary host so assertions can target a distinctive name.
export async function addOfflineHostAndReload(
  page: Page,
  input: { serverId: string; label: string; primaryLabel?: string },
): Promise<void> {
  const offlineHost = buildSeededHost({
    serverId: input.serverId,
    label: input.label,
    endpoint: "127.0.0.1:59999",
    nowIso: new Date().toISOString(),
  });

  await page.evaluate(
    ({ host, keys, primaryLabel }) => {
      const nonce = localStorage.getItem(keys.nonce);
      if (!nonce) {
        throw new Error("Expected the e2e seed nonce before overriding the host registry.");
      }
      const raw = localStorage.getItem(keys.registry);
      const registry: Array<{ serverId: string; label?: string }> = raw ? JSON.parse(raw) : [];
      if (primaryLabel && registry[0]) {
        registry[0].label = primaryLabel;
      }
      if (!registry.some((entry) => entry.serverId === host.serverId)) {
        registry.push(host);
      }
      localStorage.setItem(keys.registry, JSON.stringify(registry));
      localStorage.setItem(keys.disableSeedOnce, nonce);
    },
    {
      host: offlineHost,
      keys: {
        registry: REGISTRY_KEY,
        nonce: SEED_NONCE_KEY,
        disableSeedOnce: DISABLE_DEFAULT_SEED_ONCE_KEY,
      },
      primaryLabel: input.primaryLabel,
    },
  );

  await page.reload();
}

export async function addConnectedHostAndReload(
  page: Page,
  input: { serverId: string; label: string; port: number; primaryLabel?: string },
): Promise<void> {
  await addConnectedHostsAndReload(page, [input], { primaryLabel: input.primaryLabel });
}

export async function addConnectedHostsAndReload(
  page: Page,
  inputs: Array<{ serverId: string; label: string; port: number }>,
  options?: { primaryLabel?: string },
): Promise<void> {
  const connectedHosts = inputs.map((input) =>
    buildSeededHost({
      serverId: input.serverId,
      label: input.label,
      endpoint: `127.0.0.1:${input.port}`,
      nowIso: new Date().toISOString(),
    }),
  );

  await page.evaluate(
    ({ hosts, keys, primaryLabel }) => {
      const nonce = localStorage.getItem(keys.nonce);
      if (!nonce) {
        throw new Error("Expected the e2e seed nonce before overriding the host registry.");
      }
      const raw = localStorage.getItem(keys.registry);
      const registry: Array<{ serverId: string; label?: string }> = raw ? JSON.parse(raw) : [];
      if (primaryLabel && registry[0]) registry[0].label = primaryLabel;
      for (const host of hosts) {
        if (!registry.some((entry) => entry.serverId === host.serverId)) {
          registry.push(host);
        }
      }
      localStorage.setItem(keys.registry, JSON.stringify(registry));
      localStorage.setItem(keys.disableSeedOnce, nonce);
    },
    {
      hosts: connectedHosts,
      keys: {
        registry: REGISTRY_KEY,
        nonce: SEED_NONCE_KEY,
        disableSeedOnce: DISABLE_DEFAULT_SEED_ONCE_KEY,
      },
      primaryLabel: options?.primaryLabel,
    },
  );

  await page.reload();
}

export async function waitForConnectedHost(
  page: Page,
  input: { serverId: string; endpoint: string },
): Promise<void> {
  await page.getByTestId("sidebar-hosts-trigger").click();
  const host = page.getByTestId(`sidebar-host-row-${input.serverId}`);
  await expect(host).toContainText(input.endpoint, { timeout: 30_000 });
  await page.keyboard.press("Escape");
  await expect(host).not.toBeVisible();
}

export async function openSidebarDisplayPreferences(page: Page): Promise<void> {
  await page.getByTestId("sidebar-display-preferences-menu").click();
  await expect(page.getByTestId("sidebar-display-preferences-content")).toBeVisible({
    timeout: 10_000,
  });
}

// Host filters live a page below the display-preferences root, so the filter rows only exist
// after walking into that branch.
export async function openSidebarHostFilter(page: Page): Promise<void> {
  await openSidebarDisplayPreferences(page);
  await page.getByTestId("sidebar-display-host-filter").click();
}

// A host's filter row carries a status dot on the left next to its label.
export async function expectHostFilterRow(page: Page, serverId: string): Promise<void> {
  await expect(page.getByTestId(`sidebar-host-filter-${serverId}`)).toBeVisible();
  await expect(page.getByTestId(`sidebar-host-filter-status-${serverId}`)).toBeVisible();
}

export async function toggleHostFilter(page: Page, serverId: string): Promise<void> {
  await page.getByTestId(`sidebar-host-filter-${serverId}`).click();
}

export async function selectAllHostsFilter(page: Page): Promise<void> {
  await page.getByTestId("sidebar-host-filter-all").click();
}

export async function openHostAppearanceSettings(page: Page, serverId: string): Promise<void> {
  await openSettings(page);
  await selectSettingsHost(page, serverId);
  await openHostSection(page, serverId, "host");
  await expect(page.getByTestId("host-appearance-preview")).toBeVisible({ timeout: 15_000 });
}

// Settings replaces session navigation, so host-row assertions need the app shell back. Going
// through the in-app back control (rather than a fresh goto) also keeps the auto-seed fixture
// from rewriting the host registry, since init scripts only re-run on a real navigation.
export async function leaveHostAppearanceSettings(page: Page): Promise<void> {
  await clickSettingsBackToWorkspace(page);
  await expect(page.getByTestId("host-appearance-preview")).toHaveCount(0);
}

export async function renameHostFromSettings(page: Page, name: string): Promise<void> {
  await page.getByTestId("host-page-label-edit-button").click();
  const input = page.getByTestId("host-page-rename-modal-input");
  await expect(input).toBeVisible();
  await input.fill(name);
  await page.getByTestId("host-page-rename-modal-submit").click();
  await expect(input).toHaveCount(0);
}

export async function chooseHostColor(page: Page, colorName: string): Promise<void> {
  await page.getByRole("button", { name: /^Color, / }).click();
  await page.getByRole("menuitem", { name: colorName, exact: true }).click();
  await expect(page.getByRole("button", { name: `Color, ${colorName}` })).toBeVisible();
}

export async function chooseHostBadgeDisplay(
  page: Page,
  option: "Name" | "Icon only" | "Hidden",
): Promise<void> {
  await page.getByTestId("host-appearance-badge-display").click();
  await page.getByRole("menuitem", { name: option, exact: true }).click();
  await expect(page.getByRole("button", { name: `Sidebar badge, ${option}` })).toBeVisible();
}

export async function expectHostAppearancePreview(
  page: Page,
  input: { serverId: string; hostName: string; color: IdentityColorName },
): Promise<void> {
  const preview = page.getByTestId("host-appearance-preview");
  await expect(preview).toBeVisible();
  const badge = preview.getByTestId(`host-badge-${input.serverId}`);
  await expect(badge).toHaveText(input.hostName);
  await expect(badge.locator("svg")).toHaveAttribute(
    "stroke",
    identityForeground(input.color, "light"),
  );
}

// The auto-seed fixture rewrites the host registry on every navigation. Setting the
// disable-once flag to the current nonce makes it skip exactly one reload, which is what proves
// the appearance survived rather than being reseeded.
export async function reloadPreservingHostRegistry(page: Page): Promise<void> {
  await page.evaluate(
    (keys) => {
      const nonce = localStorage.getItem(keys.nonce);
      if (!nonce) {
        throw new Error("Expected the e2e seed nonce before reloading.");
      }
      localStorage.setItem(keys.disableSeedOnce, nonce);
    },
    { nonce: SEED_NONCE_KEY, disableSeedOnce: DISABLE_DEFAULT_SEED_ONCE_KEY },
  );
  await page.reload();
}
