import Foundation

/// Whether a Model/Effort/Mode page in the composer's session selector shows
/// its own "agent default" row, or lets one advertised option stand in for it.
///
/// Split out of the view because the rule is not obvious and the view has no
/// seam: deciding it here lets the collapse be tested without a rendered
/// popover.
enum ChatSelectorDefaultRow {
    /// "Default" resolved to the value the engine actually runs, when known.
    ///
    /// A name that already says "default" is used as-is. That is what makes the
    /// collapse necessary at all: Claude advertises a model literally named
    /// "Default (recommended)", so the synthesized row and the option row for
    /// it render the same string.
    static func label(_ name: String?) -> String {
        guard let name else { return "Default" }
        return name.localizedCaseInsensitiveContains("default") ? name : "Default — \(name)"
    }

    /// The id of the option that reads exactly like the synthesized default row
    /// and therefore replaces it, or `nil` when the page shows its own row.
    ///
    /// Matching on the id alone would collapse nearly every agent, since a
    /// default is normally one of the advertised options — and that costs a
    /// state the user can otherwise express, because pinning today's default so
    /// it stays put is not the same as following whatever the agent switches
    /// to. Only an identical label is a genuine duplicate: Codex's
    /// "Default — GPT-5" above a "GPT-5" row reads as two distinct choices.
    static func absorbingOption(
        defaultID: String?,
        defaultName: String?,
        options: [(id: String, label: String)]
    ) -> String? {
        guard let defaultID else { return nil }
        let synthesized = label(defaultName)
        return options.contains { $0.id == defaultID && $0.label == synthesized }
            ? defaultID
            : nil
    }
}
