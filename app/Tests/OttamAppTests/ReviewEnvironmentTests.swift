import Foundation
import Testing
@testable import OttamApp

@Suite("Review environment")
struct ReviewEnvironmentTests {
    @Test("an Xcode launch finds its checkout through DerivedData's info.plist")
    func sourceCheckoutFromDerivedData() throws {
        let fileManager = FileManager.default
        let root = fileManager.temporaryDirectory
            .appendingPathComponent("ReviewEnvironmentTests-\(UUID().uuidString)")
            .resolvingSymlinksInPath()
        defer { try? fileManager.removeItem(at: root) }

        let checkout = root.appendingPathComponent("checkout")
        for file in ["app/Package.swift", "backend/Cargo.toml"] {
            let url = checkout.appendingPathComponent(file)
            try fileManager.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try Data().write(to: url)
        }
        let derivedData = root.appendingPathComponent("DerivedData/app-abc")
        try fileManager.createDirectory(at: derivedData, withIntermediateDirectories: true)
        let info = ["WorkspacePath": checkout.appendingPathComponent("app").path] as NSDictionary
        try info.write(to: derivedData.appendingPathComponent("info.plist"))

        let executable = derivedData.appendingPathComponent("Build/Products/Release/OttamApp")
        #expect(ReviewEnvironment.sourceCheckoutRepoRoot(executableURL: executable) == checkout.path)

        // A DerivedData folder without the record, or not a DerivedData launch at all.
        let bare = root.appendingPathComponent("DerivedData/other/Build/Products/Debug/OttamApp")
        #expect(ReviewEnvironment.sourceCheckoutRepoRoot(executableURL: bare) == nil)
        #expect(ReviewEnvironment.sourceCheckoutRepoRoot(executableURL: checkout.appendingPathComponent("OttamApp")) == nil)
    }
}
