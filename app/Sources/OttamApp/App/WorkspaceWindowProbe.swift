import AppKit
import OttamAccessibility
import OttamBackendClient
import OttamPresentationLogic
import SwiftUI

struct WelcomeWindowProbe: NSViewRepresentable {
    let registry: WorkspaceWindowRegistry

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async {
            configure(view.window)
        }
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        DispatchQueue.main.async {
            configure(view.window)
        }
    }

    @MainActor
    private func configure(_ window: NSWindow?) {
        guard let window else { return }
        window.standardWindowButton(.miniaturizeButton)?.isHidden = true
        window.standardWindowButton(.zoomButton)?.isHidden = true
        window.tabbingMode = .disallowed
        WindowPlacement.shared.attach(to: window, role: .welcome)
        registry.registerChooser(window)
    }
}

struct WorkspaceChooserRecoveryView: View {
    let state: OttamAppRuntime.ConnectionState
    let exitError: CoreExitError?
    let retry: () -> Void

    var body: some View {
        Group {
            switch presentation {
            case .failure(let failure):
                CoreStartupFailureView(presentation: failure, exitError: exitError, retry: retry)
            case .progress(let title):
                VStack(spacing: 12) {
                    ProgressView()
                    Text(title)
                        .font(.headline)
                }
                .padding(24)
            case .content:
                EmptyView()
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var presentation: CoreRecoveryPresentation {
        switch state {
        case .starting:
            CoreRecoveryPresentation(phase: .starting)
        case .ready:
            CoreRecoveryPresentation(phase: .ready)
        case .recovering(let attempt):
            CoreRecoveryPresentation(phase: .recovering(attempt: attempt))
        case .failed(let message):
            CoreRecoveryPresentation(phase: .failed(message: message))
        }
    }
}

private struct CoreStartupFailureView: View {
    let presentation: CoreRecoveryPresentation.Failure
    let exitError: CoreExitError?
    let retry: () -> Void

    @State private var diagnostics: CoreFailureDiagnostics?
    @State private var didCopy = false
    @State private var showsDiagnostics = false

    var body: some View {
        VStack(spacing: 0) {
            diagnosticScroll
            footer
                .padding(.horizontal, 32)
                .padding(.vertical, 20)
                .frame(maxWidth: 560)
                .frame(maxWidth: .infinity)
        }
        .task(id: presentation.message) {
            diagnostics = CoreFailureDiagnostics.capture(message: presentation.message)
            showsDiagnostics = exitError == nil
        }
    }

    private var diagnosticScroll: some View {
        GeometryReader { geometry in
            // Reserve the system's legacy scrollbar gutter in both states.
            // The content width must not follow the changing scroll viewport.
            let width = max(0, geometry.size.width - NSScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy))
            ScrollView {
                diagnosticContent
                    .frame(width: min(560, width))
                    .frame(width: width)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private var diagnosticContent: some View {
        VStack(alignment: .leading, spacing: 18) {
            header
            summary
            if let details = exitError?.details, !details.isEmpty {
                Text(details)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            DisclosureGroup("Diagnostics", isExpanded: $showsDiagnostics) {
                VStack(alignment: .leading, spacing: 12) {
                    Text(diagnostics?.summaryLine ?? presentation.message)
                        .font(.caption)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                    if let diagnostics, !diagnostics.combinedLogText.isEmpty {
                        logSection(diagnostics)
                    }
                    ViewThatFits(in: .horizontal) {
                        HStack {
                            diagnosticActions
                        }
                        VStack(alignment: .leading) {
                            diagnosticActions
                        }
                    }
                }
                .padding(.top, 8)
            }
            .disclosureGroupStyle(CoreDiagnosticsDisclosureStyle())
        }
        .padding(32)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 12) {
            Image(systemName: "bolt.slash.fill")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(.red)
            VStack(alignment: .leading, spacing: 2) {
                Text("Ottam couldn’t start")
                    .font(.title2.weight(.semibold))
                    .accessibilityIdentifier(AccessibilityID.chooserCoreFailure)
            }
        }
    }

    private var summary: some View {
        Text(exitError?.message ?? "The Ottam engine is unavailable. See the core error log for details, or try again to load your workspaces.")
            .font(.callout)
            .foregroundStyle(.secondary)
            .textSelection(.enabled)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func logSection(_ diagnostics: CoreFailureDiagnostics) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Recent core log")
                    .font(.subheadline.weight(.medium))
                Spacer()
                if let trace = diagnostics.traceTail {
                    Text(trace.url.lastPathComponent)
                        .appFont(.codeMeta)
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            ScrollView {
                Text(diagnostics.combinedLogText)
                    .appFont(.code)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
            }
            .frame(height: 160)
            .background(.background.secondary, in: RoundedRectangle(cornerRadius: 8))
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .strokeBorder(.separator.opacity(0.4))
            )
        }
    }

    private var footer: some View {
        HStack(spacing: 10) {
            Spacer()
            Button("Try Again", action: retry)
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier(AccessibilityID.chooserCoreRetry)
        }
    }

    @ViewBuilder
    private var diagnosticActions: some View {
        Button(didCopy ? "Copied" : "Copy Diagnostics", action: copyDiagnostics)
        Button("Open Logs Folder", action: openLogsFolder)
    }

    private func copyDiagnostics() {
        let text = [exitError.map { "Core exit error: \($0.kind)" }, diagnostics?.clipboardText ?? presentation.message]
            .compactMap { $0 }.joined(separator: "\n\n")
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
        didCopy = true
        Task {
            try? await Task.sleep(nanoseconds: 1_500_000_000)
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
