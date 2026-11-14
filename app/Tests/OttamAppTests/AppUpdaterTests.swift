import Foundation
import Testing
@testable import OttamApp

/// Development, `run-app.sh` and UI-test bundles carry no feed, so they must
/// never start Sparkle. The positive case starts a real updater and is
/// covered by the release build's Try it instead.
@Suite("App updater")
@MainActor
struct AppUpdaterTests {
    private func bundle(info: [String: Any]) throws -> Bundle {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("AppUpdaterTests-\(UUID().uuidString).app")
        let contents = root.appendingPathComponent("Contents")
        try FileManager.default.createDirectory(at: contents, withIntermediateDirectories: true)
        var plist = info
        plist["CFBundleIdentifier"] = "dev.ottam.AppUpdaterTests"
        let data = try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0)
        try data.write(to: contents.appendingPathComponent("Info.plist"))
        return try #require(Bundle(url: root))
    }

    @Test func noFeedMeansNoUpdater() throws {
        #expect(AppUpdater.makeIfConfigured(bundle: try bundle(info: [:])) == nil)
    }

    @Test func emptyFeedMeansNoUpdater() throws {
        #expect(AppUpdater.makeIfConfigured(bundle: try bundle(info: ["SUFeedURL": ""])) == nil)
    }

    @Test func developmentBundleHasNoUpdater() {
        #expect(AppUpdater.makeIfConfigured() == nil)
    }
}
