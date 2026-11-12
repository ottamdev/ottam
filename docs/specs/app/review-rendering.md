---
kind: spec
status: proposed
---

# Review Rendering

Review is the tools pane's diff tab ([workspace shell](workspace-shell.md)).
It draws every diff line as its own SwiftUI row, and the pane pays for those
rows each time its view joins the window.

## Current Status

Proposed; the host choice below is open. A stopgap ships: an animated show
with Review selected slides open over a loader, and Review joins once the
pane has slid open (`ToolsPaneViewController`). The rebuild costs the same;
it lands after the motion. The `chat.tools-pane` scenario captures Review
with a truncating snapshot policy.

## Problem

- A collapsed inspector item's view leaves the window. Showing the pane
  rebuilds the platform views, layout, and accessibility nodes inside it on
  the main thread, so the slide stalled at 70–118 pt for 200–600 ms, then
  snapped open. Sampling put most of the cost in the display cycle and
  Review's row updates; the stall also shows without the harness's AX
  polling.
- Each expanded file is an eager `VStack` (`FileDiffBody` in
  `GitWorkingTree/DiffReviewView.swift`) of rows, each an `HStack` of gutter
  and line `Text` under `.textSelection(.enabled)`, which adds a selection
  overlay per text. It is eager on purpose: a selection cannot span a lazy
  container's rows.
- Accessibility grows with lines: two files fill the harness's 500-node route
  capture.
- Lines truncate at the pane's width, with no horizontal scroll.

## Next

Draw each expanded file as one read-only, selectable TextKit 2 `NSTextView`:
the file's lines as one attributed string, with line numbers, change markers,
and line backgrounds drawn beside and behind the text. That gives selection
across lines, Copy, Find (`usesFindBar`), horizontal scrolling, and one
accessibility text area per file. File headers, pinning, and collapse stay.
Review renders unified only.

Open choices:

1. **Host.** A) a representable per file inside the current `LazyVStack`;
   B) an AppKit scroll view for the whole tool, which then pins headers
   itself. Recommended: A, then measure.
2. **Gutter and backgrounds.** A ruler view, or drawing in custom text
   layout fragments.
3. **Highlighting.** `DiffHighlightStore` memoizes an `AttributedString` per
   line; the text view needs one per file.

Done when:

- An animated show with Review over a large diff does not stall in a sample,
  and the loader is removed.
- A drag selects across a file's lines; Copy omits line numbers; ⌘F finds
  within the pane.
- The `chat.tools-pane` Review capture fits the route policy untruncated, and
  its truncating policy is removed.
- VoiceOver reads a file as one text area with line navigation.
