import OttamPresentationLogic
import SwiftUI

/// The three-layer ACP compatibility matrix for one agent: what the SDK
/// build carries, what the agent advertised, what Ottam handles. Rows and
/// wording come from `AgentCompatibilityPresentation`; this is a plain
/// `Table` behind the same header the sign-in sheet uses.
struct AgentCompatibilitySheet: View {
    let title: String
    let presentation: AgentCompatibilityPresentation
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(.headline)
                    Text(presentation.summary)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Done") { dismiss() }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
            }
            .padding(16)
            Divider()
            Table(of: AgentCompatibilityPresentation.Row.self) {
                TableColumn("") { row in
                    Image(systemName: row.verdictSymbol)
                        .accessibilityLabel(row.verdictLabel)
                        .help(row.verdictLabel)
                }
                .width(24)
                TableColumn("Feature", value: \.feature)
                TableColumn("ACP SDK", value: \.sdkText)
                TableColumn("Agent") { row in
                    DetailCell(text: row.agentText, detail: row.agentDetail)
                }
                .width(min: 48, ideal: 60)
                TableColumn("Ottam") { row in
                    DetailCell(text: row.clientText, detail: row.clientDetail)
                }
            } rows: {
                ForEach(presentation.sections) { section in
                    Section(section.title) {
                        ForEach(section.rows) { row in
                            TableRow(row)
                        }
                    }
                }
            }
            Divider()
            Text(footer)
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
        }
        .systemTypographyRoot()
        .frame(minWidth: 720, minHeight: 460)
    }

    /// A cell whose short word has a longer story: the info symbol says
    /// there is one, and the whole cell (text and symbol) shows it on hover.
    private struct DetailCell: View {
        let text: String
        let detail: String?

        var body: some View {
            if let detail {
                HStack(spacing: 4) {
                    Text(text)
                    Image(systemName: "info.circle")
                        .foregroundStyle(.secondary)
                        .accessibilityHidden(true)
                }
                .help(detail)
                .accessibilityElement(children: .combine)
                .accessibilityValue(detail)
            } else {
                Text(text)
            }
        }
    }

    private var footer: String {
        let observed =
            "Session updates and client requests are only observed once a session runs."
        return presentation.isUnprobed
            ? "The agent has not been started yet, so the Agent column is empty. \(observed)"
            : "Agent answers come from the last initialize. \(observed)"
    }
}
