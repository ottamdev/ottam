import AppKit
import OttamAccessibility
import OttamBackendClient
import SwiftUI

/// An agent's questions, one page each, in a glass card stacked above the
/// composer. It shares the composer's material and corners, and its glass
/// materializes in place as it comes and dissolves as it goes.
struct ChatQuestionPanel: View {
    @Bindable var question: ChatPendingQuestion
    let onResolve: (AgentQuestionOutcome) -> Void
    @FocusState private var isFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            if !question.isCollapsed, let current = question.current {
                VStack(alignment: .leading, spacing: 10) {
                    QuestionPage(question: question, item: current)
                        .id(current.id)
                    footer(current)
                }
                // Its own timing, not the card's: gone before the shrinking
                // card carries it down, in only once the card has room.
                .transition(.asymmetric(
                    insertion: .opacity.animation(.easeIn(duration: 0.16).delay(0.1)),
                    removal: .opacity.animation(.easeOut(duration: 0.08))
                ))
            }
        }
        .composerPanelChrome()
        // Number keys and ⌘↩ act only while focus is in the panel, so a
        // draft typed in the composer never answers by accident. A button's
        // `keyboardShortcut` would fire from anywhere in the window.
        .focusable()
        .focusEffectDisabled()
        .focused($isFocused)
        .focusSection()
        .onKeyPress(characters: .decimalDigits) { press in
            guard !question.isCollapsed,
                  let current = question.current,
                  let digit = Int(press.characters),
                  current.options.indices.contains(digit - 1)
            else { return .ignored }
            question.toggle(current.options[digit - 1].value, in: current)
            return .handled
        }
        .onKeyPress(keys: [.return]) { press in
            guard press.modifiers == .command,
                  !question.isCollapsed,
                  let current = question.current
            else { return .ignored }
            advance(current)
            return .handled
        }
        // Take focus only from no one: a question arriving mid-draft leaves
        // the caret, and the keys typed next, in the composer.
        .onAppear {
            if !(NSApp.keyWindow?.firstResponder is NSText) { isFocused = true }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(question.request.title)
        .accessibilityIdentifier(AccessibilityID.chatQuestionPanel)
        .accessibilityValue(pageText)
    }

    private var pageText: String {
        "\(question.page + 1) of \(question.questions.count)"
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: "questionmark.bubble")
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            Text(question.current?.header ?? "Question")
                .appFont(.body, .bold)
                .lineLimit(1)
            if question.request.subagentThreadID != nil {
                Text(question.request.subagentName.map { "Asked by \($0)" } ?? "Asked by a subagent")
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .accessibilityIdentifier(AccessibilityID.chatQuestionSubagent)
                    .accessibilityValue(question.request.subagentThreadID ?? "")
            }
            Spacer(minLength: 0)
            if question.questions.count > 1 {
                Text(pageText)
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
            Button {
                withAnimation(.snappy) { question.isCollapsed.toggle() }
            } label: {
                Image(systemName: question.isCollapsed ? "chevron.up" : "chevron.down")
                    .frame(width: 16, height: 16)
            }
            .buttonStyle(.borderless)
            .help(question.isCollapsed ? "Show the question" : "Hide the question")
            .accessibilityLabel(question.isCollapsed ? "Show question" : "Hide question")
            .accessibilityIdentifier(AccessibilityID.chatQuestionCollapse)
        }
    }

    private func footer(_ current: AgentQuestion) -> some View {
        HStack(spacing: 8) {
            Button("Skip") { onResolve(.skipped) }
                .buttonStyle(.borderless)
                .help("Continue without answering")
                .accessibilityIdentifier(AccessibilityID.chatQuestionSkip)
            Spacer(minLength: 0)
            if let problem = question.problem(current), question.isAnswered(current) || question.isLastPage {
                Text(problem)
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            if question.page > 0 {
                Button("Back") { question.page -= 1 }
                    .accessibilityIdentifier(AccessibilityID.chatQuestionBack)
            }
            if question.isLastPage {
                Button("Submit") { advance(current) }
                    .buttonStyle(.borderedProminent)
                    .help("Submit (⌘↩)")
                    .disabled(!question.canSubmit)
                    .accessibilityIdentifier(AccessibilityID.chatQuestionSubmit)
            } else {
                Button("Next") { advance(current) }
                    .help("Next question (⌘↩)")
                    .disabled(question.problem(current) != nil)
                    .accessibilityIdentifier(AccessibilityID.chatQuestionNext)
            }
        }
    }

    /// Submit on the last page, Next before it; nothing while the page's
    /// answer can't be sent.
    private func advance(_ current: AgentQuestion) {
        if question.isLastPage {
            guard question.canSubmit else { return }
            onResolve(.submitted(answers: question.answers))
        } else if question.problem(current) == nil {
            question.page += 1
        }
    }
}

