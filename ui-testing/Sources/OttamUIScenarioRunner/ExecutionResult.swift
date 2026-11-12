import ScenarioKit

struct ExecutionResult {
    let runDirectory: String
    let exitCode: Int32
    let classification: RunClassification
    let cleanupVerified: Bool
}
