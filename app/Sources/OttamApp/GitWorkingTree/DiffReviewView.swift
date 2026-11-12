import OttamAccessibility
import SwiftUI
import OttamCore

/// The diff content layer stays transparent at the pane level; each changed-file card
/// provides the opaque editor surface over the AppKit background extension.
struct DiffReviewView: View {
    @Bindable var model: DiffReviewModel
    @State private var collapsedFiles: Set<String> = []
    @State private var pinnedHeaderPaths: Set<String> = []

    private let scrollCoordinateSpace = "diff-review-scroll"

    var body: some View {
        Group {
            if let errorText = model.errorText {
                DiffMessageView(
                    icon: "exclamationmark.triangle",
                    title: "Backend error",
                    detail: errorText
                )
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier(AccessibilityID.conversationToolsReview)
                .accessibilityLabel("Review")
            } else if model.isLoading && !model.hasSnapshot {
                ProgressView("Loading diff…")
                    .controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .accessibilityIdentifier(AccessibilityID.conversationToolsReview)
                    .accessibilityLabel("Review")
            } else if model.visibleFiles.isEmpty {
                DiffMessageView(
                    icon: "checkmark.circle",
                    title: "No tracked changes",
                    detail: model.repoDisplayPath
                )
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier(AccessibilityID.conversationToolsReview)
                .accessibilityLabel("Review")
            } else {
                diffScroll
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.clear)
    }

    private var diffScroll: some View {
        ScrollViewReader { proxy in
            ScrollView {
                // `pinnedViews: [.sectionHeaders]` gives the Xcode behavior: the file header
                // for whatever you're currently scrolling through stays pinned at the top
                // until the next file's header pushes it off. The card look is preserved by
                // rounding only the header's top corners and only the body's bottom corners,
                // so at rest they read as one container.
                LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                    if !model.omittedFiles.isEmpty {
                        OmittedFilesNotice(omittedFiles: model.omittedFiles)
                            .padding(.bottom, 12)
                    }

                    ForEach(model.visibleFiles, id: \.path) { file in
                        let canExpand = model.hasRenderableDiff(file)
                        let collapsed = canExpand && collapsedFiles.contains(file.path)
                        Section {
                            if canExpand && !collapsed {
                                FileBodyCard(
                                    model: model,
                                    file: file
                                )
                                    .padding(.bottom, 12)
                            }
                        } header: {
                            StickyFileHeader(
                                file: file,
                                isCollapsed: collapsed,
                                canExpand: canExpand,
                                isPinned: pinnedHeaderPaths.contains(file.path),
                                toggle: { toggleCollapse(file.path) }
                            )
                            .padding(.bottom, canExpand && !collapsed ? 0 : 12)
                            .id(file.path)
                            .background {
                                GeometryReader { proxy in
                                    Color.clear.preference(
                                        key: HeaderTopPreferenceKey.self,
                                        value: [
                                            file.path: proxy.frame(
                                                in: .named(scrollCoordinateSpace)
                                            ).minY
                                        ]
                                    )
                                }
                            }
                        }
                    }
                }
                .padding(.horizontal, 14)
                .padding(.bottom, 14)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(AccessibilityID.conversationToolsReview)
                .accessibilityLabel("Review")
            }
            .coordinateSpace(name: scrollCoordinateSpace)
            .toolbarScrollEdge()
            .onPreferenceChange(HeaderTopPreferenceKey.self) { positions in
                pinnedHeaderPaths = Set(
                    positions.compactMap { path, minY in
                        minY <= 0.5 ? path : nil
                    }
                )
            }
            .onChange(of: model.selectedFilePath) { _, newValue in
                guard let newValue else { return }
                var transaction = Transaction(animation: nil)
                transaction.disablesAnimations = true
                withTransaction(transaction) {
                    proxy.scrollTo(newValue, anchor: .top)
                }
            }
        }
    }

    private func toggleCollapse(_ path: String) {
        if collapsedFiles.contains(path) {
            collapsedFiles.remove(path)
        } else {
            collapsedFiles.insert(path)
        }
    }
}

