# Synthetic conversation broker

`conversation_broker.py` is an offline test peer, never a production runtime or
live model substitute. It asserts `codex` and `bwrap` are `/usr/bin/true` and only
accepts named fixture scenarios. It launches no subprocesses or model requests.

Rust HTTP integration tests use the `fixture-empty`, `fixture-matches`,
`fixture-fail`, `fixture-invalid-json`, `fixture-malformed`, `fixture-read-fail`
and `fixture-repeat` scenarios. Their synthetic user payload supplies a temporary
JSONL trace path, letting tests compare real broker invocations with durable
backend dispatch records. Fast process exit is intentional.

For the fixed `tools/ui-conversation-journey.mjs` script, create a private
conversation profile with this broker's absolute path, `model: "fixture-ui"`,
`codex: "/usr/bin/true"`, `bwrap: "/usr/bin/true"`, `effort: "medium"`,
`timezone: "Australia/Adelaide"`, `timeout_seconds: 5`,
`max_context_bytes: 65536` and `max_output_bytes: 16384`. Use the journey's
`--fake-model` flag and a distinct evidence directory. All prompts are matched
exactly against that one finite script; unknown prompts fail. The first draft
uses one successful empty lookup and one continuation. Later drafting,
refinement, preview, catalogue search, schedule revision, pause, resume, one-off
and unavailable-research proposals use closed operation objects.

The UI peer proves only the deterministic interface/runtime integration. It does
not qualify natural-language interpretation, provider availability or live model
acceptance. Do not use it for a live qualification record or a real database.
