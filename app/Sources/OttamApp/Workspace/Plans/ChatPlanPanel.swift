import AppKit
import OttamAccessibility
import OttamBackendClient
import SwiftUI

/// A plan waiting for approval, in a glass card stacked above the composer
/// like the question panel. The plan itself reads in the Plan tab; the panel
/// names it and takes the decision.
struct ChatPlanPanel: View {
    let permission: ChatPendingPermission
    let plan: ChatPlan
    /// Called with the chosen option ID.
    let onResolve: (String) -> Void
    /// Keep planning, then type what to change in the composer.
    let onRevise: (String?) -> Void
    let onOpenPlan: () -> Void
    @State private var selection: String?
    @State private var isCollapsed = false
    @FocusState private var isFocused: Bool

    private var rows: [AgentPermissionOption] { permission.planRows }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            if !isCollapsed {
                VStack(alignment: .leading, spacing: 10) {
                    if !plan.summary.isEmpty {
                        Text(plan.attributedSummary)
                            .appFont(.body)
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    options
                    footer
                }
                .transition(.asymmetric(
                    insertion: .opacity.animation(.easeIn(duration: 0.16).delay(0.1)),
                    removal: .opacity.animation(.easeOut(duration: 0.08))
                ))
            }
        }
        .composerPanelChrome()
        // As in the question panel: number keys and ⌘↩ act only while focus
        // is in the panel, so a draft in the composer never approves a plan.
        .focusable()
        .focusEffectDisabled()
        .focused($isFocused)
        .focusSection()
        .onKeyPress(characters: .decimalDigits) { press in
            guard !isCollapsed, let digit = Int(press.characters), rows.indices.contains(digit - 1) else { return .ignored }
            selection = rows[digit - 1].optionID
            return .handled
        }
        .onKeyPress(keys: [.return]) { press in
            guard press.modifiers == .command, !isCollapsed, let selection else { return .ignored }
            onResolve(selection)
            return .handled
        }
        .onAppear {
            selection = rows.first?.optionID
            if !(NSApp.keyWindow?.firstResponder is NSText) { isFocused = true }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(plan.title)
        .accessibilityIdentifier(AccessibilityID.chatPlanPanel)
        .accessibilityValue(plan.summary)
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: "list.bullet.clipboard")
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            Text(plan.title)
                .appFont(.body, .bold)
                .lineLimit(1)
            if permission.request.subagentThreadID != nil {
                Text(permission.request.subagentName.map { "Asked by \($0)" } ?? "Asked by a subagent")
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
            Button("Open Plan", action: onOpenPlan)
                .buttonStyle(.link)
                .help("Show the plan in the tools pane")
                .accessibilityIdentifier(AccessibilityID.chatPlanOpen)
            Button {
                withAnimation(.snappy) { isCollapsed.toggle() }
            } label: {
                Image(systemName: isCollapsed ? "chevron.up" : "chevron.down")
                    .frame(width: 16, height: 16)
            }
            .buttonStyle(.borderless)
            .help(isCollapsed ? "Show the plan request" : "Hide the plan request")
            .accessibilityLabel(isCollapsed ? "Show plan request" : "Hide plan request")
            .accessibilityIdentifier(AccessibilityID.chatPlanCollapse)
        }
    }

    private var options: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(Array(rows.enumerated()), id: \.element.optionID) { index, option in
                PlanOptionRow(option: option, number: index + 1, isSelected: selection == option.optionID) {
                    selection = option.optionID
                }
            }
        }
        // VoiceOver reads the rows as the radio group they are.
        .accessibilityRepresentation {
            Picker(plan.title, selection: $selection) {
                ForEach(rows, id: \.optionID) { option in
                    Text(option.name).tag(Optional(option.optionID))
                }
            }
            .pickerStyle(.radioGroup)
        }
    }

    private var footer: some View {
        HStack(spacing: 8) {
            Button("Revise") { onRevise(permission.reviseOption?.optionID) }
                .buttonStyle(.borderless)
                .help(permission.reviseOption?.name ?? "Keep planning and say what to change")
                .accessibilityIdentifier(AccessibilityID.chatPlanRevise)
            Spacer(minLength: 0)
            Button("Continue") {
                if let selection { onResolve(selection) }
            }
            .buttonStyle(.borderedProminent)
            .help("Continue with the selected option (⌘↩)")
            .disabled(selection == nil)
            .accessibilityIdentifier(AccessibilityID.chatPlanSubmit)
        }
    }
}

/// An option: its number key and the adapter's label. Selection uses the
/// system tint.
private struct PlanOptionRow: View {
    let option: AgentPermissionOption
    let number: Int
    let isSelected: Bool
    let onSelect: () -> Void

    var body: some View {
        Button(action: onSelect) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: isSelected ? "largecircle.fill.circle" : "circle")
                    .foregroundStyle(isSelected ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
                    .accessibilityHidden(true)
                Text(option.name)
                    .appFont(.body)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 8)
                if number <= 9 {
                    Text(String(number))
                        .appFont(.caption)
                        .foregroundStyle(.tertiary)
                        .monospacedDigit()
                        .accessibilityHidden(true)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 6)
            .background(
                isSelected ? Color.accentColor.opacity(0.14) : Color.clear,
                in: RoundedRectangle(cornerRadius: 8, style: .continuous)
            )
            .contentShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        }
        .buttonStyle(.plain)
    }
}