// MARK: - Sticky card pieces (live scroll)

/// Pinned section header styled as the top of a card: top corners rounded, hairline border.
/// When SwiftUI pins it to the top edge, square the top corners so row colors underneath
/// cannot show through the clipped rounded corner.
private struct StickyFileHeader: View {
    let file: CoreDiffFile
    let isCollapsed: Bool
    let canExpand: Bool
    let isPinned: Bool
    let toggle: () -> Void

    var body: some View {
        if !canExpand || isCollapsed {
            if isPinned {
                styled(Rectangle())
            } else {
                styled(RoundedRectangle(cornerRadius: 8, style: .continuous))
            }
        } else if isPinned {
            styled(Rectangle())
        } else {
            styled(
                UnevenRoundedRectangle(
                    topLeadingRadius: 8,
                    topTrailingRadius: 8,
                    style: .continuous
                )
            )
        }
    }

    private func styled<S: InsettableShape>(_ shape: S) -> some View {
        FileSectionHeader(file: file, isCollapsed: isCollapsed, canExpand: canExpand, toggle: toggle)
            .clipShape(shape)
            .overlay {
                shape.strokeBorder(ReviewColors.cardBorder, lineWidth: 1)
            }
    }
}

private struct HeaderTopPreferenceKey: PreferenceKey {
    static var defaultValue: [String: CGFloat] = [:]

    static func reduce(value: inout [String: CGFloat], nextValue: () -> [String: CGFloat]) {
        value.merge(nextValue()) { _, newValue in newValue }
    }
}

/// The diff body styled as the bottom of a card: bottom corners rounded, hairline border.
private struct FileBodyCard: View {
    @Bindable var model: DiffReviewModel
    let file: CoreDiffFile

    private let shape = UnevenRoundedRectangle(
        bottomLeadingRadius: 8,
        bottomTrailingRadius: 8,
        style: .continuous
    )

    var body: some View {
        FileDiffBody(model: model, file: file)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(ReviewColors.editorBackground)
            .clipShape(shape)
            .overlay {
                shape.strokeBorder(ReviewColors.cardBorder, lineWidth: 1)
            }
    }
}

// MARK: - Per-file body

struct FileDiffBody: View {
    @Environment(\.appTypography) private var typography

    @Bindable var model: DiffReviewModel
    let file: CoreDiffFile

    var body: some View {
        let language = SyntaxHighlighter.Language(path: file.path)
        // Zero-spacing stack so rows abut with no gaps; an implicit stack would insert
        // default spacing between rows (visible as backdrop-colored seams).
        //
        // Eager on purpose. A `LazyVStack` here would stop materializing rows that
        // scroll away, which is exactly what `.textSelection` needs: a selection
        // cannot span a lazy container's children, so dragging across a screenful
        // caps at whichever single row it started in. The outer file list at the
        // top of this file is still lazy — that bounds *files*, and files stay
        // materialized while expanded.
        VStack(alignment: .leading, spacing: 0) {
            rows(language: language)
        }
        .textSelection(.enabled)
    }

    private func rows(language: SyntaxHighlighter.Language) -> some View {
        ForEach(model.unifiedRows(for: file)) { row in
            switch row.content {
            case .separator(let count):
                UnmodifiedSeparator(count: count)
            case .line(let line):
                UnifiedDiffRow(
                    highlighter: model.highlighter,
                    line: line,
                    language: language,
                    typography: typography
                )
            }
        }
    }
}

// MARK: - Sticky file header

struct FileSectionHeader: View {
    let file: CoreDiffFile
    let isCollapsed: Bool
    let canExpand: Bool
    let toggle: () -> Void

