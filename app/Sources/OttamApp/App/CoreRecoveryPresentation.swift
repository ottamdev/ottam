import AppKit
import OttamAccessibility
import OttamBackendClient
import SwiftUI

struct CoreFailurePresentation: Identifiable {
    let id = UUID()
    let message: String
}

struct CoreRecoverySheet: View {
    let message: String
    var exitError: CoreExitError? = nil
    let retry: () -> Void
    let closeWorkspace: () -> Void

    @State private var diagnostics: CoreFailureDiagnostics?
    @State private var showsDiagnostics = false
    @State private var didCopy = false

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            header
            Text(exitError?.message ?? diagnostics?.summaryLine ?? message)
                .font(.callout)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)

            if let details = exitError?.details, !details.isEmpty {
                Text(details)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }

            DisclosureGroup("Diagnostics", isExpanded: $showsDiagnostics) {
                diagnosticsContent
                    .padding(.top, 8)
            }
            .disclosureGroupStyle(CoreDiagnosticsDisclosureStyle())

            Divider()

            HStack {
                Button("Close Workspace", role: .cancel, action: closeWorkspace)
                Spacer()
                Button("Retry Core", action: retry)
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier(AccessibilityID.coreRecoveryRetry)
            }
        }
        .padding(24)
        .appTypographyRoot(TypographySettings.shared.resolved)
        .frame(width: 520)
        .task(id: message) {
            diagnostics = CoreFailureDiagnostics.capture(message: message)
            showsDiagnostics = exitError == nil
        }
        .interactiveDismissDisabled()
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "bolt.slash.fill")
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(.red)
                .frame(width: 32, height: 32)
            VStack(alignment: .leading, spacing: 3) {
                Text("Core couldn’t be restored")
                    .font(.title3.weight(.semibold))
                    .accessibilityIdentifier(AccessibilityID.coreRecoverySheet)
                    .accessibilityValue("recovery exhausted")
                Text("Retry the connection or close this Workspace.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private var diagnosticsContent: some View {
        if let diagnostics {
            VStack(alignment: .leading, spacing: 10) {
                if !diagnostics.combinedLogText.isEmpty {
                    ScrollView {
                        Text(diagnostics.combinedLogText)
                            .appFont(.code)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(10)
                    }
                    .frame(height: 140)
                    .background(.background.secondary, in: RoundedRectangle(cornerRadius: 8))
                }
                HStack {
                    Button(didCopy ? "Copied" : "Copy Diagnostics", action: copyDiagnostics)
                    Button("Open Logs Folder", action: openLogsFolder)
                }
            }
        } else {
            ProgressView()
                .controlSize(.small)
        }
    }

    private func copyDiagnostics() {
        let text = diagnostics?.clipboardText ?? message
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
        didCopy = true
        Task {
            try? await Task.sleep(for: .seconds(1.5))
            didCopy = false
        }
    }

    private func openLogsFolder() {
        let directory = diagnostics?.logsDirectory
            ?? FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent(".ottam/logs")
        NSWorkspace.shared.activateFileViewerSelecting([
            diagnostics?.traceTail?.url ?? directory
        ])
    }
}

struct CoreDiagnosticsDisclosureStyle: DisclosureGroupStyle {
    func makeBody(configuration: Configuration) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                configuration.isExpanded.toggle()
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: configuration.isExpanded ? "chevron.down" : "chevron.right")
                        .font(.caption.weight(.semibold))
                        .frame(width: 10)
                        .accessibilityHidden(true)
                    configuration.label
                    Spacer(minLength: 0)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .pointerStyle(.link)
            .accessibilityIdentifier(AccessibilityID.coreDiagnosticsToggle)
            .accessibilityValue(configuration.isExpanded ? "Expanded" : "Collapsed")

            if configuration.isExpanded {
                configuration.content
            }
        }
    }
}

/// Opaque only during first launch, before a stable Workspace exists behind it.
struct WorkspaceStartupCover: View {
    @State private var showsSpinner = false

    var body: some View {
        ZStack {
            Rectangle()
                .fill(Color(nsColor: .windowBackgroundColor))
                .ignoresSafeArea()
            VStack(spacing: 10) {
                ProgressView()
                    .controlSize(.large)
                Text("Opening Workspace…")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            .opacity(showsSpinner ? 1 : 0)
        }
        .task {
            try? await Task.sleep(for: .milliseconds(200))
            withAnimation(.easeIn(duration: 0.15)) {
                showsSpinner = true
            }
        }
    }
}
