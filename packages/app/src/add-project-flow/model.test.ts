import { describe, expect, it } from "vitest";
import {
  backAddProjectPage,
  backDirectoryBrowser,
  chooseAddProjectHost,
  currentAddProjectPage,
  moveAddProjectActiveIndex,
  moveAddProjectSelection,
  navigateDirectoryBrowser,
  openAddProjectFlow,
  openDirectoryBrowserPage,
  openDirectorySearchPage,
  openGithubLocationPage,
  openNewDirectoryNamePage,
  openNewDirectoryParentPage,
  setAddProjectActiveIndex,
  setAddProjectPageInput,
  setNewDirectoryName,
  type AddProjectHost,
} from "./model";
import {
  addProjectMethodEmptyText,
  buildAddProjectMethods,
  canConfirmDirectoryBrowser,
  buildCloneLocationOptions,
  directoryBrowserNames,
  joinDirectoryPath,
  buildManualGithubRepositoryChoices,
  ownsDirectoryBrowserListing,
} from "./options";

const HOST: AddProjectHost = {
  serverId: "host-1",
  label: "Local",
  canAddProject: true,
  canBrowse: true,
  canCloneGithubRepositories: true,
  canSearchGithubRepositories: true,
  canCreateDirectory: true,
};

describe("Add Project navigation", () => {
  it("skips a single connected host without adding it to history", () => {
    const state = openAddProjectFlow({ hosts: [HOST] });

    expect(currentAddProjectPage(state)).toEqual({
      kind: "method",
      hostId: "host-1",
      activeIndex: 0,
      error: null,
      isSubmitting: false,
    });
    expect(backAddProjectPage(state)).toBeNull();
  });

  it("restores page input and selection after Back", () => {
    const secondHost = { ...HOST, serverId: "host-2", label: "Remote" };
    let state = openAddProjectFlow({ hosts: [HOST, secondHost] });
    state = setAddProjectPageInput(state, "rem");
    state = setAddProjectActiveIndex(state, 1);
    state = chooseAddProjectHost(state, secondHost.serverId);
    state = openDirectorySearchPage(state, secondHost.serverId);

    state = backAddProjectPage(state) ?? state;
    state = backAddProjectPage(state) ?? state;

    expect(currentAddProjectPage(state)).toEqual({
      kind: "host",
      query: "rem",
      activeIndex: 1,
      error: null,
    });
  });

  it("wraps keyboard selection in both directions", () => {
    expect(moveAddProjectActiveIndex(2, 3, "next")).toBe(0);
    expect(moveAddProjectActiveIndex(0, 3, "previous")).toBe(2);
    expect(moveAddProjectSelection(0, [true, false, true], "next")).toBe(2);
  });

  it("restores a directory name after returning to and reselecting its parent", () => {
    let state = openAddProjectFlow({ hosts: [HOST] });
    state = openNewDirectoryParentPage(state, HOST.serverId);
    state = openNewDirectoryNamePage(state, HOST.serverId, "~/dev");
    state = setNewDirectoryName(state, "command-center");
    state = backAddProjectPage(state) ?? state;
    state = openNewDirectoryNamePage(state, HOST.serverId, "~/dev");

    expect(currentAddProjectPage(state)).toMatchObject({
      kind: "new-directory-name",
      parentPath: "~/dev",
      name: "command-center",
    });
  });

  it("restores the GitHub destination query and active parent when reopening a repository", () => {
    const repository = {
      id: "repo-1",
      nameWithOwner: "getpaseo/paseo",
      cloneUrl: "git@github.com:getpaseo/paseo.git",
      description: null,
      visibility: "public",
      updatedAt: null,
    };
    let state = openAddProjectFlow({ hosts: [HOST] });
    state = openGithubLocationPage(state, HOST.serverId, repository);
    state = setAddProjectPageInput(state, "~/dev");
    state = setAddProjectActiveIndex(state, 2);
    state = backAddProjectPage(state) ?? state;
    state = openGithubLocationPage(state, HOST.serverId, repository);

    expect(currentAddProjectPage(state)).toMatchObject({
      kind: "github-location",
      query: "~/dev",
      activeIndex: 2,
    });
  });

  it("unwinds directory browse history before leaving the page", () => {
    let state = openAddProjectFlow({ hosts: [HOST] });
    state = openDirectoryBrowserPage(state, HOST.serverId);
    state = navigateDirectoryBrowser(state, "~/alpha");
    state = navigateDirectoryBrowser(state, "~/alpha/beta");

    state = backDirectoryBrowser(state)!;
    expect(currentAddProjectPage(state)).toMatchObject({
      kind: "directory-browser",
      currentPath: "~/alpha",
      history: ["~"],
    });
    state = backDirectoryBrowser(state)!;
    expect(currentAddProjectPage(state)).toMatchObject({
      kind: "directory-browser",
      currentPath: "~",
      history: [],
    });
    expect(backDirectoryBrowser(state)).toBeNull();
    expect(backAddProjectPage(state)).not.toBeNull();
  });
});

