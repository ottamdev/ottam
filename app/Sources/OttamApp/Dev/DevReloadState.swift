#if DEBUG
import Foundation
import Observation
import SwiftUI

@MainActor
@Observable
final class DevReloadState {
    enum Phase: Equatable {
        case idle
        case reloading
        case failed(String)
    }

    var phase: Phase = .idle
    private var failureID: UUID?

    var isReloading: Bool {
        phase == .reloading
    }

    @discardableResult
    func beginReloading() -> Bool {
        guard !isReloading else {
            return false
        }

        failureID = nil
        phase = .reloading
        return true
    }

    func fail(_ message: String) {
        let id = UUID()
        failureID = id
        phase = .failed(message)

        Task { @MainActor in
            try? await Task.sleep(for: .seconds(4))
            guard failureID == id else {
                return
            }

            phase = .idle
            failureID = nil
        }
    }
}

extension View {
    /// Rebuild progress over the workspace, from Debug › Rebuild and Relaunch.
    func devReloadOverlay(_ state: DevReloadState) -> some View {
        overlay {
            if state.phase != .idle {
                DevReloadOverlay(phase: state.phase)
                    .transition(.opacity)
                    .allowsHitTesting(false)
            }
        }
        .animation(.easeInOut(duration: 0.16), value: state.phase)
    }
}

private struct DevReloadOverlay: View {
    let phase: DevReloadState.Phase

    var body: some View {
        HStack(spacing: 10) {
            switch phase {
            case .idle:
                EmptyView()
            case .reloading:
                ProgressView()
                    .controlSize(.small)
                Text("Reloading...")
                    .font(.callout.weight(.medium))
            case .failed(let message):
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.yellow)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Reload failed")
                        .font(.headline)
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
        .shadow(color: .black.opacity(0.18), radius: 18, y: 8)
        .frame(maxWidth: 360)
    }
}
#endif