/// One question: its prompt and the control its kind calls for.
private struct QuestionPage: View {
    @Bindable var question: ChatPendingQuestion
    let item: AgentQuestion

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(item.prompt)
                .appFont(.body)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
            // A long option list scrolls inside the card rather than growing
            // the bar the composer rides in.
            ScrollView {
                // One stack: a modifier on the bare builder would lay each of
                // its views out on its own, stacking the Other row over the
                // options.
                VStack(alignment: .leading, spacing: 4) {
                    control
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .scrollBounceBehavior(.basedOnSize)
            .frame(maxHeight: 260)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder
    private var control: some View {
        switch item.kind {
        case .singleSelect(let options, let allowsOther, _):
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(options.enumerated()), id: \.element.value) { index, option in
                    QuestionOptionRow(
                        option: option,
                        number: index + 1,
                        multiple: false,
                        isSelected: question.isSelected(option.value, in: item)
                    ) {
                        question.toggle(option.value, in: item)
                    }
                }
            }
            // VoiceOver reads the rows as the radio group they are.
            .accessibilityRepresentation {
                Picker(item.prompt, selection: singleSelection) {
                    ForEach(options, id: \.value) { option in
                        Text(option.label).tag(Optional(option.value))
                    }
                }
                .pickerStyle(.radioGroup)
            }
            if allowsOther { QuestionOtherRow(question: question, item: item) }
        case .multiSelect(let options, let allowsOther, _, _, _):
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(options.enumerated()), id: \.element.value) { index, option in
                    QuestionOptionRow(
                        option: option,
                        number: index + 1,
                        multiple: true,
                        isSelected: question.isSelected(option.value, in: item)
                    ) {
                        question.toggle(option.value, in: item)
                    }
                }
            }
            if allowsOther { QuestionOtherRow(question: question, item: item) }
        case .text(let secure, _):
            QuestionFieldControl(question: question, item: item, secure: secure, prompt: "Answer")
        case .number(let integer, let minimum, let maximum, _):
            QuestionFieldControl(
                question: question,
                item: item,
                secure: false,
                prompt: Self.numberPrompt(integer: integer, minimum: minimum, maximum: maximum)
            )
        case .boolean:
            Picker(item.prompt, selection: toggle) {
                Text("Yes").tag(Optional(true))
                Text("No").tag(Optional(false))
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .fixedSize()
        }
    }

    private var singleSelection: Binding<String?> {
        Binding(
            get: { question.selections[item.id]?.first },
            set: { value in
                if let value { question.toggle(value, in: item) }
            }
        )
    }

    private var toggle: Binding<Bool?> {
        Binding(
            get: { question.toggles[item.id] },
            set: { question.toggles[item.id] = $0 }
        )
    }

    private static func numberPrompt(integer: Bool, minimum: Double?, maximum: Double?) -> String {
        let noun = integer ? "Whole number" : "Number"
        let format = ChatPendingQuestion.format
        switch (minimum, maximum) {
        case let (minimum?, maximum?): return "\(noun), \(format(minimum))–\(format(maximum))"
        case let (minimum?, nil): return "\(noun), \(format(minimum)) or more"
        case let (nil, maximum?): return "\(noun), up to \(format(maximum))"
        case (nil, nil): return noun
        }
    }
}

/// An option: its number key, label and description. Selection uses the
/// system tint; a multi-select row reads as the checkbox it is.
private struct QuestionOptionRow: View {
    let option: AgentQuestionOption
    let number: Int
    let multiple: Bool
    let isSelected: Bool
    let onToggle: () -> Void

    var body: some View {
        Button(action: onToggle) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: symbol)
                    .foregroundStyle(isSelected ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(option.label)
                        .appFont(.body)
                    if let description = option.description, !description.isEmpty {
                        Text(description)
                            .appFont(.callout)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
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
        .modifier(CheckboxRepresentation(enabled: multiple, label: label, isOn: isSelected, onToggle: onToggle))
    }

    private var symbol: String {
        switch (multiple, isSelected) {
        case (true, true): "checkmark.square.fill"
        case (true, false): "square"
        case (false, true): "largecircle.fill.circle"
        case (false, false): "circle"
        }
    }

    private var label: String {
        [option.label, option.description].compactMap { $0 }.joined(separator: ", ")
    }
}

private struct CheckboxRepresentation: ViewModifier {
    let enabled: Bool
    let label: String
    let isOn: Bool
    let onToggle: () -> Void

    func body(content: Content) -> some View {
        if enabled {
            content.accessibilityRepresentation {
                Toggle(label, isOn: Binding(get: { isOn }, set: { _ in onToggle() }))
                    .toggleStyle(.checkbox)
            }
        } else {
            content
        }
    }
}

/// A select's free-text answer.
private struct QuestionOtherRow: View {
    @Bindable var question: ChatPendingQuestion
    let item: AgentQuestion

    var body: some View {
        TextField("Other", text: question.textBinding(item.id), prompt: Text("Other answer"))
            .modifier(QuestionFieldChrome())
            .padding(.top, 4)
            .accessibilityLabel("Other answer")
            .accessibilityIdentifier(AccessibilityID.chatQuestionOther)
    }
}

/// A text or number answer; a secret one never shows what was typed.
private struct QuestionFieldControl: View {
    @Bindable var question: ChatPendingQuestion
    let item: AgentQuestion
    let secure: Bool
    let prompt: String

    var body: some View {
        Group {
            if secure {
                SecureField(item.prompt, text: question.textBinding(item.id), prompt: Text(prompt))
            } else {
                TextField(item.prompt, text: question.textBinding(item.id), prompt: Text(prompt))
            }
        }
        .modifier(QuestionFieldChrome())
        .labelsHidden()
    }
}

/// A field drawn by SwiftUI on the panel's glass. The system rounded border
/// is translucent AppKit chrome: while the panel animates it is flattened
/// with no glass behind it to blend with, and flashes black.
private struct QuestionFieldChrome: ViewModifier {
    @FocusState private var isFocused: Bool

    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: 6, style: .continuous)
        content
            .textFieldStyle(.plain)
            .focused($isFocused)
            .padding(.horizontal, 7)
            .padding(.vertical, 5)
            // No focus ring, as in the composer: the caret marks focus, and
            // the fill brightens a step so a Tab landing here still shows.
            .background(isFocused ? AnyShapeStyle(.tertiary) : AnyShapeStyle(.quaternary), in: shape)
    }
}

extension AgentQuestion {
    /// The options of a select, in order; empty for any other kind.
    var options: [AgentQuestionOption] {
        switch kind {
        case .singleSelect(let options, _, _), .multiSelect(let options, _, _, _, _): options
        default: []
        }
    }
}