describe("Add Project options", () => {
  it("hides every mutating method when the host lacks stable project identity", () => {
    const outdatedHost = { ...HOST, canAddProject: false };

    expect(buildAddProjectMethods(outdatedHost)).toEqual([]);
    expect(addProjectMethodEmptyText(outdatedHost)).toBe("Update the host to use Add Project.");
  });

  it("keeps host-upgrade methods discoverable while hiding local-only Browse", () => {
    expect(
      buildAddProjectMethods({
        ...HOST,
        canBrowse: false,
        canCloneGithubRepositories: false,
        canSearchGithubRepositories: false,
        canCreateDirectory: false,
      }),
    ).toEqual([
      {
        id: "directory-browser",
        label: "Browse directories",
        description: "Browse directories on Local",
      },
      {
        id: "directory-search",
        label: "Search for directory",
        description: "Find a directory on Local",
      },
      {
        id: "github",
        label: "Clone from GitHub",
        description: "Update this host to clone GitHub repositories",
        disabled: true,
      },
      {
        id: "new-directory",
        label: "New directory",
        description: "Update this host to create directories",
        disabled: true,
      },
    ]);
  });

  it("filters and deterministically sorts browser directories", () => {
    expect(
      directoryBrowserNames([
        { name: "zeta", kind: "directory" },
        { name: "README", kind: "file" },
        { name: "Alpha", kind: "directory" },
        { name: "alpha", kind: "directory" },
      ]),
    ).toEqual(["Alpha", "alpha", "zeta"]);
  });

  it("joins child names with remote server path semantics", () => {
    expect(joinDirectoryPath("/", "src")).toBe("/src");
    expect(joinDirectoryPath("~", "src")).toBe("~/src");
    expect(joinDirectoryPath("C:\\", "src")).toBe("C:\\src");
    expect(joinDirectoryPath("C:\\Users", "src")).toBe("C:\\Users\\src");
    expect(joinDirectoryPath("\\\\server\\share", "src")).toBe("\\\\server\\share\\src");
  });

  it("rejects stale browser responses and disconnected confirmation", () => {
    const listing = { hostId: "host-1", path: "~/current", names: ["child"] };
    expect(ownsDirectoryBrowserListing(listing, "host-1", "~/current")).toBe(true);
    expect(ownsDirectoryBrowserListing(listing, "host-2", "~/current")).toBe(false);
    expect(ownsDirectoryBrowserListing(listing, "host-1", "~/old")).toBe(false);
    expect(
      canConfirmDirectoryBrowser({
        ownsListing: true,
        connected: false,
        failed: false,
        submitting: false,
      }),
    ).toBe(false);
    expect(
      canConfirmDirectoryBrowser({
        ownsListing: true,
        connected: true,
        failed: true,
        submitting: false,
      }),
    ).toBe(false);
    expect(
      canConfirmDirectoryBrowser({
        ownsListing: true,
        connected: true,
        failed: false,
        submitting: false,
      }),
    ).toBe(true);
  });

  it("offers manual URL and protocol-specific owner/repo clone choices", () => {
    expect(buildManualGithubRepositoryChoices("git@github.com:getpaseo/paseo.git")).toEqual([
      expect.objectContaining({
        id: "manual:git@github.com:getpaseo/paseo.git",
        nameWithOwner: "getpaseo/paseo",
        cloneUrl: "git@github.com:getpaseo/paseo.git",
      }),
    ]);
    expect(buildManualGithubRepositoryChoices("getpaseo/paseo")).toEqual([
      expect.objectContaining({ cloneProtocol: "https", cloneUrl: "getpaseo/paseo" }),
      expect.objectContaining({ cloneProtocol: "ssh", cloneUrl: "getpaseo/paseo" }),
    ]);
    expect(buildManualGithubRepositoryChoices("paseo")).toEqual([]);
  });

  it("shows final clone paths while retaining parent paths as values", () => {
    expect(
      buildCloneLocationOptions({
        parents: ["~/dev", "~/workspace"],
        repositoryName: "paseo",
        existingPaths: ["~/workspace/paseo"],
      }),
    ).toEqual([
      {
        id: "~/dev",
        path: "~/dev",
        displayPath: "~/dev/paseo",
        secondaryText: "Parent directory: ~/dev",
        disabled: false,
      },
      {
        id: "~/workspace",
        path: "~/workspace",
        displayPath: "~/workspace/paseo",
        secondaryText: "Already exists",
        disabled: true,
      },
    ]);
  });

  it("shows equivalent absolute-home and tilde destinations only once", () => {
    expect(
      buildCloneLocationOptions({
        parents: ["/Users/moboudra/dev", "~/dev"],
        repositoryName: "dotfiles",
        existingPaths: [],
      }),
    ).toEqual([
      {
        id: "/Users/moboudra/dev",
        path: "/Users/moboudra/dev",
        displayPath: "/Users/moboudra/dev/dotfiles",
        secondaryText: "Parent directory: /Users/moboudra/dev",
        disabled: false,
      },
    ]);
  });
});
