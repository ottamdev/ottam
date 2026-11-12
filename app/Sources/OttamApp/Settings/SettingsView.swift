import AppKit
import OttamAccessibility
import OttamGhostty
import OttamPresentationLogic
import SwiftUI

struct SettingsView: View {
    let runtime: OttamAppRuntime
    @State private var selectedMode = AppearanceSettings.current()
    private let typography = TypographySettings.shared
    @SceneStorage("settings.selectedPane") private var selectedPaneRaw = SettingsPane.app.rawValue
    @State private var columnVisibility: NavigationSplitViewVisibility = .all
    @State private var searchText = ""
    @State private var terminalReloadError: String?
    @State private var overlayPinsFontSize = false
    /// Window-lifetime, so the sidebar search can match agent names and a
    /// revisited Agents pane shows its last answer immediately.
    @State private var agentsModel: AgentsSettingsModel

    init(runtime: OttamAppRuntime) {
        self.runtime = runtime
        _agentsModel = State(initialValue: AgentsSettingsModel(client: runtime.client))
    }

    private var selectedPane: SettingsPane {
        SettingsPane(rawValue: selectedPaneRaw) ?? .app
    }

    var body: some View {
        NavigationSplitView(columnVisibility: $columnVisibility) {
            sidebar
        } detail: {
            detail
        }
        .navigationSplitViewStyle(.balanced)
        .accessibilityIdentifier(AccessibilityID.settingsWindow)
        .frame(minWidth: 820, idealWidth: 980, minHeight: 540, idealHeight: 680)
        .onReceive(NotificationCenter.default.publisher(for: AppearanceSettings.changedNotification)) { notification in
            if let mode = notification.object as? AppearanceMode {
                selectedMode = mode
            }
        }
        // The one-shot deep-link. Applied after @SceneStorage has restored —
        // onAppear runs after body — so a deep-link beats the restored pane,
        // and onChange retargets an already-open window.
        .onAppear {
            consumePendingPane()
        }
        .onChange(of: runtime.pendingSettingsPane) {
            consumePendingPane()
        }
#if DEBUG
        .background {
            UITestWindowProbe { window in
                UITestPreparation.prepareSettingsIfRequested(window: window)
            }
            .frame(width: 0, height: 0)
        }
#endif
    }

    private func consumePendingPane() {
        guard let pane = runtime.pendingSettingsPane else { return }
        selectedPaneRaw = pane.rawValue
        runtime.pendingSettingsPane = nil
    }

    private var paneSelection: Binding<SettingsPane?> {
        Binding(
            get: { selectedPane },
            set: { newValue in
                if let newValue {
                    selectedPaneRaw = newValue.rawValue
                }
            }
        )
    }

