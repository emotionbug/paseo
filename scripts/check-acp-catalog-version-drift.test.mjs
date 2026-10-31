import assert from "node:assert/strict";
import test from "node:test";
import { applyUpdates, parseCatalogEntries } from "./check-acp-catalog-version-drift.mjs";

test("parses Dart lists, nested metadata, comments and quoted argument bytes", () => {
  const source = String.raw`const acpProviderCatalog = <Map<String, dynamic>>[
    {
      'id': 'npm',
      "title": "Agent \"quoted\" ] }",
      'version': '1.0.0',
      // Delimiters inside comments are not structure: ] }
      'command': [
        'npx', '-y', '@scope/acp@1.0.0',
        '', 'spaces  stay ', "quote: \" and '",
        'apostrophe: \' and "', r'C:\tools\agent', r'$HOME',
        "line\n\t\r\b\f\v", "\x41\u0042\u{1F600}", "\$HOME",
        "literal ,] and }", // trailing comma
      ],
      'env': {'KEY': 'value',},
      'params': {'enabled': false, 'other': null, 'count': 2,},
    },
    /* Nested /* comment ] */ } */
    {'id': 'manual', 'title': 'Manual', 'version': 'manual', 'command': ['agent', 'acp',],},
  ];`;
  assert.deepEqual(parseCatalogEntries(source).map(({ start, end, ...entry }) => entry), [
    {
      id: "npm",
      title: 'Agent "quoted" ] }',
      version: "1.0.0",
      command: [
        "npx", "-y", "@scope/acp@1.0.0",
        "", "spaces  stay ", 'quote: " and \'',
        "apostrophe: ' and \"", "C:\\tools\\agent", "$HOME",
        "line\n\t\r\b\f\v", "AB\u{1F600}", "$HOME",
        "literal ,] and }",
      ],
    },
    { id: "manual", title: "Manual", version: "manual", command: ["agent", "acp"] },
  ]);
});

test("updates multiple native map entries without altering metadata or skipped entries", () => {
  const source = String.raw`const acpProviderCatalog = <Map<String, dynamic>>[
  {
    "id": "npm", "title": "Npm",
    "env": {"version": "untouched", "command": ["untouched"], "KEY": "value"},
    "version": "1.0.0",
    "command": [
      "npx", "-y", "@scope/acp@1.0.0", // keep the pin comment
      'literal ] }', r'C:\tools\agent', "\$HOME", "quote: \"", "",
    ],
  },
  {"id": "python", "title": "Python", "version": "2.0.0", "command": ["uvx", "--from", "acp==2.0.0", "acp",],},
  {"id": "manual", "title": "Manual", "version": "manual", "command": ["agent", "acp"]},
];
`;
  const entries = parseCatalogEntries(source);
  const results = entries.map((entry, index) => ({
    ...entry,
    checked: index < 2,
    latestVersion: index === 0 ? "10.2.3" : index === 1 ? "3.0.1" : null,
    updatedCommand: index === 0
      ? entry.command.map((arg, argIndex) => argIndex === 2 ? "@scope/acp@10.2.3" : arg)
      : index === 1 ? ["uvx", "--from", "acp==3.0.1", "acp"] : null,
  }));
  const updated = applyUpdates(source, results);
  assert.equal(updated, source
    .replace('"version": "1.0.0"', '"version": "10.2.3"')
    .replace("@scope/acp@1.0.0", "@scope/acp@10.2.3")
    .replace('"version": "2.0.0"', '"version": "3.0.1"')
    .replace("acp==2.0.0", "acp==3.0.1"));
  assert.deepEqual(parseCatalogEntries(updated).map((entry) => entry.command), [
    results[0].updatedCommand, results[1].updatedCommand, entries[2].command,
  ]);
  assert.equal(applyUpdates(source, []), source);
});

test("rejects a missing native catalog rather than silently checking zero entries", () => {
  assert.throws(() => parseCatalogEntries("const other = [];"), /Could not find acpProviderCatalog/);
});

test("serializes changed argument bytes as Dart strings rather than interpolation", () => {
  const source = `const acpProviderCatalog = <Map<String, dynamic>>[
    {'id': 'test', 'version': '1.0.0', 'command': ['npx', 'agent@1.0.0',],},
  ];`;
  const entry = parseCatalogEntries(source)[0];
  const updatedCommand = ["npx", "agent@2.0.0", "$HOME", 'quote " slash \\ newline\n'];
  const updated = applyUpdates(source, [{
    ...entry, checked: true, latestVersion: "2.0.0", updatedCommand,
  }]);
  const parsed = parseCatalogEntries(updated)[0];
  assert.equal(parsed.version, "2.0.0");
  assert.deepEqual(parsed.command, updatedCommand);
});

test("rejects expressions, interpolation and malformed commands without evaluating code", () => {
  for (const command of [
    `["npx", computeVersion()]`,
    `["npx", "$version"]`,
    `["npx", "\${version}"]`,
    `["npx", 42]`,
    `["npx",,]`,
    `["npx"`,
    String.raw`["npx", "\u{110000}"]`,
  ]) {
    const source = `const acpProviderCatalog = <Map<String, dynamic>>[
      {"id": "test", "command": ${command}},
    ];`;
    assert.throws(() => parseCatalogEntries(source));
  }
});
