# NeMo Relay native thin path

The `xai-grok-shell/nemo-relay` Cargo feature enables direct NeMo Relay lifecycle
instrumentation. It is off by default and does not alter Mixpanel or
`xai-grok-telemetry`.

## Confirmed call sites

The shell owns these boundaries:

| Relay lifecycle | Shell boundary |
| --- | --- |
| `Agent` session | `run_session` |
| child `Agent` subagent | `run_session`, when `parent_session_id` resolves to a live parent |
| `Function` turn | `process_conversation_turn_with_recovery` |
| managed tool | the `dispatch_tool` callback |
| managed LLM | `submit_and_collect_with_metadata` |

The resulting hierarchy is `Agent(session) -> Function(turn) -> managed
tool/managed LLM`. A subagent is a child `Agent`; if its live parent cannot be
resolved, the thin path emits no subagent scope rather than inventing a root.

## Privacy defaults

`nemo-relay-thin` registers fail-closed scope and mark sanitizers before its
subscriber. They remove `data`, `category_profile`, and `metadata`. Managed
tool arguments, tool results, LLM requests, and LLM responses are empty JSON
objects before sanitization. The subscriber accepts only these `(kind, name)`
pairs:

```text
(scope, grok.session)
(scope, grok.subagent)
(scope, grok.turn)
(scope, grok.tool)
(scope, grok.llm)
```

It does not emit a user identifier. Set `NEMO_RELAY_STDOUT=1` to print the
allowlisted pairs; otherwise the subscriber is silent.

## Verify

The default shell build does not activate the optional dependency:

```bash
cargo check -p xai-grok-shell
```

Compile the real call sites:

```bash
cargo check -p xai-grok-shell --features nemo-relay
```

Run the deterministic headless lifecycle:

```bash
NEMO_RELAY_STDOUT=1 cargo run -p nemo-relay-thin --example headless
cargo test -p nemo-relay-thin
```

## Proposal, not yet confirmed

The first port intentionally does not configure ATOF, ATIF, or OpenTelemetry
exporters. Gauge should confirm whether the generic allowlisted tool and LLM
names provide enough event fidelity before exporter configuration is added.