    private var sidebarPanes: [SettingsPane] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return SettingsPane.allCases }
        // Until the agent list arrives its names are unknown, so Agents stays
        // a candidate rather than flashing "No Results" for "claude".
        let agentNamesPending = !agentsModel.didLoadOnce && agentsModel.errorText == nil
        return SettingsPane.allCases.filter { pane in
            (pane == .agents && agentNamesPending)
                || pane.searchText(agents: agentsModel.agents).localizedCaseInsensitiveContains(query)
        }
    }

    @ViewBuilder
    private var sidebar: some View {
        List(selection: paneSelection) {
            if sidebarPanes.isEmpty {
                Text("No Results")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(sidebarPanes) { pane in
                    SettingsSidebarEntryRow(
                        title: pane.title,
                        symbolName: pane.symbolName
                    )
                    .tag(pane)
                }
            }
        }
        .listStyle(.sidebar)
        .navigationTitle("Settings")
        .searchable(text: $searchText, placement: .sidebar, prompt: "Search Settings")
        // Agent names come from agent.list, which can probe Node. A search is
        // the one thing outside the Agents pane that asks for them, so it
        // loads the list once; an idle window on another pane stays quiet.
        .onChange(of: searchText) { _, query in
            if !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                agentsModel.loadForSearch()
            }
        }
        .navigationSplitViewColumnWidth(210)
    }

    /// Per-pane detail: shows only the selected pane's content rather than one long
    /// scroll-to-section page, and titles the window with the active pane. Agents
    /// owns its own NavigationStack (Xcode-style drill-in per agent), so its
    /// titles come from the stack, not from here.
    @ViewBuilder
    private var detail: some View {
        Group {
            switch selectedPane {
            case .app:
                paneScroll { appSection }
                    .navigationTitle(selectedPane.title)
            case .agents:
                AgentsSettingsSection(runtime: runtime, model: agentsModel)
            case .mcpServers:
                McpServersSettingsSection(runtime: runtime)
            case .storage:
                StorageSettingsView(client: runtime.client)
            case .terminal:
                paneScroll { terminalSection }
                    .navigationTitle(selectedPane.title)
            }
        }
        .toggleStyle(.switch)
    }

    private func paneScroll(@ViewBuilder content: () -> some View) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                content()
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 20)
            .padding(.vertical, 20)
        }
    }

    private var appSection: some View {
        VStack(alignment: .leading, spacing: 14) {
            SettingsSectionHeader("App")

            SettingsCard {
                ThemePickerRow(selectedMode: selectedMode) { mode in
                    selectedMode = AppearanceSettings.select(mode)
                }
            }

            TypographySettingsCard(settings: typography)
        }
    }

    private var terminalSection: some View {
        VStack(alignment: .leading, spacing: 14) {
            SettingsSectionHeader("Terminal")

            SettingsCard {
                SettingsCardRow(
                    "Ghostty Overlay",
                    subtitle: GhosttyTerminalConfigurationReload.overlayConfigURL()?.path ?? "Unavailable",
                    controlWidth: 104
                ) {
                    // Success says nothing: the terminals visibly update. A
                    // failure is the only outcome worth an interruption.
                    Button("Reload") {
                        if !GhosttyTerminalConfigurationReload.reload(source: "settings") {
                            terminalReloadError = GhosttyTerminalConfigurationReload.lastReloadError
                                ?? "Ottam couldn't reload the Ghostty overlay."
                        }
                        overlayPinsFontSize = GhosttyTerminalConfigurationReload.overlayPinsFontSize()
                    }
                }

                // The app's font lands beneath the overlay file, so an overlay
                // that sets `font-size` silently wins over the Code Font size.
                // Say so rather than leaving it mysterious.
                if overlayPinsFontSize {
                    SettingsCardDivider()

                    SettingsCardRow(
                        "Font Override",
                        subtitle: "The overlay sets font-size, which overrides Code Font's size in the terminal."
                    ) {
                        EmptyView()
                    }
                }
            }
        }
        // The overlay is a file the user edits outside the app, so this is read
        // when the pane appears rather than cached at launch.
        .task {
            overlayPinsFontSize = GhosttyTerminalConfigurationReload.overlayPinsFontSize()
        }
        .alert(
            "Couldn't Reload Terminal Settings",
            isPresented: Binding(
                get: { terminalReloadError != nil },
                set: { if !$0 { terminalReloadError = nil } }
            )
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(terminalReloadError ?? "")
        }
    }

}

private struct ThemePickerRow: View {
    let selectedMode: AppearanceMode
    let onSelect: (AppearanceMode) -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Text("Theme")
                .font(.body)
                .frame(maxWidth: .infinity, alignment: .leading)

