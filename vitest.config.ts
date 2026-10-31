import path from "node:path";
import { configDefaults, defineConfig } from "vitest/config";

// React's test APIs require its development build, even in a production shell.
process.env.NODE_ENV = "test";

export default defineConfig({
  resolve: {
    extensions: [
      ".web.mjs",
      ".web.js",
      ".web.mts",
      ".web.ts",
      ".web.jsx",
      ".web.tsx",
      ".mjs",
      ".js",
      ".mts",
      ".ts",
      ".jsx",
      ".tsx",
      ".json",
    ],
    alias: [
      {
        find: /^@getpaseo\/relay\/e2ee$/,
        replacement: path.resolve(__dirname, "packages/relay/src/e2ee.ts"),
      },
      {
        find: /^@getpaseo\/relay$/,
        replacement: path.resolve(__dirname, "packages/relay/src/index.ts"),
      },
      // The website's `~/` path alias, as in packages/website/tsconfig.json.
      { find: /^~\/(.*)$/, replacement: path.resolve(__dirname, "packages/website/src/$1") },
      { find: "react", replacement: path.resolve(__dirname, "node_modules/react") },
      { find: "react-dom", replacement: path.resolve(__dirname, "node_modules/react-dom") },
    ],
  },
  test: {
    // Fake-timer suites freeze p-throttle's clock, so a real per-second cap deadlocks them.
    env: {
      PASEO_GIT_MAX_PROCESSES_PER_SECOND: "10000",
    },
    exclude: [...configDefaults.exclude, "**/.claude/**", "**/.dev/**"],
    // The website installs its own React; bundling its router lets tests render website components
    // that use router hooks against the one React the tests use.
    server: { deps: { inline: [/packages\/website\/node_modules\/@tanstack\/(react-)?router/] } },
  },
});
