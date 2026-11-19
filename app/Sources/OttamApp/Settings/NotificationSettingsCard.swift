import AppKit
import SwiftUI

/// Which banners Ottam shows, in the App pane. They gate banners only: the
/// sidebar dot and the Dock badge show either way.
struct NotificationSettingsCard: View {
    @Bindable var settings: NotificationSettings
    /// Nil where Ottam cannot post at all (tests, UI-test runs).
    let presenter: NotificationPresenter?

    var body: some View {
        SettingsCard {
            SettingsCardRow(
                "Agent Finished",
                subtitle: "A banner when a run ends while you're looking elsewhere."
            ) {
                Toggle("Agent Finished", isOn: $settings.agentFinished)
                    .labelsHidden()
            }

            SettingsCardDivider()

            SettingsCardRow(
                "Agent Needs Input",
                subtitle: "A banner when an agent waits for an approval or an answer."
            ) {
                Toggle("Agent Needs Input", isOn: $settings.agentNeedsInput)
                    .labelsHidden()
            }

            if presenter?.authorization == .denied {
                SettingsCardDivider()

                SettingsCardRow(
                    "Notifications Are Off",
                    subtitle: "System Settings doesn't allow Ottam to show banners."
                ) {
                    Button("Open System Settings…", action: openSystemSettings)
                }
            }
        }
        // Allowed or not is the user's call in System Settings, so it is read
        // each time the pane appears and whenever Ottam comes back to the front.
        .task { await presenter?.refreshAuthorization() }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            Task { await presenter?.refreshAuthorization() }
        }
    }

    private func openSystemSettings() {
        var components = URLComponents()
        components.scheme = "x-apple.systempreferences"
        components.path = "com.apple.Notifications-Settings.extension"
        if let bundleID = Bundle.main.bundleIdentifier {
            components.queryItems = [URLQueryItem(name: "id", value: bundleID)]
        }
        if let url = components.url {
            NSWorkspace.shared.open(url)
        }
    }
}
