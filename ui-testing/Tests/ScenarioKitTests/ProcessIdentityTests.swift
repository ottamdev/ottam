import UIHarnessCore
import XCTest

final class ProcessIdentityTests: XCTestCase {
    func testCurrentIdentityRoundTripsAndRejectsPIDReuseShape() throws {
        let current = try XCTUnwrap(HarnessProcessIdentity.current())
        XCTAssertEqual(
            HarnessProcessIdentity.forPID(current.pid),
            current
        )
        let mismatched = HarnessProcessIdentity(
            pid: current.pid,
            startSeconds: current.startSeconds,
            startMicroseconds: current.startMicroseconds &+ 1
        )
        XCTAssertNotEqual(
            HarnessProcessIdentity.forPID(current.pid),
            mismatched
        )
    }

    func testExactExecutableEnumerationIncludesCurrentProcess() throws {
        let current = try XCTUnwrap(HarnessProcessIdentity.current())
        let executablePath = try XCTUnwrap(current.executablePath)
        XCTAssertTrue(
            HarnessProcessIdentity.running(
                executableURL: URL(fileURLWithPath: executablePath)
            ).contains(current)
        )
    }
}