    var body: some View {
        let state = DiffFileDisplayState(file: file)
        HStack(spacing: 8) {
            if canExpand {
                Button(action: toggle) {
                    Image(systemName: isCollapsed ? "chevron.right" : "chevron.down")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(ReviewColors.secondaryText)
                        .frame(width: 14)
                }
                .buttonStyle(.plain)
            } else {
                Image(systemName: "minus")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(ReviewColors.tertiaryText)
                    .frame(width: 14)
                    .help("No line diff to expand")
            }

            Image(systemName: icon.symbol)
                .font(.system(size: 12))
                .foregroundStyle(icon.color)
                .frame(width: 16)

            VStack(alignment: .leading, spacing: 1) {
                Text(file.path)
                    .appFont(.code, .bold)
                    .foregroundStyle(ReviewColors.primaryText)
                    .lineLimit(1)
                    .truncationMode(.middle)

                if let secondaryText = state.secondaryText {
                    Text(secondaryText)
                        .appFont(.codeMeta)
                        .foregroundStyle(ReviewColors.tertiaryText)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }

            Spacer(minLength: 8)

            if let label = state.label {
                DiffStatusBadge(label: label)
            }
            if !canExpand {
                DiffStatusBadge(label: "No line diff")
            }
            DiffCountBadge(additions: file.additions, deletions: file.deletions)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(ReviewColors.fileHeaderBackground)
        .help(helpText(state: state))
    }

    private var icon: (symbol: String, color: Color) {
        switch DiffFileDisplayState(file: file) {
        case .deleted:
            return ("trash", ReviewColors.deletionAccent)
        case .renamed:
            return ("arrow.right", .orange)
        default:
            break
        }

        switch (file.path as NSString).pathExtension.lowercased() {
        case "swift": return ("swift", .orange)
        case "md", "markdown": return ("doc.richtext", .green)
        case "sh", "bash", "zsh": return ("terminal", .teal)
        case "go": return ("curlybraces", .blue)
        case "json", "plist", "yaml", "yml": return ("curlybraces.square", .purple)
        default: return ("doc", ReviewColors.secondaryText)
        }
    }

    private func helpText(state: DiffFileDisplayState) -> String {
        switch state {
        case .renamed(let oldPath):
            if let oldPath, !oldPath.isEmpty {
                return "Renamed from \(oldPath) to \(file.path)"
            }
            return "Renamed \(file.path)"
        case .deleted:
            return "Deleted \(file.path)"
        case .added:
            return "Added \(file.path)"
        case .modified:
            return file.path
        case .other(let status):
            return "\(status.capitalized) \(file.path)"
        }
    }
}

// MARK: - Unified row

private struct UnifiedDiffRow: View {
    // The highlighter, not the model: a row needs one memo lookup, and holding the
    // model would make every row an observer of it. See `DiffHighlightStore`.
    let highlighter: DiffHighlightStore
    let line: CoreDiffLine
    let language: SyntaxHighlighter.Language
    let typography: AppTypography.Resolution

    var body: some View {
        HStack(spacing: 0) {
            GutterNumber(value: displayNumber, kind: line.kind)

            Text(highlighter.line(line.content, language: language, kind: line.kind, typography: typography))
                .lineLimit(1)
                .truncationMode(.tail)
                .padding(.leading, 12)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.vertical, 1)
        .padding(.leading, 3)
        .frame(minHeight: 18)
        .background(rowBackground)
        // Full-height leading marker (drawn after padding) so runs of changed lines read as
        // one continuous bar rather than per-row dashes.
        .overlay(alignment: .leading) {
            Rectangle()
                .fill(markerColor)
                .frame(width: 3)
        }
    }

    private var displayNumber: Int? {
        line.kind == .deletion ? line.oldLine : (line.newLine ?? line.oldLine)
    }

    private var markerColor: Color {
        switch line.kind {
        case .addition: return ReviewColors.additionAccent
        case .deletion: return ReviewColors.deletionAccent
        default: return .clear
        }
    }

    private var rowBackground: Color {
        switch line.kind {
        case .addition: return ReviewColors.additionBackground
        case .deletion: return ReviewColors.deletionBackground
        case .hunk: return ReviewColors.hunkBackground
        case .metadata, .note: return ReviewColors.separatorBackground
        case .context: return .clear
        }
    }
}

// MARK: - Shared pieces

private struct GutterNumber: View {
    let value: Int?
    let kind: CoreDiffLineKind

    var body: some View {
        Text(value.map(String.init) ?? "")
            .appFont(.codeGutter)
            .foregroundStyle(color)
            .frame(width: 42, alignment: .trailing)
            .padding(.leading, 6)
    }

    private var color: Color {
        switch kind {
        case .addition: return ReviewColors.additionAccent
        case .deletion: return ReviewColors.deletionAccent
        default: return ReviewColors.tertiaryText
        }
    }
}

private struct UnmodifiedSeparator: View {
    let count: Int

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "chevron.down")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(ReviewColors.secondaryText)
                .frame(width: 42, alignment: .center)
            Text("\(count) unmodified line\(count == 1 ? "" : "s")")
                .appFont(.label)
                .foregroundStyle(ReviewColors.secondaryText)
            Spacer(minLength: 0)
        }
        .padding(.vertical, 5)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(ReviewColors.separatorBackground.opacity(0.5))
    }
}

