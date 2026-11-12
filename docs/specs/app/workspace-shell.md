---
kind: spec
status: active
---

# AppKit Workspace Shell

AppKit owns each workspace window's shell: window, toolbar, and column split.
SwiftUI owns the content inside each column. The SwiftUI `App` keeps the
chooser, Settings, the Markdown lab, and the menu commands. This replaced a
SwiftUI shell whose `.inspector` crashed on macOS 27 and grew the window,
whose nested split could not divide the full-width toolbar, and whose hosted
toolbar items resized a frame late.

## Current Status

The shell and the Chat tools pane (Review, Terminal, Plan, file and Subagent
tabs) have shipped. Open: the unchecked spike results and the Add Tool
flicker below.

## Active

- **S2 restored launch.** The landing's "Set Up Agents" opens Settings on Agents through
  the main host's `openWindow`; unchecked after a restored launch with no
  chooser. Verify with: ⌥⌘Q with a workspace window, relaunch, use the
  command.
- **S3 materials.** Sidebar glass and rows match; the identity name renders
  without vibrancy, and the transcript scroll edge is unchecked. Verify with:
  real-window pixels against the SwiftUI baseline.
- **S6 animated show of a widened pane.** The snap path fits a pane widened
  past the room (`widenedToolsPaneFitsWindow`); the animated path is
  unchecked.
- **S7 real pointer.** The tab strip passes through AX presses; a real click
  and hover are unchecked, since every view in the strip reports
  `mouseDownCanMoveWindow`.
- **Add Tool flicker.** Inserting the bordered Add Tool item blanks New Chat
  and Tools, which fade back in over about 0.2 s; inserting the unbordered
  tabs alone does not. Next to try: Add Tool unbordered, its view a
  `.glass` `NSButton`.
- **S10 pane scroll edge (parked).** The toolbar gives main's band an opaque
  strip but gives the inspector's band nothing, so the Plan tab does not
  scroll under the tab band with the transcript's edge. No toolbar, safe-area,
  or separator setting changes it; a top-aligned
  `NSSplitViewItemAccessoryViewController` is the only edge source and
  misplaces it. Matching main needs a custom backing.

## Decisions

- **Window.** One `NSWindowController` per workspace, created by the window
  registry; tabbing disallowed. The 920×620 minimum: height is a required
  constraint on the split view, width is `windowWillResize(_:to:)`. A width
  constraint above priority 500 pins the split's fitting width and grows the
  window when the pane uncollapses; `contentMinSize` is ignored under Auto
  Layout.
- **Split.** A root `NSSplitViewController`: sidebar (min 240), main (min
  320), and an inspector item for the tools pane (min 280, default 320).
  Neither side has a maximum (`unspecifiedDimension`; an inspector's default
  270 maximum pins a 280 pane). Holding priorities: main 250 < sidebar 255 <
  inspector 260. Collapsibles use
  `.preferResizingSiblingsWithFixedSplitView`, and showing the pane never
  grows the window: the controller fits the pane to the room beside the
  sidebar and main's minimum, narrowing the sidebar first when needed. The
  pane cannot collapse itself (`canCollapse` off); the selected conversation
  owns its visibility. Toggling animates through the item's animator;
  conversation switches and Reduce Motion snap. A collapsed pane's view leaves
  the window, so an animated show with Review selected slides open over a
  loader and Review joins after the slide
  ([Review rendering](review-rendering.md)).
- **Persistence.** `splitView.autosaveName` owns the sidebar's width and
  collapsed state. The pane's visibility is reapplied from the conversation
  after autosave loads, and its width resets to 320 each launch. Nothing is
  written per resize tick, and programmatic collapses (a project error hiding
  the sidebar) never land in autosave.
- **Tools pane.** Visibility and tabs are per conversation, in memory for the
  window's lifetime. A draft has no tools until its first send, so Tools and
  ⌥⌘I are disabled on it. The pane owns its terminals: closing the window
  closes them; a replaced Core closes terminal tabs and keeps Review; a
  deleted conversation closes its terminals. Without tabs the pane shows a
  tool picker (an inset list, one row per tool kind); closing the last tab
  returns to it, and only Tools and ⌥⌘I hide the pane.
