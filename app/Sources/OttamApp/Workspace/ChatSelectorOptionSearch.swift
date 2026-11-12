import Foundation

/// Search over one of the session selector's option pages.
///
/// Most agents advertise a handful of models, and a plain list is the right
/// shape for those. A multi-provider agent such as oh-my-pi lists every model
/// of every signed-in provider — dozens of rows — and an unbounded popover then
/// runs the height of the screen. Past `threshold` the page gains a search field
/// and a fixed-height scrolling list instead, the same trade
/// `FontFamilyPicker` makes for a couple of hundred font families.
enum ChatSelectorOptionSearch {
    /// Pages with more options than this are searchable and height-capped.
    static let threshold = 12

    static func isSearchable(optionCount: Int) -> Bool {
        optionCount > threshold
    }

    /// The options matching `query`, in their original order.
    ///
    /// Every whitespace-separated term must appear in the label or the id,
    /// ignoring case and diacritics, so "glm 5.3" finds "GLM-5.3" and a model
    /// can be found by the id an agent's docs use for it.
    static func matches<Option>(
        _ options: [Option],
        query: String,
        id: (Option) -> String,
        label: (Option) -> String
    ) -> [Option] {
        let terms = query.split(whereSeparator: \.isWhitespace).map(String.init)
        guard !terms.isEmpty else { return options }
        return options.filter { option in
            let fields = [label(option), id(option)]
            return terms.allSatisfy { term in
                fields.contains { field in
                    field.range(of: term, options: [.caseInsensitive, .diacriticInsensitive]) != nil
                }
            }
        }
    }
}
