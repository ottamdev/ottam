import ScenarioKit

public enum ScenarioRegistry {
    public static let smoke = UIScenarioDefinition.smoke

    private static let registered = UIScenarioRegistry(
        [
            UIScenarioDefinition.workspaceCoreReplacement,
            .coreRecoveryPresentation,
            .chooserCoreRecovery,
            .chooserStartupError,
            .chatInterruptAndContinue,
            .chatNativeSteerAndStop,
            .chatQueueDrain,
            .chatToolsPane,
            .chatDeliveryRetry,
            .chatRunFailure,
            .chatTranscriptHistoryRecovery,
            .chatSwitchKeepsPlace,
            .chatDeliveryMarkDelivered,
            .chatPendingRetryResume,
            .chatAuthRecovery,
            .chatPermissionPrompt,
            .chatQuestionPanel,
            .chatNoUsableEngine,
            .chatEngineChecking,
            .settingsAgentCompatibility,
            .settingsAgentUpdate,
            .settingsExistingSetups,
            .settingsProfiles,
            .settingsProfileDeleteProbe,
            .chatTranscriptLiveLight,
            .chatTranscriptLiveDark,
            .chatTranscriptComplete,
            .chatTranscriptStreaming,
            .chatTranscriptNormalized,
            .chatTranscriptPayloads,
            .chatSessionSignals,
            .chatBackgroundTasks,
            .chatUsage,
            .chatSubagentThreads,
            .chatNativeSubagentThreads,
            .chatTranscriptCaptureRecovery,
            .chatTranscriptAppKitNative,
            .chatCodeBlocks,
            .chatTables,
            .transcriptMeasureMultiTurn,
            .transcriptMeasureSingleReply,
            .transcriptMeasureDetached,
            .transcriptMeasureIdleTool,
            .chatMeasureSwitchCacheMemory,
            .transcriptUpdatingTools,
            .captureOcclusionProof,
            .driverReadinessDelay,
            .driverForcedFailure,
            .workspaceZeroProjectShapeLight,
            .workspaceZeroProjectShapeDark,
            .workspaceSingleProjectShapeLight,
            .workspaceSingleProjectShapeDark,
            .workspaceMultiProjectShapeLight,
            .workspaceMultiProjectShapeDark,
            .workspaceTwoWindowIdentityCapture,
            .markdownEngineLabKitchenSinkLight,
            .markdownEngineLabKitchenSinkDark,
            .markdownEngineLabCodeHeavyLight,
            .markdownEngineLabCodeHeavyDark,
            .markdownEngineLabTableHeavyLight,
            .markdownEngineLabTableHeavyDark,
            .markdownEngineLabListsAndQuotesLight,
            .markdownEngineLabListsAndQuotesDark,
            .markdownEngineLabImagesLight,
            .markdownEngineLabImagesDark,
            .markdownEngineLabUserInlineLight,
            .markdownEngineLabUserInlineDark,
            .markdownEngineLabCards,
        ]
    )

    public static var scenarios: [UIScenarioDefinition] {
        registered.scenarios
    }

    public static func definition(id: String) -> UIScenarioDefinition? {
        registered.definition(id: id)
    }

    public static func descriptor(id: String) -> ScenarioDescriptor? {
        definition(id: id)?.descriptor
    }
}
