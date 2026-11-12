---
kind: spec
status: active
---

# Quote Reply

## Current Status

Quote reply works in the chat transcript: a Reply | Copy capsule follows a
selection, Reply grows it into a comment card, Add stages the quote and
comment above the composer, and the next send carries them as block quotes
ahead of the draft. The contract is in
[macOS Native UI](../../../architecture/app/macos-ui.md). M2, the same in
the Plan tab, is next and starts when its Try it is approved
([ACP work plan › Not Scheduled](../../backend/acp/README.md#not-scheduled)).

## Next

### M2: quote reply in the Plan tab

- [ ] `quote:4` Reply to a selection in the Plan tab.
  - Why: a plan runs to dozens of lines, and Revise sends whatever is in the
    composer as the next prompt, so feedback needs to point at the lines it
    is about.
  - Scope: the Plan tab's text hosts the same `QuoteReplyPresenter`. Reply
    stages the quote and comment above the composer like a transcript
    reply; the staged message goes out with Revise's prompt, or with the
    next send while the request is pending. Decide with this item: whether
    the quote names its source (a heading, or "From the plan"), and whether
    Reply on a pending plan presses Revise itself.
  - Done when: with a pending Codex or Claude plan, selecting plan text and
    replying sends the agent a block quote of those lines and the comment,
    and the revised plan replaces the tab's content.
  - Verify with: focused Swift tests for staging from the plan source; a
    real-window run on Codex and Claude with screenshots plus AX
    (`chat.quote-reply.*` ids on the Plan tab).

Try it for M2 is written when it starts.

## Backlog

- Two transcript checks not yet made in the real window: the capsule
  segments' hover fill (synthetic mouse moves do not show it) and top-edge
  pinning (unit-tested). Trigger: the next real-window pass over the
  transcript.
- A registered UI scenario for quote reply; the `chat.quote-reply.*` AX ids
  exist but no scenario uses them. Trigger: M2's walkthrough.

## Decisions

1. **Add, not send (2026-10-20).** A reply waits above the composer, and the
   next message sends the staged replies in order, then the draft, so the
   composer stays the one place a message leaves from.
2. **AppKit presenter, SwiftUI glass (2026-10-20).** The overlay is an
   AppKit subview above the clip view so it moves in the pass that scrolls
   the text; the glass is SwiftUI because `NSGlassEffectView` has no
   materialize. Never a `glassEffectID` or `glassEffectUnion` (macOS 26
   key-view loop); see [macOS Native UI](../../../architecture/app/macos-ui.md).