private struct OmittedFilesNotice: View {
    let omittedFiles: [CoreDiffOmittedFile]

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle")
                .foregroundStyle(ReviewColors.secondaryText)
            Text(summary)
                .appFont(.callout)
                .foregroundStyle(ReviewColors.secondaryText)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(ReviewColors.separatorBackground.opacity(0.65))
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(ReviewColors.cardBorder, lineWidth: 1)
        }
        .help(omittedFiles.map { "\($0.path): \($0.reason)" }.joined(separator: "\n"))
    }

    private var summary: String {
        let count = omittedFiles.count
        return "\(count) changed file\(count == 1 ? "" : "s") omitted from the rendered diff"
    }
}

struct DiffCountBadge: View {
    let additions: Int
    let deletions: Int

    var body: some View {
        HStack(spacing: 6) {
            Text("+\(additions)")
                .foregroundStyle(ReviewColors.additionAccent)
            Text("−\(deletions)")
                .foregroundStyle(ReviewColors.deletionAccent)
        }
        .appFont(.codeDiff, .bold)
    }
}

private struct DiffStatusBadge: View {
    let label: String

    var body: some View {
        Text(label)
            .appFont(.caption, .bold)
            .foregroundStyle(ReviewColors.secondaryText)
            .lineLimit(1)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(ReviewColors.separatorBackground.opacity(0.7))
            .clipShape(RoundedRectangle(cornerRadius: 4, style: .continuous))
    }
}

private struct DiffMessageView: View {
    let icon: String
    let title: String
    let detail: String

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: icon)
                .font(.system(size: 28, weight: .light))
                .foregroundStyle(ReviewColors.secondaryText)
            Text(title)
                .appFont(.title, .bold)
                .foregroundStyle(ReviewColors.primaryText)
            Text(detail)
                .appFont(.callout)
                .foregroundStyle(ReviewColors.secondaryText)
                .multilineTextAlignment(.center)
                .textSelection(.enabled)
        }
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

// MARK: - Row building

enum DiffRowBuilder {
    struct UnifiedRow: Identifiable {
        enum Content {
            case separator(Int)
            case line(CoreDiffLine)
        }
        let id: Int
        let content: Content
    }

    static func unifiedRows(for file: CoreDiffFile) -> [UnifiedRow] {
        var rows: [UnifiedRow] = []
        rows.reserveCapacity(file.hunks.reduce(0) { $0 + $1.lines.count + 1 })
        var index = 0
        var previousOldEnd = 0

        for hunk in file.hunks where hunk.kind != .metadata {
            let unchanged = hunk.oldStart - previousOldEnd - 1
            if unchanged > 0 {
                rows.append(UnifiedRow(id: index, content: .separator(unchanged)))
                index += 1
            }
            for line in hunk.lines {
                rows.append(UnifiedRow(id: index, content: .line(line)))
                index += 1
            }
            previousOldEnd = hunk.oldStart + max(hunk.oldLines, 1) - 1
        }
        return rows
    }
}
