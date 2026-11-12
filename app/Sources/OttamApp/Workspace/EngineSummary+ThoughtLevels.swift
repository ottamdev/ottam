import OttamBackendClient

extension EngineSummary {
    /// The effort levels, and the level the agent runs without a pick, for a
    /// model choice (`nil` means the engine's default model).
    ///
    /// Agents tie both to the model: omp applies each model's own default
    /// level, and Claude and OpenCode re-list (or drop) levels per model. A
    /// model a session has run on carries what the agent reported for it.
    /// Otherwise the engine-wide select, observed on the default model, holds
    /// for that model only; for any other model its levels are a best guess
    /// and the default is unknown.
    func thoughtLevelSelect(forModel modelID: String?) -> (levels: [EngineSelectOption], defaultID: String?) {
        let effectiveID = modelID ?? defaultModelID
        if let model = models.first(where: { $0.modelID == effectiveID }),
           let levels = model.thoughtLevels {
            return (levels, model.defaultThoughtLevelID)
        }
        let isDefaultModel = modelID == nil || modelID == defaultModelID
        return (thoughtLevels, isDefaultModel ? defaultThoughtLevelID : nil)
    }
}
