import AppKit
import SwiftUI

/// A font family control that opens a bounded, searchable list rather than a menu.
///
/// A `Picker` would be the native choice, but its `NSMenu` sizes itself to the
/// screen — there is no max height at any layer — and a couple of hundred
/// families fills the display top to bottom. A popover holding a `List` is the
/// same shape the system font panel uses, and it brings search with it, which is
/// what a list this long actually needs.
///
/// Selection applies as it moves, so arrowing through the list previews each
/// family live. That matches the rest of the typography card, where every control
/// takes effect immediately.
struct FontFamilyPicker: View {
    let families: [String]
    @Binding var family: String?

    @State private var isPresented = false
    @State private var searchText = ""

    var body: some View {
        Button {
            isPresented = true
        } label: {
            HStack(spacing: 6) {
                Text(label)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Image(systemName: "chevron.up.chevron.down")
                    .imageScale(.small)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(width: 190)
        .popover(isPresented: $isPresented, arrowEdge: .bottom) {
            content
        }
    }

    private var label: String {
        family ?? FontFamilyPicker.systemLabel
    }

    static let systemLabel = "System font"

    private var content: some View {
        VStack(spacing: 0) {
            TextField("Search", text: $searchText)
                .textFieldStyle(.roundedBorder)
                .padding(10)

            Divider()

            ScrollViewReader { proxy in
                List(selection: selection) {
                    ForEach(rows) { row in
                        Text(row.family ?? FontFamilyPicker.systemLabel)
                            .font(previewFont(for: row.family))
                            .lineLimit(1)
                            .truncationMode(.tail)
                    }
                }
                .listStyle(.plain)
                .onAppear {
                    // The stored family is usually far down a 200-entry list;
                    // opening on it rather than on A is the difference between
                    // "here is your font" and "go find it again".
                    proxy.scrollTo(FamilyRow.id(for: family), anchor: .center)
                }
            }
        }
        // The height that made this control worth building. Wide enough that
        // long family names are not all truncated.
        .frame(width: 260, height: 320)
    }

    private var rows: [FamilyRow] {
        let matches = searchText.isEmpty
            ? families
            : families.filter { $0.localizedCaseInsensitiveContains(searchText) }
        // The system entry is not a family and has no name to match, so it stays
        // pinned at the top whenever the search has not been narrowed past it.
        let system = FontFamilyPicker.systemLabel.localizedCaseInsensitiveContains(searchText)
        return (system ? [FamilyRow(family: nil)] : []) + matches.map { FamilyRow(family: $0) }
    }

    /// Each name written in the font it names — the reason to build a list of
    /// views instead of a menu of strings. Resolved per visible row rather than
    /// per family: the list is lazy, and a search keystroke rebuilds every row.
    private func previewFont(for family: String?) -> Font {
        guard let family, let nsFont = FontCatalog.previewFont(forFamily: family, size: 13) else {
            return .system(size: 13)
        }
        return Font(nsFont)
    }

    /// `List` selection is not optional-friendly when one row means "no family",
    /// so rows carry string ids and this maps back to the binding.
    private var selection: Binding<String?> {
        Binding(
            get: { FamilyRow.id(for: family) },
            set: { newValue in
                guard let newValue else { return }
                family = newValue == FamilyRow.systemID ? nil : newValue
            }
        )
    }
}

private struct FamilyRow: Identifiable {
    let family: String?

    var id: String { FamilyRow.id(for: family) }

    static let systemID = "__system__"

    static func id(for family: String?) -> String {
        family ?? systemID
    }
}
