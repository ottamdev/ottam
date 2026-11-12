import Foundation
import ScenarioKit

public extension UIScenarioDefinition {
    static let captureOcclusionProof = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "capture.occlusion-proof",
            description: "Prove driver-owned ScreenCaptureKit capture is window-isolated.",
            tags: ["capture", "platform-contract"],
            requirements: ["Screen Recording"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat"
        )
    ) { context in
        try await context.proveOcclusionIsolation()
    }
}
