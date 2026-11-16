import Foundation
import Sparkle
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

    /// A check that finds nothing is not an error worth logging as one.
    @Test func noUpdateIsNotAnAbort() {
        #expect(UpdaterEventDelegate.isNoUpdate(NSError(domain: SUSparkleErrorDomain, code: 1001)))
        #expect(!UpdaterEventDelegate.isNoUpdate(NSError(domain: SUSparkleErrorDomain, code: 2001)))
        #expect(!UpdaterEventDelegate.isNoUpdate(NSError(domain: NSURLErrorDomain, code: 1001)))
    }

    /// A scheduled check leaves the update to the button only while a sidebar
    /// can show it.
    @Test func scheduledUpdatesWaitOnlyWhenASidebarIsShown() {
        let reminders = UpdateReminderDelegate()
        #expect(reminders.supportsGentleScheduledUpdateReminders)
        #expect(reminders.standardUserDriverShouldHandleShowingScheduledUpdate(.empty(), andInImmediateFocus: true))
        reminders.canRemind = { true }
        #expect(!reminders.standardUserDriverShouldHandleShowingScheduledUpdate(.empty(), andInImmediateFocus: true))
    }

    /// The button outlives looking at the update, Remind Me Later and a
    /// closed window; Skip This Version or a check that finds nothing clears it.
    @Test func updateStaysAvailableUntilSkippedOrGone() {
        let events = UpdaterEventDelegate()
        // Optional requirements: a misspelled method compiles and is never called.
        #expect(events.responds(to: #selector(SPUUpdaterDelegate.updater(_:userDidMake:forUpdate:state:))))
        #expect(events.responds(to: #selector(SPUUpdaterDelegate.updaterDidNotFindUpdate(_:error:))))
        var available: [String?] = []
        events.onAvailabilityChange = { available.append($0) }

        events.found(version: "26.11.2")
        events.chose(.dismiss)
        events.chose(.install)
        #expect(available == ["26.11.2"])
        events.chose(.skip)
        #expect(available == ["26.11.2", nil])
        events.found(version: "26.11.3")
        events.notFound()
        #expect(available == ["26.11.2", nil, "26.11.3", nil])
    }

    /// A critical update never waits behind the button.
    @Test func criticalUpdatesAlwaysShowSparklesWindow() {
        #expect(UpdateReminderDelegate.reminds(critical: false, canRemind: true))
        #expect(!UpdateReminderDelegate.reminds(critical: true, canRemind: true))
        #expect(!UpdateReminderDelegate.reminds(critical: false, canRemind: false))
    }
}
