import OttamAccessibility
import SwiftUI

/// The agent's modes and commands while the composer holds `/`, in the same
/// glass card as the question panel. Unlike that panel it never takes focus:
/// typing keeps filtering, and the composer routes ↑/↓, Return, Tab, and Esc
/// here.
struct ChatCommandPanel: View {
    let picker: ChatCommandPicker
    let onPick: (ChatCommandPickerItem) -> Void

    /// About eight rows; the rest scroll.
    private static let maxListHeight: CGFloat = 264

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    if !picker.modes.isEmpty {
                        section("Modes", picker.modes)
                    }
                    if !picker.commands.isEmpty {
                        section("Commands", picker.commands)
                    } else if !picker.hasCommands {
                        note("No commands yet")
                    }
                    if !picker.skills.isEmpty {
                        section("Skills", picker.skills)
                    }
                    if picker.items.isEmpty, picker.hasCommands {
                        note("No matches")
                    }
                }
            }
            .scrollIndicators(.automatic)
            .frame(maxHeight: Self.maxListHeight)
            .fixedSize(horizontal: false, vertical: true)
            .onChange(of: picker.highlightedID) { _, id in
                guard let id else { return }
                proxy.scrollTo(id)
            }
        }
        .composerPanelChrome()
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Commands")
        .accessibilityIdentifier(AccessibilityID.chatCommandPanel)
        .accessibilityValue(picker.highlighted.map(Self.label) ?? "")
    }

    private func section(_ title: String, _ items: [ChatCommandPickerItem]) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
                .appFont(.callout)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 8)
                .padding(.top, 4)
                .padding(.bottom, 2)
                .accessibilityAddTraits(.isHeader)
            ForEach(items) { item in
                row(item)
            }
        }
    }

    private func note(_ text: String) -> some View {
        Text(text)
            .appFont(.body)
            .foregroundStyle(.secondary)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
    }

    private func row(_ item: ChatCommandPickerItem) -> some View {
        let isHighlighted = item.id == picker.highlightedID
        return Button { onPick(item) } label: {
            HStack(spacing: 8) {
                icon(item)
                    .frame(width: 16)
                    .foregroundStyle(.secondary)
                switch item {
                case .mode(let mode, let isCurrent):
                    Text(mode.displayName)
                        .appFont(.body)
                    Text("mode")
                        .appFont(.body)
                        .foregroundStyle(.secondary)
                    Spacer(minLength: 0)
                    if isCurrent {
                        Image(systemName: "checkmark")
                            .foregroundStyle(.secondary)
                    }
                case .command(let command):
                    Text(command.bareName)
                        .appFont(.body)
                        .layoutPriority(1)
                    Text(command.description)
                        .appFont(.body)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                    Spacer(minLength: 0)
                }
            }
            .lineLimit(1)
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(isHighlighted ? AnyShapeStyle(.quaternary) : AnyShapeStyle(.clear))
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // A click picks the row without taking focus from the composer.
        .focusable(false)
        .id(item.id)
        // The row truncates the description; hovering shows all of it.
        .help(Self.description(item))
        .accessibilityLabel(Self.label(item))
        .accessibilityAddTraits(isHighlighted ? .isSelected : [])
        .accessibilityIdentifier(AccessibilityID.chatCommandRow)
    }

    @ViewBuilder
    private func icon(_ item: ChatCommandPickerItem) -> some View {
        switch item {
        case .mode:
            Image(systemName: "switch.2")
        case .command(let command):
            // The character that invokes it.
            Text(command.isSkill ? "$" : "/")
                .appFont(.body, .bold)
        }
    }

    private static func description(_ item: ChatCommandPickerItem) -> String {
        if case .command(let command) = item { command.description } else { "" }
    }

    private static func label(_ item: ChatCommandPickerItem) -> String {
        switch item {
        case .mode(let mode, let isCurrent):
            "\(mode.displayName) mode" + (isCurrent ? ", current" : "")
        case .command(let command):
            (command.isSkill ? command.name : "/\(command.name)") + ", \(command.description)"
        }
    }
}
