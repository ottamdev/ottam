import SwiftUI

/// Tool input and its result share one surface, with independent selection,
/// scrolling, and copy actions. The card's store owns both readers.
struct ChatPayloadDetailView: View {
    @Bindable var store: ChatPayloadDetailStore

    private var accessibilityStatus: String {
        store.loading ? "loading" : store.failure != nil ? "failed" : store.payload?.locallyTruncated == true ? "partial" : store.unsaved ? "unsaved" : "available"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let input = store.input {
                ChatPayloadSectionView(store: input, isInput: true)
                Divider()
                    .padding(.horizontal, 12)
            }
            ChatPayloadSectionView(store: store)
        }
        .workspaceCardSurface()
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("chat.payload.detail")
        .accessibilityLabel("Tool details, \(accessibilityStatus)")
    }
}

struct ChatToolInputPresentation {
    let text: String
    let isCommand: Bool

    init(text: String, kind: String?) {
        if kind == "execute",
           let object = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any],
           let command = (object["command"] as? String) ?? (object["cmd"] as? String) {
            self.text = command
            isCommand = true
        } else {
            self.text = text
            isCommand = false
        }
    }
}

private struct ChatPayloadSectionView: View {
    @Environment(\.appTypography) private var typography
    @Bindable var store: ChatPayloadDetailStore
    var isInput = false
    @State private var copyTask: Task<Void, Never>?
    @State private var showsCopied = false
    @State private var copyFailure: String?

    private var input: ChatToolInputPresentation {
        ChatToolInputPresentation(text: store.text, kind: store.activity?.kind)
    }
    private var title: String {
        if isInput { return input.isCommand ? "Command" : "Arguments" }
        return store.sections.first { $0.0 == store.section }?.1 ?? "Output"
    }
    private var displayText: String { isInput ? input.text : store.text }
    private var isImage: Bool {
        guard let payload = store.payload, payload.parts.indices.contains(store.part) else { return false }
        return payload.parts[store.part].format.hasPrefix("image/")
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                if !isInput, store.sections.count > 1 {
                    Picker("Details", selection: Binding(get: { store.section }, set: { store.select($0) })) {
                        ForEach(store.sections, id: \.0) { section in Text(section.1).tag(section.0) }
                    }
                    .labelsHidden()
                    .fixedSize()
                } else {
                    Text(title)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                if store.unsaved { Text("Unsaved").font(.caption).foregroundStyle(.secondary) }
                if store.loading {
                    ProgressView().controlSize(.mini).frame(width: 16, height: 16)
                        .accessibilityLabel("Loading \(title.lowercased())")
                }
                Spacer(minLength: 8)
                if !isImage { copyButton }
            }
            .padding(.horizontal, 12)
            if !isInput, let payload = store.payload, payload.parts.count > 1 {
                Picker("Content", selection: Binding(get: { store.part }, set: { store.select(store.section, part: $0) })) {
                    if store.hasDiff { Text("Diff").tag(-1) }
                    ForEach(Array(payload.parts.enumerated()), id: \.offset) { index, part in
                        Text(part.path.map { "\(part.name) — \(($0 as NSString).lastPathComponent)" } ?? part.name).tag(index)
                    }
                }
                .padding(.horizontal, 12)
            }
            if store.payload?.upstreamTruncated == true {
                Text("The agent supplied truncated output").font(.caption).foregroundStyle(.secondary)
                    .padding(.horizontal, 12)
            }
            if let image = store.image {
                ScrollView {
                    Image(nsImage: image).resizable().scaledToFit().frame(maxHeight: 240)
                        .accessibilityLabel("Captured image").accessibilityIdentifier("chat.payload.image")
                        .padding(8)
                }
                .frame(height: 260)
            } else {
                if store.startOffset > 0 { Button("Load earlier content") { store.previousPage() }.padding(.horizontal, 12) }
                if !displayText.isEmpty {
                    ChatPayloadTextView(text: displayText, font: typography.nsFont(.code),
                        maximumLines: isInput ? ChatPayloadTextView.inputMaximumLines : ChatPayloadTextView.outputMaximumLines,
                        followsEnd: !isInput && store.followsEnd, unsaved: store.unsaved,
                        onScroll: { store.followsEnd = $0 }, onNextPage: { store.nextPage() },
                        accessibilityID: isInput ? "chat.payload.input.text" : "chat.payload.text")
                        .padding(.horizontal, 4)
                }
                if let failure = store.failure {
                    HStack {
                        Text(failure).foregroundStyle(.secondary)
                        Button("Retry") { store.retry() }
                    }
                    .padding(.horizontal, 12)
                }
                if store.nextOffset != nil { Button("Load more") { store.nextPage() }.padding(.horizontal, 12) }
            }
        }
        .padding(.vertical, 8)
        .onDisappear { copyTask?.cancel() }
        .alert("Couldn't copy \(title.lowercased())", isPresented: Binding(
            get: { copyFailure != nil }, set: { if !$0 { copyFailure = nil } }
        )) {
            Button("OK", role: .cancel) { copyFailure = nil }
        } message: {
            Text(copyFailure ?? "")
        }
    }

    private var copyButton: some View {
        Button {
            copyFailure = nil
            copyTask = Task {
                defer { copyTask = nil; showsCopied = false }
                do {
                    let text = try await store.textForCopy()
                    try Task.checkCancellation()
                    WorkspacePasteboard.copy(text)
                    showsCopied = true
                    try await Task.sleep(for: .seconds(1.5))
                } catch is CancellationError { }
                catch { copyFailure = error.localizedDescription }
            }
        } label: {
            Image(systemName: showsCopied ? "checkmark" : "square.on.square")
                .font(.system(size: 12))
                .contentTransition(.symbolEffect(.replace))
                .frame(width: 22, height: 22)
        }
        .buttonStyle(.borderless)
        .foregroundStyle(.secondary)
        .pointerStyle(.link)
        .disabled(store.data.isEmpty || store.failure != nil || copyTask != nil)
        .help("Copy \(title.lowercased())")
        .accessibilityLabel("Copy \(title.lowercased())")
        .accessibilityValue(showsCopied ? "copied" : "")
        .accessibilityIdentifier(isInput ? "chat.payload.copy.input" : "chat.payload.copy.output")
    }
}
