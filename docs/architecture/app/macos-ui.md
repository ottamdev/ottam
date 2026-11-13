---
kind: architecture
status: active
---

# macOS Native UI

The Swift app is Ottam's presentation shell: windows and scenes, commands and
menus, Settings, navigation, notifications, transient per-window state, and
rendering of state Core supplies. `@State`, `@SceneStorage` and `@AppStorage`
hold UI state only; durable domain authority stays behind the Core protocol
(`AgentConnection`, one per app, shared by every window).

## Shell Rules

- One Workspace is one AppKit-owned window with SwiftUI column content
  ([Workspace Shell](../../specs/app/workspace-shell.md)). Chat is the
  workspace window's only page; Review, Terminal, Plan, file and Subagent views
  are tabs in its tools pane.
- Settings is a separate app window for app and machine plumbing (harnesses,
  auth, named agents, terminal and machine-local preferences). It is never a
  workspace page. Its `Search Settings` field is the only search; there is no
  workspace search until a concrete use case exists.
- AppKit interop only where SwiftUI cannot provide the desktop behavior.
  Justify any custom control against the system one it replaces.
- Mirror stable user actions through focused commands and menus before adding
  toolbar-only controls. Keep debug and rebuild actions out of product menus.
- Share status vocabulary (badges, progress, warnings) across surfaces. The
  one exception is a running turn's trailer, which shows the thread mark
  (`ThreadSpinner`) as the transcript's working state; controls and loading
  states keep the system `ProgressView`.
- While Core is unavailable the workspace chooser swaps its content, at the
  same size, for startup, reconnecting or failure content; no card layers over
  it. A failure in an open workspace keeps the workspace and shows
  `CoreRecoverySheet`.
- Window frames (`WindowPlacement`) save per app identity, worktree builds
  included, and register as native autosave names only after the restored frame
  is fitted to a display; there are no per-monitor layouts. UI-test and capture
  launches skip restoration (`isEnabledForCurrentLaunch`).

## Sidebar And Toolbar

- The sidebar (`SidebarView`, `OttamWorkspaceLogic/SidebarSections.swift`) is
  one merged `List`. A selected, waiting or working row always shows regardless
  of a section's visible cap. Section collapse restores with the window; "Show
  more" and parent folds do not persist.
- Delegated children are indented rows under their parent, not a
  `DisclosureGroup`: the system triangle sits only at the leading edge and the
  parent's fold chevron must trail its title.
- Rows stay a `Label` with an empty icon so the selected title bolds. A sidebar
  `List` row cannot be shorter than the Sidebar icon size's height; the
  row-inset and row-height modifiers do not lower it.
- The agents pill is an `NSMenuToolbarItem` (AX "Agents") beside the title,
  shown only while the selected conversation has a parent or children. Names
  live in its tooltip and AX label, not in the pill.

## Surfaces And Materials

Semantic content surfaces for static operational content; Liquid Glass only
for floating controls over rich content. Sidebar, toolbar and Settings keep
platform-default materials; code, diff and editor surfaces stay opaque enough
to read. Repeated cards and status tokens use shared semantic styles.

### Composer Panels

The composer and the panels stacked above it (question, plan, command) are
floating controls, so they are glass through `ComposerPanelChrome`:
`.glassEffect(.regular)`, no opaque fill, one `GlassEffectContainer` whose
spacing equals the card gap so cards stay apart at rest.

- A panel's glass materializes in place (`.glassEffectTransition(.materialize)`).
  Rejected: a `.matchedGeometry` morph out of the composer, which drags the
  panel's content over the composer while the glass grows.
- Arrival and departure run in an explicit `withAnimation`; a value-keyed
  `.animation` on the stack never reaches them, which is why the thread keeps
  its own copy of the shown question.
- Panel fields are plain SwiftUI fields with a quaternary fill, no focus ring.
  Rejected: the system rounded-border field, translucent AppKit chrome that
  flashes black when animated over glass.
- A panel is a focus section: number keys and ⌘↩ act only while it has focus,
  so a composer draft is never answered by accident. ⌘↩ is a key handler, not
  a button shortcut (which fires from anywhere in the window). An arriving
  panel takes focus only when no text view holds it.
- Single-select rows expose a radio group via `accessibilityRepresentation`;
  multi-select rows expose checkboxes. The tool row keeps the record (answer,
  Skipped, Not answered).
- The question panel's draft, page and collapse state live in
  `ChatPendingQuestion`, per conversation, and survive switching away.

### Quote Reply

- Transcript overlays (`QuoteReplyPresenter`) are AppKit subviews of the
  transcript scroll view above the clip view, so they move in the same pass
  that scrolls the text. AppKit moves only the hosting view and passes clicks
  outside the surface through; SwiftUI draws the glass because
  `NSGlassEffectView` has no materialize.
- Every re-show is a fresh surface (`.id(generation)`); a dismissed card never
  morphs back. Content changes fade around the resize, never during it.
- Never give this glass a `glassEffectID` or `glassEffectUnion`: on macOS 26
  either one inside a `GlassEffectContainer` around focusable controls makes
  the key-view walk loop forever when a field gains focus.
- Reply adds, never sends: replies wait above the composer as removable chips
  (`ChatQuoteCommentList`) and the next message carries them as Markdown block
  quotes ahead of the draft (`OttamPresentationLogic/QuoteReply.swift`). User
  bubbles render inline Markdown; block quotes are the one block form they keep.

## Agent Avatars

A named agent or delegated child shows a shape tile (`AgentAvatarView`,
`OttamWorkspaceLogic/AgentAvatar.swift`) seeded by a 32-bit value; a parent
keeps its product's brand tile; tiles never appear on sidebar rows.

- A named agent always stores `avatarSeed` (default: FNV-1a of the lowercased
  name), so a rename keeps the tile and Shuffle replaces it.
- A child stores no seed, only `delegated_named_agent_id`; Core sends the
  agent's current seed with the child so a Shuffle reaches every child. An
  ad-hoc child gets a random seed from Core.
- Rejected: pixel identicons (noise at 16–18 pt) and tetromino tiles (mirror
  pairs blur at 16 pt).

## Verification

Route-level behavior depends on the real window, toolbar, sidebar, safe areas
and Settings scenes, so verify it in the launched app: one focused registered
scenario (`./scripts/ui-test.sh run <scenario-id>`), or
`./scripts/smoke-app-bundle.sh` for packaging and page inventory, with paired
pixel and AX evidence under `.build/ui-tests/current`.

Harness limits the code does not show:

- Setting the question panel's Other field AX value directly does not update
  its SwiftUI binding; typed input does, so `chat.question-panel` picks an
  option instead.
- An AX audit walks the tree for about half a second and can miss a live
  transcript row during a fast stream; wait on the element. Never read "the
  run finished" from `chat.run-status` disappearing; wait on the composer's
  primary button carrying `chat.submit`.
