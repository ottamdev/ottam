import Foundation
import Testing
@testable import OttamApp

@Suite("Typography settings")
@MainActor
struct TypographySettingsTests {
    private func defaults(_ function: String = #function) -> UserDefaults {
        let suite = "typography-tests.\(function).\(UUID().uuidString)"
        return UserDefaults(suiteName: suite) ?? .standard
    }

    @Test("payload viewport height follows the selected code font size")
    func payloadViewportTracksCodeSize() {
        let settings = TypographySettings(defaults: defaults())
        let original = settings.resolved.nsFont(.code)
        let originalHeight = ChatPayloadTextView.height(forLines: 2, font: original)
        settings.codeSizeDelta = 6
        let larger = settings.resolved.nsFont(.code)
        #expect(larger.pointSize == original.pointSize + 6)
        #expect(ChatPayloadTextView.height(forLines: 2, font: larger) > originalHeight)
    }

    @Test("payload viewport fits wrapped text up to the requested line cap")
    func payloadViewportWrapsAndCaps() {
        let font = AppTypography.Resolution.builtIn.nsFont(.code)
        let text = String(repeating: "wrapped reasoning ", count: 80)
        let oneLine = ChatPayloadTextView.height(for: "One line", width: 400, font: font, maximumLines: 20)
        let wide = ChatPayloadTextView.height(for: text, width: 700, font: font, maximumLines: 20)
        let narrow = ChatPayloadTextView.height(for: text, width: 180, font: font, maximumLines: 20)
        let outputCap = ChatPayloadTextView.height(forLines: 20, font: font)
        let inputCap = ChatPayloadTextView.height(for: text, width: 180, font: font, maximumLines: 10)

        #expect(oneLine == ChatPayloadTextView.height(forLines: 1, font: font))
        #expect(ChatPayloadTextView.height(for: "One\nTwo", width: 400, font: font, maximumLines: 20)
            == ChatPayloadTextView.height(forLines: 2, font: font))
        #expect(wide > oneLine && wide < outputCap)
        #expect(narrow == outputCap)
        #expect(inputCap == ChatPayloadTextView.height(forLines: 10, font: font))
        #expect(inputCap < outputCap)
    }

    @Test("text defaults to the system font, code to the bundled family")
    func defaultsDifferByRole() {
        let settings = TypographySettings(defaults: defaults())

        #expect(settings.uiFamily == nil)
        #expect(settings.resolved.ui == .system)
        #expect(settings.codeFamily == AppTypography.builtInFamilyName)
    }

    /// The sentinel exists so that choosing the system font is distinguishable
    /// from never having chosen — which only bites the role whose default is a
    /// real family.
    @Test("choosing the system font for code survives a reload")
    func systemChoiceForCodePersists() {
        let store = defaults()
        let settings = TypographySettings(defaults: store)

        settings.codeFamily = nil

        #expect(TypographySettings(defaults: store).codeFamily == nil)
    }

    @Test("a chosen text family survives a reload")
    func chosenTextFamilyPersists() {
        let store = defaults()
        let settings = TypographySettings(defaults: store)

        settings.uiFamily = AppTypography.builtInFamilyName

        let reloaded = TypographySettings(defaults: store)
        #expect(reloaded.uiFamily == AppTypography.builtInFamilyName)
    }
}
