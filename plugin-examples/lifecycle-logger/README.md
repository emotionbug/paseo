# Lifecycle logger

Registers every server lifecycle hook (nine events and three before hooks) and logs their data as
JSON. It observes workspace creation and archive, agent creation, session opening, turns,
permissions, closing, and archive. Before hooks return the request unchanged. Environment values
are redacted; prompts and timeline content are logged.

Install it on a daemon where plugins are enabled:

```bash
paseo plugin install /absolute/path/to/plugin-examples/lifecycle-logger
paseo plugin logs lifecycle-logger
```

`paseo plugin logs` shows a recent in-memory tail of the plugin's output. Log lines are capped at
16 KiB, so a large timeline can be truncated. The output is not copied to the daemon log, and
removing the plugin or restarting the daemon clears it.

See the [lifecycle reference](../../public-docs/plugins/reference.md#lifecycle-hooks) for callback
shapes and delivery rules. Install it alongside [lifecycle-actions](../lifecycle-actions) on a test
daemon to watch those callbacks run.
