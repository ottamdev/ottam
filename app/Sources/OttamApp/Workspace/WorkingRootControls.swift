import SwiftUI

/// A menu row naming a working root, check-marked when it is the current one.
///
/// Shared rather than copied so the choosers that survive in the
/// multi-project shape cannot drift apart again.
@ViewBuilder
func workingRootMenuLabel(_ title: String, selected: Bool) -> some View {
    if selected {
        Label(title, systemImage: "checkmark")
    } else {
        Text(title)
    }
}
