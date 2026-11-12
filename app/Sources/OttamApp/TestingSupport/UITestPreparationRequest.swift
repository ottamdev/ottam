#if DEBUG
import AppKit
import Foundation

extension UITestPreparation {
    struct PreparationRequest {
        /// Whether the run captures the workspace window's Chat page.
        let preparesWorkspace: Bool
        let exitWhenFinished: Bool
        let windowFrameSize: NSSize?
        let externalStateDirectory: URL?
        /// Applied only inside the uniquely identified isolated test process,
        /// so it remains an evidence fixture rather than product state.
        let appearance: NSAppearance.Name?
        let preparesWorkspaceChooser: Bool
        let preparesSettings: Bool
        let preparesMarkdownLab: Bool
        /// Which lab fixture the gallery opens on, in the kebab-case spelling
        /// scenario IDs use. Nil keeps the lab's own default.
        let markdownLabFixture: String?
        /// Which lab tab opens (`gallery`, `streaming`, `cards`). Nil keeps
        /// the lab's own default.
        let markdownLabTab: String?
        let createEmptyWorkspace: Bool
        let quiet: Bool
        let workspaceCount: Int
        /// Extra Git-backed project paths to add onto the primary Workspace,
        /// beyond the one `openProject` seeds — the multi-project sidebar
        /// shape, as opposed to `workspaceCount`'s multi-*window* shape.
        /// Fixture ownership stays with the harness that sets this env var,
        /// not with the app.
        let additionalProjectPaths: [String]
        let conversationSelection: String?

        static func fromEnvironment() -> PreparationRequest? {
            let environment = ProcessInfo.processInfo.environment
            guard environment["OTTAM_UI_TEST_MODE"] == "1" else {
                return nil
            }
            guard let externalStateDirectory = parseExternalStateDirectory(
                environment["OTTAM_SCREENSHOT_EXTERNAL_DIR"]
            ) else {
                return nil
            }

            let preparesWorkspace = parsePreparesWorkspace(
                environment["OTTAM_SCREENSHOT_ROUTES"]
            )
            let preparesWorkspaceChooser =
                environment["OTTAM_SCREENSHOT_CHOOSER"] == "1"
            let preparesSettings =
                environment["OTTAM_UI_TEST_CAPTURE_SETTINGS"] == "1"
            let preparesMarkdownLab =
                environment["OTTAM_UI_TEST_CAPTURE_MARKDOWN_LAB"] == "1"
            guard preparesWorkspace || preparesWorkspaceChooser || preparesSettings
                || preparesMarkdownLab
            else {
                log("no valid routes requested")
                return nil
            }

            return PreparationRequest(
                preparesWorkspace: preparesWorkspace,
                exitWhenFinished:
                    environment["OTTAM_SCREENSHOT_EXIT"] != "0",
                windowFrameSize: parseWindowFrameSize(
                    environment["OTTAM_SCREENSHOT_WINDOW_SIZE"]
                ),
                externalStateDirectory: externalStateDirectory,
                appearance: parseAppearance(
                    environment["OTTAM_UI_TEST_APPEARANCE"]
                ),
                preparesWorkspaceChooser: preparesWorkspaceChooser,
                preparesSettings: preparesSettings,
                preparesMarkdownLab: preparesMarkdownLab,
                markdownLabFixture: parseMarkdownLabFixture(
                    environment["OTTAM_UI_TEST_MARKDOWN_LAB_FIXTURE"]
                ),
                markdownLabTab: parseMarkdownLabFixture(
                    environment["OTTAM_UI_TEST_MARKDOWN_LAB_TAB"]
                ),
                createEmptyWorkspace:
                    environment["OTTAM_SCREENSHOT_EMPTY_WORKSPACE"] == "1",
                quiet: environment["OTTAM_UI_TEST_QUIET"] == "1",
                workspaceCount: parseWorkspaceCount(
                    environment["OTTAM_UI_TEST_WORKSPACE_COUNT"]
                ),
                additionalProjectPaths: parseProjectPaths(
                    environment["OTTAM_UI_TEST_PROJECT_PATHS"]
                ),
                conversationSelection: parseConversationSelection(
                    environment["OTTAM_SCREENSHOT_CONVERSATION"]
                )
            )
        }

        private static func parseWorkspaceCount(_ value: String?) -> Int {
            guard let value, let count = Int(value) else {
                return 1
            }
            return min(max(count, 1), 3)
        }

        /// Same comma-list shape as `parsePreparesWorkspace`; each entry is asserted to
        /// exist by the caller rather than by the app, since the app has no
        /// business synthesizing fixture repositories.
        private static func parseProjectPaths(_ value: String?) -> [String] {
            guard let value = value?
                .trimmingCharacters(in: .whitespacesAndNewlines),
                !value.isEmpty
            else {
                return []
            }
            return value.split(separator: ",").compactMap { component in
                let path = component.trimmingCharacters(
                    in: .whitespacesAndNewlines
                )
                return path.isEmpty ? nil : path
            }
        }

        private static func parseMarkdownLabFixture(_ value: String?) -> String? {
            let normalized = value?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            guard let normalized, !normalized.isEmpty else { return nil }
            return normalized
        }

        private static func parseAppearance(_ value: String?) -> NSAppearance.Name? {
            let normalized = value?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            let name: NSAppearance.Name?
            switch normalized {
            case "light":
                name = .aqua
            case "dark":
                name = .darkAqua
            case nil, "":
                name = nil
            default:
                log("ignoring unknown UI-test appearance \(value ?? "")")
                name = nil
            }
            return name
        }

        private static func parseConversationSelection(
            _ value: String?
        ) -> String? {
            guard let value = value?
                .trimmingCharacters(in: .whitespacesAndNewlines),
                !value.isEmpty
            else {
                return nil
            }
            return value
        }

        private static func parseExternalStateDirectory(
            _ value: String?
        ) -> URL? {
            guard let value = value?
                .trimmingCharacters(in: .whitespacesAndNewlines),
                !value.isEmpty
            else {
                return nil
            }
            return URL(fileURLWithPath: value).standardized
        }

        /// The harness still names pages as a comma list of routes, where
        /// `chat` is the workspace window and `none` skips it. Unset means
        /// `chat`.
        private static func parsePreparesWorkspace(_ value: String?) -> Bool {
            guard let value = value?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !value.isEmpty
            else { return true }
            var preparesWorkspace = false
            for component in value.split(separator: ",") {
                let routeName = component.trimmingCharacters(in: .whitespacesAndNewlines)
                if routeName == "chat" {
                    preparesWorkspace = true
                } else if !routeName.isEmpty, routeName != "none" {
                    log("ignoring unknown route \(routeName)")
                }
            }
            return preparesWorkspace
        }

        private static func parseWindowFrameSize(_ value: String?) -> NSSize? {
            guard let value = value?
                .trimmingCharacters(in: .whitespacesAndNewlines),
                !value.isEmpty
            else {
                return nil
            }
            let parts = value.lowercased()
                .split(separator: "x", maxSplits: 1)
                .map {
                    String($0).trimmingCharacters(
                        in: .whitespacesAndNewlines
                    )
                }
            guard parts.count == 2,
                  let width = Double(parts[0]),
                  let height = Double(parts[1]),
                  width > 0,
                  height > 0
            else {
                log("ignoring invalid screenshot window size \(value)")
                return nil
            }
            return NSSize(width: width, height: height)
        }
    }
}
#endif
