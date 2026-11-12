---
kind: spec
status: active
---

# Agent Efficiency: File Splits And Cleanup

## Current Status

This file holds the P1 file splits and P2 cleanup items of
[Agent Efficiency](agent-efficiency.md). That spec owns the findings, the
order, and the decisions. Nothing here has landed except part of P1-5 and
P2-7. Next: P1-4, the first item in that spec's [Order](agent-efficiency.md#order).

## Active

### P1: file splits (one PR each, no behavior change)

Move code into sibling files or extensions, and widen visibility only where the
compiler requires it. Verify each with the P0-5 ladder plus the full gate.
Re-derive split boundaries from the current `extension`/`impl` boundaries at
split time.

- [ ] **P1-1 `Workspace/ChatWorkspaceView.swift` → ~9 files.** Proposed pieces:
  shell (`ChatWorkbench`, `ChatCenterPane`, `ChatLayout`), composer, composer
  actions, empty landing, session selector, queued-messages button, permission
  card, loose thread, composer column. Move its `private extension JSONValue`
  and `ChatWorkspaceModel`'s into `ProtocolTypes+Extensions.swift`. The two
  share no members, so the move is for findability. Done when: no file is over
  600 lines and the build is green.
- [ ] **P1-2 `Workspace/MacAgentTranscriptView.swift` → ~9 files.** Pieces:
  `TranscriptScrollViews`, the representable, the controller core, `+Debug`
  (the `debug*` funcs, test-only, so do it first), `+Layout`, `+DataSource`,
  `+Selection`, `+Streaming`, `TranscriptCardCollectionItem`. Done when: the
  largest piece is under 800 lines, `test-app.sh --filter
  NativeTranscriptRenderer` is green, and the `chat.transcript-*` scenarios
  pass.