            HStack(spacing: 10) {
                ForEach(AppearanceMode.allCases) { mode in
                    ThemeOptionButton(
                        mode: mode,
                        isSelected: selectedMode == mode,
                        onSelect: onSelect
                    )
                }
            }
            .layoutPriority(1)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ThemeOptionButton: View {
    let mode: AppearanceMode
    let isSelected: Bool
    let onSelect: (AppearanceMode) -> Void

    var body: some View {
        Button {
            onSelect(mode)
        } label: {
            VStack(spacing: 5) {
                ThemeOptionThumbnail(mode: mode)
                    .frame(width: 76, height: 50)

                Text(mode.displayName)
                    .font(.chrome(.caption, weight: isSelected ? .semibold : .regular))
                    .foregroundStyle(isSelected ? .primary : .secondary)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .background(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(isSelected ? Color.accentColor.opacity(0.12) : Color.clear)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .stroke(isSelected ? Color.accentColor : Color.clear, lineWidth: 2)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .focusable(false)
        .accessibilityLabel(mode.displayName)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }
}

private struct ThemeOptionThumbnail: View {
    let mode: AppearanceMode

    var body: some View {
        ZStack {
            if mode == .system {
                ThemeWindowThumbnail(isDark: false)
                    .mask(alignment: .leading) {
                        Rectangle()
                            .frame(width: 38)
                    }
                ThemeWindowThumbnail(isDark: true)
                    .mask(alignment: .trailing) {
                        Rectangle()
                            .frame(width: 38)
                    }
                Rectangle()
                    .fill(Color.primary.opacity(0.16))
                    .frame(width: 1)
            } else {
                ThemeWindowThumbnail(isDark: mode == .dark)
            }
        }
    }
}

private struct ThemeWindowThumbnail: View {
    let isDark: Bool

    var body: some View {
        GeometryReader { geometry in
            let width = geometry.size.width
            let height = geometry.size.height

            ZStack {
                LinearGradient(
                    colors: isDark
                        ? [Color(red: 0.10, green: 0.11, blue: 0.22), Color(red: 0.05, green: 0.06, blue: 0.10)]
                        : [Color(red: 0.64, green: 0.82, blue: 0.96), Color(red: 0.24, green: 0.42, blue: 0.78)],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )

                Path { path in
                    path.move(to: CGPoint(x: 0, y: height * 0.52))
                    path.addQuadCurve(
                        to: CGPoint(x: width, y: height),
                        control: CGPoint(x: width * 0.52, y: height * 0.20)
                    )
                    path.addLine(to: CGPoint(x: width, y: 0))
                    path.addLine(to: CGPoint(x: 0, y: 0))
                }
                .fill(
                    LinearGradient(
                        colors: isDark
                            ? [Color(red: 0.20, green: 0.22, blue: 0.56).opacity(0.50), .clear]
                            : [Color(red: 0.82, green: 0.91, blue: 1.00).opacity(0.58), .clear],
                        startPoint: .topLeading,
                        endPoint: .bottomTrailing
                    )
                )

                VStack(spacing: 0) {
                    HStack {
                        Image(systemName: "applelogo")
                            .font(.system(size: max(height * 0.08, 6)))
                            .foregroundStyle(isDark ? .white : .black)
                            .opacity(0.75)
                        Spacer()
                    }
                    .padding(.horizontal, max(width * 0.04, 4))
                    .frame(height: max(height * 0.12, 8))
                    .background(.ultraThinMaterial)
                    Spacer()
                }

                VStack(spacing: 0) {
                    Rectangle()
                        .fill(isDark ? Color(white: 0.20) : Color(white: 0.90))
                        .frame(height: max(height * 0.15, 8))
                    ZStack(alignment: .top) {
                        Rectangle()
                            .fill(isDark ? Color(white: 0.15) : Color(white: 0.98))
                        RoundedRectangle(cornerRadius: max(width * 0.02, 2), style: .continuous)
                            .fill(Color.accentColor)
                            .frame(height: max(height * 0.12, 6))
                            .padding(max(width * 0.04, 4))
                    }
                }
                .clipShape(RoundedRectangle(cornerRadius: max(width * 0.04, 4), style: .continuous))
                .frame(width: width * 0.65, height: height * 0.45)
                .shadow(color: .black.opacity(isDark ? 0.40 : 0.15), radius: 4, x: 0, y: 2)
                .offset(x: -width * 0.08, y: -height * 0.10)

                VStack(spacing: 0) {
                    ZStack {
                        Rectangle()
                            .fill(isDark ? Color(white: 0.18) : Color(white: 0.92))
                        HStack(spacing: max(width * 0.025, 2)) {
                            Circle()
                                .fill(Color(red: 1.00, green: 0.37, blue: 0.34))
                                .frame(width: max(width * 0.04, 3))
                            Circle()
                                .fill(Color(red: 1.00, green: 0.74, blue: 0.18))
                                .frame(width: max(width * 0.04, 3))
                            Circle()
                                .fill(Color(red: 0.15, green: 0.79, blue: 0.25))
                                .frame(width: max(width * 0.04, 3))
                            Spacer()
                        }
                        .padding(.horizontal, max(width * 0.04, 4))
                    }
                    .frame(height: max(height * 0.18, 10))
                    Rectangle()
                        .fill(isDark ? Color(white: 0.10) : .white)
                }
                .clipShape(RoundedRectangle(cornerRadius: max(width * 0.05, 5), style: .continuous))
                .shadow(color: .black.opacity(isDark ? 0.50 : 0.20), radius: 6, x: 0, y: 3)
                .frame(width: width * 0.75, height: height * 0.55)
                .offset(x: width * 0.12, y: height * 0.20)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .stroke(Color.primary.opacity(0.10), lineWidth: 1)
        )
    }
}
