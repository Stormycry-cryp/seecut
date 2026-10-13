# Agent Flow and local components

The native host owns the active document, graph revision, selection epoch and account scope. `context` returns those bindings. `flow_read` provides bounded schema, graph, model, run, registered-result and original-operation queries. `flow_write` uses the existing native graph/execution commands for edits, run-to/run-node, explicit adoption and pause.

The managed service validates an explicit editor-tool vocabulary on both requests and returned tool calls, including the canvas and Flow methods. The service must be updated alongside a client that advertises these tools; accepting their names grants no host permission.

Each write requires the current native edit lease and approval of the exact frozen arguments. The runtime allocates the operation identity. A scheduling receipt is not a completed generation: query the run and its registered results. If an IPC response is lost, query the original invocation/operation; do not allocate another write or treat an empty recovery result as success. Completed receipts remain bound to the original document, client and account.

Conversation attachments store immutable global-library asset versions or frozen Flow node context. A new send validates them again. Editing a historical message creates a new input; historical requests and billing identities remain intact. Result cards survive the final text event and reopen through the existing global-library handoff. Image previews are sent only when the selected public model capability accepts images; text-only models retain stable asset references without receiving or inferring image pixels. Rejected content is reported as an input problem rather than a disconnected runtime.

## Local macOS bundle

The normal desktop entry locates `Contents/Resources/agent/components.json`. The complete local component input contains fixed Node and MCP executables, the matching private runtime, its production dependencies, build identity and third-party notices. No credentials belong in this directory.

`SEECUT_AGENT_BUNDLE=/absolute/sealed/bundle scripts/make-app.sh` is an optional local packaging input. `scripts/agent-bundle-manifest.py seal` creates its bounded file manifest; `verify` rejects missing, changed, extra or linked files. Packaging copies the verified input, rewrites native dependencies, signs inner executables, regenerates the manifest from their final bytes, then signs and verifies the outer app.

The private runtime and assembled local application are not public repository or CI artifacts. Omitting the optional input retains public-only packaging. This local prerequisite does not claim cross-platform release signing, notarization, update delivery, or verification of a live paid provider.
