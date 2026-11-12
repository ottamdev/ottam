import AppKit
import OttamWorkspaceLogic
import SwiftUI

/// The agents pill's face: one circle per agent working on the task,
/// overlapping left to right, the parent's brand first. Each circle is cut
/// back where the next one covers it, so the faces stay apart on any
/// toolbar material without a ring in a fixed color.
struct AgentStackView: View {
    let members: [ConversationAgentGroup.Member]
    var size: CGFloat = 18

    /// More members than this show one fewer face and a "+N" count.
    static let maximumFaces = 4

    private var overlap: CGFloat { size * 0.22 }
    private var gap: CGFloat { max(1, size * 0.08) }

    private enum Face {
        case member(ConversationAgentGroup.Member)
        case more(Int)
    }

    private var faces: [Face] {
        guard members.count > Self.maximumFaces else { return members.map(Face.member) }
        let shown = Self.maximumFaces - 1
        return members.prefix(shown).map(Face.member) + [.more(members.count - shown)]
    }

    var body: some View {
        let faces = faces
        HStack(spacing: -overlap) {
            ForEach(faces.indices, id: \.self) { index in
                face(faces[index])
                    .mask {
                        Rectangle()
                            .overlay {
                                if index < faces.count - 1 {
                                    Circle()
                                        .frame(width: size + gap * 2, height: size + gap * 2)
                                        .offset(x: size - overlap)
                                        .blendMode(.destinationOut)
                                }
                            }
                            .compositingGroup()
                    }
            }
        }
        .accessibilityHidden(true)
    }

    @ViewBuilder
    private func face(_ face: Face) -> some View {
        switch face {
        case .member(let member):
            switch member.icon {
            case .brand(let brand):
                AgentIconView(brand: brand, name: member.agentName, size: size, round: true)
            case .avatar(let avatar):
                AgentAvatarView(avatar: avatar, size: size, round: true)
            }
        case .more(let count):
            Text("+\(count)")
                .font(.system(size: size * 0.42, weight: .bold, design: .rounded))
                .foregroundStyle(.white)
                .frame(width: size, height: size)
                .background(Circle().fill(Color.gray.gradient))
        }
    }
}

extension AgentStackView {
    /// The stack as an image, for the toolbar item. It is described by
    /// `description` for VoiceOver.
    @MainActor
    static func image(_ members: [ConversationAgentGroup.Member], description: String) -> NSImage {
        let stack = AgentStackView(members: members)
        let count = CGFloat(stack.faces.count)
        let width = stack.size * count - stack.overlap * max(0, count - 1)
        return appKitImage(size: CGSize(width: width, height: stack.size), description: description) { stack }
    }
}
