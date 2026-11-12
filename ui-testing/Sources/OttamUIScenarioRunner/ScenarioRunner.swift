import AppKit
import OttamAccessibility
import OttamUIScenarios
import Foundation
import MacAutomationKit
import ScenarioKit

@main
enum OttamUIScenarioRunner {
    static func main() async {
        do {
            let arguments = RunnerArguments(CommandLine.arguments)
            switch arguments.command {
            case "list":
                for scenario in ScenarioRegistry.scenarios {
                    let descriptor = scenario.descriptor
                    print(
                        "\(descriptor.id)\t\(descriptor.description)\t"
                            + "tags=\(descriptor.tags.joined(separator: ","))\t"
                            + "requires=\(descriptor.requirements.joined(separator: ","))"
                    )
                }
            case "preflight", "request-permissions":
                let recorder = ActionRecorder(
                    url: URL(fileURLWithPath: arguments.root)
                        .appendingPathComponent(".build/ui-testing/runtime/preflight-actions.jsonl")
                )
                try FileManager.default.createDirectory(
                    at: URL(fileURLWithPath: arguments.root)
                        .appendingPathComponent(".build/ui-testing/runtime"),
                    withIntermediateDirectories: true
                )
                let client = DriverClient(
                    driverBundleURL: URL(fileURLWithPath: arguments.driverApp),
                    requestsDirectory: URL(fileURLWithPath: arguments.root)
                        .appendingPathComponent(".build/ui-testing/runtime"),
                    topology: arguments.driverTopology,
                    actionRecorder: recorder
                )
                let response = try client.invoke(command: arguments.command)
                if response.status != "passed" {
                    try printJSON(response.raw)
                    exit(69)
                }
                let preflightUnavailable = arguments.command == "preflight"
                    && (response.result["accessibilityAllowed"] as? Bool != true
                    || response.result["screenRecordingAllowed"] as? Bool != true
                    || response.result["screenLocked"] as? Bool != false
                    || response.result["onConsole"] as? Bool != true)
                if preflightUnavailable {
                    try printJSON([
                        "schemaVersion": 1,
                        "command": "preflight",
                        "status": "environment_unavailable",
                        "message":
                            "An unlocked console session and driver permissions are required.",
                        "result": response.result,
                    ])
                    FileHandle.standardError.write(
                        Data(
                            """
                            environment_unavailable: unlock the console session and enable the \
                            stable Ottam UI Test Driver in Accessibility and Screen & System \
                            Audio Recording, then relaunch it.\n
                            """.utf8
                        )
                    )
                    exit(69)
                }
                try printJSON(response.raw)
            case "smoke":
                let result = await ScenarioExecution(arguments: arguments)
                    .run(ScenarioRegistry.smoke)
                print(result.runDirectory)
                exit(result.exitCode)
            case "run":
                guard let scenarioID = arguments.scenarioID,
                      let scenario = ScenarioRegistry.definition(id: scenarioID)
                else {
                    throw RunnerError.unknownScenario(arguments.scenarioID ?? "")
                }
                let result = await ScenarioExecution(arguments: arguments).run(scenario)
                print(result.runDirectory)
                exit(result.exitCode)
            default:
                throw RunnerError.usage
            }
        } catch {
            FileHandle.standardError.write(Data("scenario runner failed: \(error)\n".utf8))
            exit(64)
        }
    }

    private static func printJSON(_ value: Any) throws {
        let data = try JSONSerialization.data(
            withJSONObject: value,
            options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        )
        FileHandle.standardOutput.write(data + Data([0x0A]))
    }
}
