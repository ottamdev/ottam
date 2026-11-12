---
kind: spec
status: proposed
---

# Markdown Composer and Editor

## Current Status

Proposed, not started. The Chat composer is `ComposerTextView`, a TextKit 2
`NSTextView` wrapped for SwiftUI by `MarkdownTextView`
(`Workspace/MarkdownTextView.swift`); it keeps plain Markdown source and caps
at ten lines. `OttamMarkdownEngine` holds reusable parsing, attributed styling
(`MarkdownStyler`), and tokenization. A read-only document viewer shipped as
the Plan and file tabs ([plan review](../chat/plan-review.md)); edit mode is
this spec. The transcript renderer is out of scope.

## Next

1. Reuse `MarkdownStyler` in the composer with source characters preserved
   and selection/caret mappings unchanged.
2. A typing controller for list continuation, quote continuation, marker
   consumption, and newline behavior, with undo grouping and IME-safe edits.
3. Prototype always-visible versus caret-revealed marker styling; record one
   policy before implementation proceeds.
4. Markdown→attributed→Markdown round-trip property tests before block
   editing adds lossy projection risk.
5. Edit mode for the document viewer, once the composer behavior and
   serialization contract are stable.

Done when:

- Typing, paste, undo/redo, selection, IME input, submit/newline shortcuts,
  and the ten-line height cap keep current behavior.
- Large pasted drafts do not force whole-document layout per keystroke.
- Round-trip tests preserve source modulo a documented normalization policy.
- Tables, footnotes, and raw HTML remain literal editable source.
- The editor shares renderer-neutral parsing and styling without importing
  transcript coordinators or collection-layout types.

## Non-Goals

- A transcript-wide TextKit surface; Chat uses the AppKit collection
  renderer.
- WYSIWYG editing of tables, footnotes, or raw HTML in the first version.
- Changing Chat submission or attachment protocol semantics.