- **Subagent tab.** A thread card opens one reusable, read-only Subagent tab
  (not in the picker or `+`) that draws the thread with the main renderer
  under a breadcrumb ([subagent sessions](../backend/acp/subagent-sessions.md#decisions)).
- **Hosts.** One long-lived `NSHostingController` per column with
  `sizingOptions = []` and `sceneBridgingOptions = []`; `rootView` is set
  once. The sidebar `.sidebar` `List` sits in a `NavigationStack`, because
  outside one an inactive window keeps the old row's selected weight (macOS
  27). Remove the stack when Apple fixes it.
- **Toolbar.** One `NSToolbar` per window with an identifier unique to the
  window, because toolbars sharing an identifier share inserted and removed
  items. Each action is its own native item. The standard sidebar toggle sits
  after a leading flexible space, as in Mail and Finder. Tools sits with New
  Chat before the inspector separator; it is a custom item, not the standard
  `toggleInspector:` item, whose label and AX name say Inspector. The window
  controller revalidates items when their predicates change without an event
  (a first send creating its conversation). The title is the conversation
  title; the subtitle is `project · branch` (just the branch for a
  single-root Workspace, absent for Workspace files); the Window menu keeps
  `workspace — title`.
- **Commands.** `OttamWindowActions` is the single, identity-stable action
  holder. The registry exposes the main window's actions and clears them on
  resign-main. Menu `.disabled` and `NSToolbarItemValidation` read the same
  predicates. No `@FocusedValue`, no `SidebarCommands`.
- **Tab strip.** A custom SwiftUI strip as the view of an unbordered toolbar
  item after the inspector separator, host `sizingOptions = []`. The selected
  tab gets a weighted width under a sliding glass pill; others share the rest,
  collapse to icon and number, then scroll. Close is a bare ✕ that gains a
  circle under the pointer: always on the selected tab, on hover elsewhere
  only where it clears the label. Tabs have no context menu (AppKit takes
  toolbar right clicks). Add Tool (`+`) is a bordered `NSMenuToolbarItem`
  after the strip. Tabs and Add Tool leave the toolbar before the pane slides
  shut and join after it slides open, because a collapsed section keeps room
  for its items.
- **Tab behavior.** Rapid animated toggles settle on the latest state: a
  toggle during a slide waits for its end. A Ghostty view focuses on mount
  only when focus was requested; Open Terminal and tab clicks request it, and
  the one-shot request is cancelled by close, hide, or a conversation switch.
  Show Previous/Next Tab (⇧⌘[ / ⇧⌘]) cycle a shown pane's tabs, wrapping;
  focus follows unless a view outside the pane holds it. New Terminal (⌃\`)
  is also in the Workspace menu, and works over a focused terminal because
  the terminal view does not override `performKeyEquivalent`.
- **Closed windows.** The session's `isClosed` flag is set on close; async
  actions run through `runTask`, which cancels them, and each checks
  `!Task.isCancelled, !isClosed` after every await.
- **Safe area.** Main sets `automaticallyAdjustsSafeAreaInsets` so the main
  column runs under the sidebar glass.
- **Restoration.** `NSWindowRestoration` encodes project, draft root,
  conversation, and collapsed sidebar sections; the shell keeps no
  `@SceneStorage`. If `NSApplicationDidFinishRestoringWindows` fires with no
  workspace window, the registry opens the default window. Test and capture
  launches are not restorable. A first-time window cascades from the
  frontmost workspace window.

### SwiftUI and AppKit constraints found building the strip

- A horizontal `ScrollView` proposes no width; a custom `Layout` sizes the
  tabs from the width measured with `onGeometryChange`.
- A view swapped by `ViewThatFits` or `if` ghosts while animating; each tab is
  one view whose modifiers change.
- Model changes inside `withAnimation` also animate the scroll view's
  document width, so `scrollTo` lands short; tabs animate with
  `.animation(_:value:)` and only the scroll uses `withAnimation`.
- Tabs carry the selected trait inside a tab-group container so VoiceOver
  reads them as tabs.
- After an animated show, the inspector item's view loses the toolbar band
  from its safe area; a snapped show keeps it.

### Rejected

- Tabs as an `NSToolbarItemGroup` (no per-tab close; reports the wrong AX
  value) or a segmented control (no per-tab close; overflows at three tabs).
- The strip in a top-aligned split-item accessory (jumps into the band after
  an animated show).
- An AppKit `NSOutlineView` sidebar: the `NavigationStack` fixes the weight
  bug, and the rewrite would swap one native source list for another.
  Reconsider if the workaround breaks or the sidebar needs drag reordering.
- A UserDefaults open-window list to seed restoration: plan B only if native
  restoration regresses on a later beta.
