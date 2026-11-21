import LocalStudioKit
import SwiftUI

struct QuestionCard: View {
    let store: StudioStore

    var body: some View {
        let questions = store.conversation.questions
        if let pending = questions.first {
            QuestionCardBody(conversation: store.conversation, pending: pending, count: questions.count)
                .id(pending.id)
        }
    }
}

private struct QuestionCardBody: View {
    let conversation: ConversationStore
    let pending: PluginQuestion
    let count: Int
    @State private var typed = ""

    var body: some View {
        let question = pending.question
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Image(systemName: "puzzlepiece.extension.fill")
                    .icon(.inline)
                    .accessibilityHidden(true)
                Text(question.header)
                    .font(.system(size: 12.5, weight: .semibold))
                Text("asks")
                    .font(.system(size: 12.5))
                    .foregroundStyle(Theme.secondaryText)
                Spacer(minLength: 8)
                if count > 1 {
                    Text("1/\(count)")
                        .font(.system(size: 12))
                        .monospacedDigit()
                        .foregroundStyle(Theme.secondaryText)
                        .help("\(count) questions are waiting")
                }
            }
            .foregroundStyle(Theme.primary)
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Plugin \(question.header) asks")
            Text(question.question)
                .font(.system(size: 13))
                .lineSpacing(2)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
            if !question.options.isEmpty {
                FlowChoices(options: question.options) { label in
                    conversation.answer(pending, with: label)
                }
            }
            HStack(spacing: 6) {
                TextField(question.options.isEmpty ? "Type your answer" : "Or type another answer", text: $typed)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { send() }
                    .accessibilityLabel("Answer for \(question.header)")
                Button("Answer") { send() }
                    .controlSize(.regular)
                    .disabled(typed.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .help("Send the typed answer to \(question.header)")
            }
        }
        .padding(12)
        .background(Theme.card, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(Theme.primary.opacity(0.3)))
    }

    private func send() {
        guard !typed.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        conversation.answer(pending, with: typed)
    }
}

private struct FlowChoices: View {
    let options: [QuestionOption]
    let choose: (String) -> Void

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 6) {
                buttons
                Spacer(minLength: 0)
            }
            VStack(alignment: .leading, spacing: 6) {
                buttons
            }
        }
    }

    private var buttons: some View {
        ForEach(options, id: \.label) { option in
            Button(option.label) { choose(option.label) }
                .controlSize(.regular)
                .help(option.description.isEmpty ? "Answer \(option.label)" : option.description)
        }
    }
}
