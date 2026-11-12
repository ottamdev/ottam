public struct UIScenarioRegistry {
    public let scenarios: [UIScenarioDefinition]

    public init(_ scenarios: [UIScenarioDefinition]) {
        self.scenarios = scenarios
    }

    public func definition(id: String) -> UIScenarioDefinition? {
        scenarios.first { $0.descriptor.id == id }
    }
}
