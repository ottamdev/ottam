import Foundation
import OttamBackendClient

/// What first-launch onboarding has done in this home.
struct OnboardingRecord: Codable, Equatable {
    enum Status: String, Codable {
        case inProgress
        case complete
    }

    var status: Status
    /// Harness IDs the user chose, in the order the picker lists them.
    var selectedHarnessIDs: [String] = []
    /// Of those, the ones using the CLI's own setup instead of an Ottam profile.
    var cliSetupHarnessIDs: [String] = []

    static let complete = OnboardingRecord(status: .complete)
}

/// Owns `$OTTAM_HOME/onboarding.json`. The app writes it, like
/// `app-session.json`, so launch can decide which window opens without a core
/// round-trip.
struct OnboardingStore {
    var directory: URL

    var fileURL: URL { directory.appendingPathComponent("onboarding.json") }
    /// Core's database: its presence without our file means the home predates
    /// onboarding.
    var databaseURL: URL { directory.appendingPathComponent("ottam.sqlite") }

    static func `default`(environment: [String: String] = ProcessInfo.processInfo.environment) -> OnboardingStore {
        OnboardingStore(directory: OttamPaths.home(environment: environment))
    }

    func load() -> OnboardingRecord? {
        guard let data = try? Data(contentsOf: fileURL) else { return nil }
        return try? JSONDecoder().decode(OnboardingRecord.self, from: data)
    }

    func save(_ record: OnboardingRecord) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(record).write(to: fileURL, options: .atomic)
    }

    /// Whether this launch opens the Onboarding window instead of the Welcome
    /// window. `OTTAM_ONBOARDING=1` forces it and `0` skips it, for
    /// development and UI tests; UI test runs skip it unless they force it, so
    /// scenarios with fresh homes keep landing on the window they capture.
    func launchNeedsOnboarding(environment: [String: String] = ProcessInfo.processInfo.environment) -> Bool {
        switch environment["OTTAM_ONBOARDING"] {
        case "1": return true
        case "0": return false
        default: break
        }
        if environment["OTTAM_UI_TEST_MODE"] == "1" { return false }
        if let record = load() { return record.status != .complete }
        if FileManager.default.fileExists(atPath: databaseURL.path) {
            // A home from before onboarding existed: record it so a later
            // deletion of the database never brings onboarding back.
            try? save(.complete)
            return false
        }
        // Recorded now: core creates the database right after launch, so a
        // quit before the first Continue must not read as a legacy home.
        try? save(OnboardingRecord(status: .inProgress))
        return true
    }
}