- [ ] **P1-3 `Workspace/ChatWorkspaceModel.swift` → core + extensions.** Split
  into `ChatRunState.swift` (value types), the core, `+EngineSelection`,
  `+Composer`, `+Recovery`, `+Submit`, `+Queue`, `+Events` (its method-name
  switch becomes P1-14's router), and `+Loading`. Separating the
  `resolvePermissions` overloads is part of the win. Do P1-9 first so the split
  does not carry the forwarders. Done when: no file is over 500 lines, and the
  build and `test-app.sh` are green.
- [ ] **P1-4 Inline Rust `mod tests` → sibling `tests.rs`.** Files:
  `agent/{registry, model_catalog, auth, adapter_install, node_resolver}.rs`,
  `agent/acp/session_config.rs`, `server/chat/{transcript, wire}.rs`,
  `server/agents/task_index.rs`, `net/mod.rs`, `diff.rs`. Follow the pattern
  `acp/tests.rs` and `server/agents/tests.rs` already use. `registry.rs`,
  `model_catalog.rs`, and `node_resolver.rs` have inner `#[cfg(test)]` items,
  so they are not clean tail cuts. Do transcript, registry, model_catalog, and
  auth in one PR and the rest in a follow-up. Done when: each production half
  is under 1,000 lines, except registry and model_catalog (P1-12), and the
  library test count is unchanged. Verify with:
  `cargo check --workspace --all-targets`; `./scripts/test-rust-core.sh --lib`.
- [~] **P1-5 `server/agents/conversation_runner.rs`.** `autonomous.rs`,
  `delegation.rs`, and `attribution.rs` are already extracted. Remaining step 1
  (its own PR): extract named helpers from `steer_conversation_run` and
  `forward_one`. Step 2: `runner/{forwarding, types, launch, prompt,
  completion, session}.rs`. Read the run-ordering invariants in
  `docs/architecture/backend/` first. Done when: no file is over 600 lines, no
  function is over 150 lines, and the full `test-rust-core.sh` is green.
- [ ] **P1-6 `OttamBackendClient/AgentConnection.swift` forwarders.**
  `OttamCoreProcessClient` and `public extension AgentConnection` repeat every
  RPC name. Preferred: delete the forwarder class together with P1-9. This is
  a public-API decision, open in the
  [Decisions](agent-efficiency.md#decisions). Fallback: split the extension
  into `+Workspace`, `+Agents`, `+Conversations`, `+Terminals`. Done when: each
  RPC name appears once per file.
- [ ] **P1-7 `server/handlers.rs` → `handlers/{project, runtime, conversation,
  terminal}.rs`.** One `impl AgentServer` holds four handler clusters that do
  not interleave. The pain is edit anchoring. The dispatch that reaches these
  handlers is P1-13.
- [ ] **P1-8 Consolidate JSONL test waiters into `tests/support/mod.rs`.**
  `drain_install_events` (×3), `wait_for_run_status` (×2, with hardcoded
  request ids), `collect_terminal_output`, `engine`, `phases`, and
  near-identical poll loops are each duplicated. Keep one monotonic-deadline
  implementation of each, passing only the remaining budget to each wait. Done
  when: the duplicates are gone and the JSONL tests are green.
- [ ] **P1-9 Extract the backend facade from `GitWorkingTree/DiffReviewModel.swift`.**
  About 80 call sites take `backend: DiffReviewModel`. Extract a
  `BackendFacade` protocol and implementation, inject it once per model, and
  leave diff state in `DiffReviewModel`. Do it before P1-3. Two separate PRs
  follow it:
  - Rename `Workspace/TerminalWorkspaceView.swift` (it declares
    `TerminalSessionModel`).
  - Dedupe `ByteCappedDataBuffer` across `OttamGhostty` and `OttamApp`, which
    is an API-surface decision.

  Done when: `rg 'backend: DiffReviewModel' app/Sources` returns nothing, the
  build is green, and `chat.queue-drain` and `chat.tools-pane` pass.
- [ ] **P1-10 `ui-testing/…/ScenarioExecution.swift` `run(_:)`.** One function
  covers almost the whole file. Extract per-phase helpers: setup, drive,
  capture, manifest. Done when: no function in the runner target is over 150
  lines.
- [ ] **P1-11 Split `TranscriptCoordinator` (`server/chat/transcript/live.rs`)
  after P1-4.** It is one large `impl`. `record_update`, `checkpoint`,
  `finish`, `enforce_bounds`, and `write_item` are each over ~150 lines, and
  the lock order of its sync fields is only a comment. `AgentUpdate::` is
  matched in about 10 production files, and nothing checks exhaustiveness
  across `HarnessUpdateKind` ↔ `AgentUpdate`. Scope:
  - per-update-family pure classifiers (the `assembly.rs` pattern);
  - `transcript/writer.rs` for `checkpoint`/`write_item`;
  - a typed lock-order struct;
  - an exhaustive `map_update`;
  - distinct names for the four `fn apply`.

  Done when: no file in `server/chat/transcript/` is over 1,200 lines and no
  function is over 150 lines. Verify with: `test-rust-core.sh --lib --filter
  transcript`, `--test chat_storage`, `--test jsonl_protocol`, and the
  `chat.transcript-*` scenarios.
- [ ] **P1-12 Split `agent/registry.rs` and `agent/model_catalog.rs`.**
  `AgentRegistry` is a god object: definitions, instances, adapter lifecycle,
  runtime probing, home/plugin writing, installs, auth, and shutdown, with
  `fn probe` ×7. Its static definitions sit at the bottom, past the lifecycle
  code. `model_catalog.rs` persists `model-catalog.json` with no version field;
  its `Option` fields mean "written before this field existed, re-probe".
  Scope: `agent/registry/{definitions, lifecycle, runtime, homes}.rs` behind
  the existing facade; `agent/model_catalog/{entry, probe, persist}.rs` plus a
  `version` field and a `persistence.md` section. Done when: no file in either
  directory is over 800 lines, and `./scripts/inspect-core.sh call agent.list
  '{}' --spawn` output is unchanged. Verify with: `--filter registry`,
  `--filter model_catalog`, and `--test jsonl_protocol --filter agent`.
- [ ] **P1-13 One request-dispatch table; no function over 150 lines on the
  request path.** `server/mod.rs::handle_request` special-cases several methods
  with `if request.method == …` before its `match`.
  `server/agents/requests.rs::handle_agent_request` is one ~500-line function
  that nests `instance_requests.rs::handle_agent_instance_request`. Nothing
  enforces function length: there is no `clippy.toml` and no `[lints]` table.
  Scope:
  - a method → handler table;
  - per-method `handle_agent_*` functions (the `handlers.rs` shape);
  - helper extraction from `assembly.rs::apply`,
    `payloads/live_service.rs::submit`, `acp/actor.rs::run_acp_backend`, and
    `attribution.rs::apply`;
  - `clippy.toml` with `too-many-lines-threshold = 150` plus
    `#![warn(clippy::too_many_lines)]`;
  - `//!` docs naming the request path (`serve_stdio` → `handle_request` →
    handler → `ChatService` → `Database::run` → repositories) and the update
    chain (`decode_session_update` → `HarnessUpdateKind` → `map_update` →
    `forward_harness_update` → `forward_one` → `record_transcript_update` →
    `record_update` → `checkpoint`).

  Done when: `handle_request` reads as a table and clippy is clean with the
  lint. Verify with: `cargo clippy --workspace --all-targets -- -D warnings`
  and `--test jsonl_protocol`.
- [ ] **P1-14 Generated `ProtocolMethod` and one typed event router.** Seven
  app files hand-type protocol method strings. The generated Swift emits no
  method constants, so a renamed method fails only at runtime. Emit
  `ProtocolMethod` from `generate-protocol-artifacts`. Add
  `AgentEventHub.subscribe<Params>(_ method: ProtocolMethod, …)`, and move
  `ChatWorkspaceModel.subscribeToEventsIfNeeded`'s switch into
  `ConversationEventRouter` (with P1-3). Done when:
  `rg '"(conversation|agent|terminal|workspace|engine|storage)(\.[a-zA-Z]+)+"'
  app/Sources/OttamApp --glob '!*.generated.swift'` returns nothing. Verify
  with: `./scripts/check-protocol-alignment.sh`; `test-app.sh --filter
  ChatWorkspace`.
- [ ] **P1-15 Split `server/agents/tests.rs` and `agent/acp/tests.rs`.** These
  are the most-churned test files. A whole read is over the `Read` cap, and the
  fakes are interleaved with the tests. Extract the fakes first, then split
  into `server/agents/tests/{mod, fakes, permission, session_continuity,
  steering, lifecycle, subagents}.rs`, and `agent/acp/tests/` the same way.
  Done when: no test file is over 1,500 lines and test counts are unchanged.
  Verify with: `--lib --filter server::agents`; `--lib --filter acp`.

## Next

### P2: cleanup, discoverability, smaller splits

- [ ] **P2-2 `scripts/` hygiene.** There is no `scripts/README.md`. Add one
  that separates live gates from script self-tests. Add `scripts/lib/common.sh`
  (`OTTAM_ROOT`, `log/warn/die`, `make_scratch_dir`). Today repo-root
  resolution mixes `pwd -P` and `pwd`, which is a worktree/symlink bug.
- [ ] **P2-3 `scripts/test-filter.sh <pattern>…`.** Runs `test-app.sh --filter`
  once per pattern and prints one summary line each.
- [ ] **P2-4 Remaining large Rust production files.** Includes `terminal/pty.rs`
  and `agent/acp/actor.rs`. In `actor.rs`, `PromptTasks`, `ExtensionTasks`,
  and `AuthenticationTasks` are near-identical, and one generic would remove
  two of them. Also `server/agents/attribution.rs`, `server/chat/runs.rs`, and
  `ottam-protocol/src/fixtures.rs`. Split only when a task is already open in
  the file.
- [ ] **P2-5 Test-file splits on churn only.** `NativeTranscriptRendererTests.swift`
  becomes sub-suites. `tests/chat_storage.rs` becomes
  `tests/chat_storage/{support, conversations, run_admission, sessions,
  delivery, events_paging, recovery, attachments, ordinals}.rs`. Also
  `PresentationLogicTests.swift`. `OttamBackendClientTests/TestRunner.swift` is
  the only module not on Swift Testing; migrating it is a separate decision.
- [ ] **P2-6 Filename ↔ primary-type mismatches.** `GhosttyEmbedding.swift`
  (C-interop lifetimes, so split with care), `ChatPresentation.swift`,
  `Settings/AgentsSettingsView.swift`, `Workspace/TerminalIdentityModels.swift`,
  `OttamWorkspaceLogic/EngineRuntimeAvailability.swift`, and the duplicate
  basename `CoreRecoveryPresentation.swift` (in `App/` and
  `OttamPresentationLogic/`). Rename `ottam-protocol/src/core.rs` →
  `envelope.rs`. In `ui-testing/`, most files mismatch, including the synonym
  files `RunnerSupport`/`ScenarioRuntimeSupport`/`ScenarioRuntimeHelpers`/
  `ScenarioExecutionSupport`.
- [~] **P2-7 `Workspace/TranscriptTextItemView.swift`.** Split into measurement,
  artifact cache, link target, view, and collection item. It is the hot path
  for text-rendering changes. Promote it to P1 if a re-measure shows read
  pressure. `SidebarView.swift` is not worth a PR.
- [ ] **P2-9 Docs orphan check.** Add an orphan-reachability check to
  `check-docs.sh`. Consider validating backticked code paths.
- [ ] **P2-10 Toolchain preflight.** Scripts should assert that
  `xcode-select -p` points at Xcode and print the one-line fix
  (CommandLineTools causes `Invalid manifest`/`emit-module failed`). Today only
  `test-app.sh` and `check-protocol-alignment.sh` check, through
  `scripts/lib/xcode-developer-dir.sh`. When `ui-test.sh smoke`
  fails to capture a route, it should print a remediation.
- [ ] **P2-11 `ottam-test-acp/src/main.rs`.** Split up the long `fn main` and
  its `session/prompt` arm, and add a README for the behavior-driving env vars.
- [ ] **P2-12 Protocol crate cleanups.** `DEFERRED_TYPES = &[]` in
  `swift/overrides.rs` is dead. `assert_fixture_matches` in `tests/artifacts.rs`
  is a pass-through over near-identical asserts, and `overrides.rs` repeats one
  table-lookup body.
- [ ] **P2-13 Module READMEs and the render-pipeline map.** No target directory
  under `app/Sources` has a README; module contracts live only in
  `Package.swift` comments. Add a one-paragraph README per target. Add
  `docs/architecture/app/chat/transcript/pipeline.md`, listing the event →
  render hops in order: `AgentEventHub.publish` → `ChatWorkspaceModel` method
  switch → `ChatTranscriptStore.merge` → `StreamingCommitCoordinator` →
  `ChatWorkspaceView` → `MacAgentTranscriptView.updateNSViewController` →
  `ChatTranscriptPresentationCoordinator.publishPending` →
  `MacAgentTranscriptViewController.apply` → `TranscriptBlockProjector` →
  `TranscriptTextCollectionItem` / `ChatTranscriptCardStore.makeCard`. Fix the
  `markdown-renderer.md` diagram, which omits `AgentEventHub`, the switch, and
  the SwiftUI hop.
- [ ] **P2-14 Fold `Workspace/` into `Transcript/`, `Terminal/`, `Chat/`; move
  design tokens out.** `Workspace/` holds app-wide `AppTypography.swift`,
  `MarkdownEngineTheme`, the composer editor, `AgentSignInController`, the
  renderer files, and the terminal models. Do it after P1-2, P1-3, and P1-9.
  Done when: `Workspace/` has ≤ 15 files, `AppTypography` is under `App/` or
  `Design/`, and the `smoke-app-bundle.sh` page inventory is unchanged.
- [ ] **P2-15 Test and dev hooks out of production paths.** `UITestPreparation`
  is consulted from production views and controllers, and `DevReloadState` is
  stored on every `WorkspaceWindowSession`. Add a `TestHooks` protocol that
  `OttamAppRuntime` injects, with a no-op release implementation, and put
  `Dev/` and `TestingSupport/` under `#if DEBUG`. Done when:
  `rg -l UITestPreparation app/Sources --glob '!**/TestingSupport/**'` finds
  ≤ 2 files and registered scenarios still pass.
- [ ] **P2-16 One name per type across `ottam-core` and `ottam-protocol`.**
  - Delete the core `SteeringCapability`, which duplicates the protocol one.
  - Rename core `AgentRuntimeStatus` → `RuntimeProbeStatus`.
  - Use one ID type at the boundary (`AgentId` vs `AgentID`).
  - Map `ProjectSourceKind` once.
  - Drop the `as Wire…` aliases.
  - Resolve the two `wire.rs` files and `payloads.rs` beside `payload/`.
- [ ] **P2-17 Swift `OttamCore` target.** Despite its name it is not the Rust
  core: it holds hand-written diff types plus an image-intake helper. Fold the
  diff types into the generated Swift and rename or dissolve the target. Add
  one `PresentableError` in `OttamPresentationLogic` to replace the repeated
  `AgentConnectionError.rpcError` matching.
