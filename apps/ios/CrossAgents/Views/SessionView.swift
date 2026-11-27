import PhotosUI
import SwiftUI
import UIKit

struct SessionView: View {
    @Environment(AppStore.self) private var store
    let agentId: String

    @State private var draft = ""
    @State private var pickerItems: [PhotosPickerItem] = []
    @State private var selectedImages: [UIImage] = []
    @State private var isLoading = false
    @State private var isSending = false
    @State private var errorMessage: String?
    @State private var showingModelSheet = false
    @State private var previewImage: UIImage?

    private var transcript: Transcript? {
        store.transcripts[agentId]
    }

    private var agent: Agent? {
        transcript?.agent ?? store.agent(by: agentId)
    }

    private var pendingQuestions: [HumanItem] {
        store.openQuestions(for: agentId)
    }

    private var isRunning: Bool {
        if let delivery = transcript?.delivery, delivery.isBusy { return true }
        return agent?.status == .running || agent?.status == .starting
    }

    private var scrollToken: String {
        guard let turns = transcript?.turns else { return "0" }
        let last = turns.last
        let itemCount = last?.items.count ?? 0
        var lastLen = 0
        if let lastItem = last?.items.last {
            switch lastItem {
            case .output(_, let text): lastLen = text.count
            case .toolCall(let tc): lastLen = (tc.output?.count ?? 0) + tc.status.count
            default: lastLen = 1
            }
        }
        return "\(turns.count)-\(itemCount)-\(lastLen)"
    }

    var body: some View {
        VStack(spacing: 0) {
            if let errorMessage {
                HStack(spacing: 6) {
                    Image(systemName: "exclamationmark.triangle.fill")
                    Text(errorMessage)
                        .font(.caption)
                        .lineLimit(2)
                    Spacer()
                    Button { self.errorMessage = nil } label: {
                        Image(systemName: "xmark")
                            .font(.caption2.bold())
                    }
                }
                .foregroundStyle(.white)
                .padding(.horizontal, 12)
                .padding(.vertical, 7)
                .background(.red.opacity(0.9))
            }

            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 16) {
                        if isLoading && transcript == nil {
                            ProgressView("加载对话记录…")
                                .frame(maxWidth: .infinity)
                                .padding(.top, 48)
                        } else if let turns = transcript?.turns, !turns.isEmpty {
                            ForEach(turns) { turn in
                                TurnBubbleView(
                                    agentId: agentId,
                                    turn: turn,
                                    onSendNow: { msgId in
                                        Task { await sendNow(messageId: msgId) }
                                    },
                                    onPreviewImage: { img in
                                        previewImage = img
                                    }
                                )
                            }
                        } else {
                            VStack(spacing: 10) {
                                if let agent {
                                    AgentAvatarView(agent: agent, size: 54)
                                    Text(agent.name)
                                        .font(.headline)
                                    Text("\(store.nodeDisplayName(for: agent.node)) · \(agent.model ?? agent.plugin)")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                                Text("发送一条消息或截图开始对话")
                                    .font(.subheadline)
                                    .foregroundStyle(.secondary)
                            }
                            .frame(maxWidth: .infinity)
                            .padding(.top, 56)
                        }

                        if let err = transcript?.error, !err.isEmpty {
                            Label(err, systemImage: "exclamationmark.octagon.fill")
                                .font(.footnote)
                                .foregroundStyle(.red)
                                .padding(10)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(.red.opacity(0.1), in: RoundedRectangle(cornerRadius: 10))
                        }

                        if isRunning {
                            HStack(spacing: 8) {
                                ProgressView()
                                    .controlSize(.small)
                                Text(transcript?.delivery.state == "starting" ? "Agent 正在启动…" : "Agent 正在思考并执行…")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            .padding(.horizontal, 4)
                        }

                        Color.clear
                            .frame(height: 1)
                            .id("bottom-anchor")
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
                }
                .scrollDismissesKeyboard(.interactively)
                .onChange(of: scrollToken) { _, _ in
                    withAnimation(.easeOut(duration: 0.18)) {
                        proxy.scrollTo("bottom-anchor", anchor: .bottom)
                    }
                }
                .onAppear {
                    proxy.scrollTo("bottom-anchor", anchor: .bottom)
                }
            }

            if let firstQuestion = pendingQuestions.first {
                InlineQuestionCard(question: firstQuestion)
                    .padding(.horizontal, 12)
                    .padding(.bottom, 6)
            }

            composerBar
        }
        .background(Color(.systemGroupedBackground))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 1) {
                    HStack(spacing: 5) {
                        Text(agent?.name ?? "对话")
                            .font(.subheadline.weight(.semibold))
                            .lineLimit(1)
                        if let status = agent?.status {
                            StatusDot(status: status)
                        }
                    }
                    Text("\(store.nodeDisplayName(for: agent?.node)) · \(agent?.model ?? agent?.plugin ?? "")")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Button {
                        showingModelSheet = true
                    } label: {
                        Label("切换模型 / 推理力度", systemImage: "cpu")
                    }
                    Button {
                        Task { await loadTranscript() }
                    } label: {
                        Label("刷新消息", systemImage: "arrow.clockwise")
                    }
                    if transcript?.capabilities?.compaction == true {
                        Button {
                            Task { await compact() }
                        } label: {
                            Label("压缩上下文", systemImage: "arrow.down.right.and.arrow.up.left")
                        }
                    }
                    if let agent {
                        Divider()
                        Button(role: agent.isArchived ? nil : .destructive) {
                            Task { await toggleArchive(agent) }
                        } label: {
                            Label(agent.isArchived ? "取消归档" : "归档会话", systemImage: agent.isArchived ? "tray.and.arrow.up" : "archivebox")
                        }
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
        .task(id: agentId) {
            await loadTranscript()
        }
        .onChange(of: pickerItems) { _, newItems in
            Task { await loadPickedPhotos(newItems) }
        }
        .sheet(isPresented: $showingModelSheet) {
            if let agent {
                ModelPickerSheet(agent: agent)
            }
        }
        .sheet(isPresented: Binding(
            get: { previewImage != nil },
            set: { if !$0 { previewImage = nil } }
        )) {
            if let previewImage {
                FullScreenImageSheet(image: previewImage)
            }
        }
    }

    // MARK: - Composer Bar

    private var composerBar: some View {
        VStack(spacing: 8) {
            if !selectedImages.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(Array(selectedImages.enumerated()), id: \.offset) { idx, img in
                            ZStack(alignment: .topTrailing) {
                                Image(uiImage: img)
                                    .resizable()
                                    .scaledToFill()
                                    .frame(width: 62, height: 62)
                                    .clipShape(RoundedRectangle(cornerRadius: 8))
                                Button {
                                    selectedImages.remove(at: idx)
                                } label: {
                                    Image(systemName: "xmark.circle.fill")
                                        .font(.system(size: 18))
                                        .symbolRenderingMode(.palette)
                                        .foregroundStyle(.white, .black.opacity(0.75))
                                }
                                .offset(x: 5, y: -5)
                            }
                            .padding(.top, 5)
                        }
                    }
                    .padding(.horizontal, 12)
                }
            }

            HStack(alignment: .bottom, spacing: 8) {
                PhotosPicker(
                    selection: $pickerItems,
                    maxSelectionCount: max(1, 4 - selectedImages.count),
                    matching: .images
                ) {
                    Image(systemName: "photo.on.rectangle.angled")
                        .font(.system(size: 21))
                        .foregroundStyle(.secondary)
                        .frame(width: 34, height: 34)
                }
                .disabled(selectedImages.count >= 4 || isSending)

                TextField(isRunning ? "继续补充或排队发送…" : "发送消息给 \(agent?.name ?? "Agent")…", text: $draft, axis: .vertical)
                    .lineLimit(1...5)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 18))

                let hasContent = !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !selectedImages.isEmpty
                if isRunning && !hasContent {
                    Button {
                        Task { await cancelTurn() }
                    } label: {
                        Image(systemName: "stop.circle.fill")
                            .font(.system(size: 32))
                            .foregroundStyle(.red)
                    }
                    .accessibilityLabel("停止当前生成")
                } else {
                    Button {
                        Task { await send() }
                    } label: {
                        if isSending {
                            ProgressView()
                                .frame(width: 32, height: 32)
                        } else {
                            Image(systemName: "arrow.up.circle.fill")
                                .font(.system(size: 32))
                                .foregroundStyle(hasContent ? Color.accentColor : Color.gray.opacity(0.4))
                        }
                    }
                    .disabled(!hasContent || isSending)
                    .accessibilityLabel("发送消息")
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
        }
        .background(.bar)
        .overlay(alignment: .top) {
            Divider()
        }
    }

    // MARK: - Actions

    private func loadTranscript() async {
        isLoading = true
        defer { isLoading = false }
        do {
            _ = try await store.loadTranscript(agentId: agentId)
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func loadPickedPhotos(_ items: [PhotosPickerItem]) async {
        guard !items.isEmpty else { return }
        var loaded: [UIImage] = []
        for item in items {
            if let data = try? await item.loadTransferable(type: Data.self),
               let img = UIImage(data: data) {
                loaded.append(img)
            }
        }
        selectedImages.append(contentsOf: loaded.prefix(max(0, 4 - selectedImages.count)))
        pickerItems = []
    }

    private func send() async {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let imgs = selectedImages
        guard !text.isEmpty || !imgs.isEmpty else { return }

        isSending = true
        errorMessage = nil
        let previousText = draft
        let previousImages = selectedImages
        draft = ""
        selectedImages = []
        defer { isSending = false }

        do {
            let canSteer = isRunning && (transcript?.capabilities?.steer == true)
            _ = try await store.sendMessage(
                agentId: agentId,
                text: text.isEmpty ? "请查看这张图片。" : text,
                images: imgs,
                now: false,
                steer: canSteer
            )
            let feedback = UIImpactFeedbackGenerator(style: .light)
            feedback.impactOccurred()
        } catch {
            draft = previousText
            selectedImages = previousImages
            errorMessage = error.localizedDescription
        }
    }

    private func sendNow(messageId: String) async {
        do {
            try await store.sendMessageNow(agentId: agentId, messageId: messageId)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func cancelTurn() async {
        do {
            try await store.cancelAgent(agentId: agentId)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func compact() async {
        do {
            try await store.compactAgent(agentId: agentId)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func toggleArchive(_ agent: Agent) async {
        do {
            try await store.archiveAgent(agentId: agent.id, archived: !agent.isArchived)
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

// MARK: - Turn Bubble & Tool Cards

private struct TurnBubbleView: View {
    @Environment(AppStore.self) private var store
    let agentId: String
    let turn: TranscriptTurn
    let onSendNow: (String) -> Void
    let onPreviewImage: (UIImage) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !turn.input.isEmpty || !(turn.attachments?.isEmpty ?? true) {
                HStack(alignment: .top, spacing: 8) {
                    Spacer(minLength: 36)
                    VStack(alignment: .trailing, spacing: 6) {
                        if let attachments = turn.attachments, !attachments.isEmpty {
                            AttachmentThumbnailsView(
                                agentId: agentId,
                                attachments: attachments,
                                onPreview: onPreviewImage
                            )
                        }
                        if !turn.input.isEmpty {
                            Text(turn.input)
                                .font(.body)
                                .foregroundStyle(.white)
                                .textSelection(.enabled)
                                .padding(.horizontal, 13)
                                .padding(.vertical, 9)
                                .background(
                                    (turn.withdrawn == true ? Color.gray : Color.accentColor),
                                    in: RoundedRectangle(cornerRadius: 16, style: .continuous)
                                )
                                .strikethrough(turn.withdrawn == true)
                        }
                        HStack(spacing: 6) {
                            if turn.withdrawn == true {
                                Text("已撤回")
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            } else if turn.isQueued {
                                Text("排队等待中")
                                    .font(.caption2)
                                    .foregroundStyle(.orange)
                                Button("立即插队") {
                                    onSendNow(turn.messageId)
                                }
                                .font(.caption2.weight(.semibold))
                                .buttonStyle(.bordered)
                                .controlSize(.mini)
                            }
                            Text(DateHelpers.relative(turn.sentAt))
                                .font(.caption2)
                                .foregroundStyle(.tertiary)
                        }
                    }
                    UserAvatarView(size: 28)
                }
            }

            if !turn.items.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(Array(turn.items.enumerated()), id: \.offset) { _, item in
                        TranscriptItemView(item: item)
                    }
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color(.systemBackground), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            }
        }
    }
}

private struct AttachmentThumbnailsView: View {
    @Environment(AppStore.self) private var store
    let agentId: String
    let attachments: [Attachment]
    let onPreview: (UIImage) -> Void

    var body: some View {
        HStack(spacing: 6) {
            ForEach(attachments) { att in
                if att.isImage {
                    let key = "\(agentId)/\(att.id)"
                    if let img = store.attachmentImages[key] {
                        Button {
                            onPreview(img)
                        } label: {
                            Image(uiImage: img)
                                .resizable()
                                .scaledToFill()
                                .frame(width: 110, height: 110)
                                .clipShape(RoundedRectangle(cornerRadius: 12))
                        }
                    } else {
                        RoundedRectangle(cornerRadius: 12)
                            .fill(Color(.secondarySystemFill))
                            .frame(width: 90, height: 90)
                            .overlay { ProgressView() }
                            .task(id: key) {
                                store.loadAttachmentImageIfNeeded(agentId: agentId, attachmentId: att.id)
                            }
                    }
                } else {
                    Label(att.name, systemImage: "doc.fill")
                        .font(.caption)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 6)
                        .background(Color(.secondarySystemFill), in: Capsule())
                }
            }
        }
    }
}

private struct TranscriptItemView: View {
    let item: TranscriptItem

    var body: some View {
        switch item {
        case .output(let stream, let text):
            if stream == .thought {
                ThoughtCard(text: text)
            } else {
                MarkdownContentView(text: text)
            }
        case .toolCall(let tc):
            ToolCallCard(toolCall: tc)
        case .permission(_, let title, let decision):
            HStack(spacing: 6) {
                Image(systemName: decision.hasPrefix("allow") ? "checkmark.shield.fill" : "xmark.shield.fill")
                    .foregroundStyle(decision.hasPrefix("allow") ? .green : .orange)
                Text("权限「\(title)」：\(decision)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
        case .steer(_, let text, _, let read):
            HStack(spacing: 6) {
                Image(systemName: "arrow.turn.down.right")
                    .foregroundStyle(.indigo)
                Text(text)
                    .font(.caption)
                Spacer()
                Text(read ? "已读" : "插话")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            .padding(8)
            .background(Color.indigo.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
        case .warning(let message):
            Label(message, systemImage: "exclamationmark.triangle.fill")
                .font(.caption)
                .foregroundStyle(.orange)
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.orange.opacity(0.1), in: RoundedRectangle(cornerRadius: 8))
        case .error(let message):
            Label(message, systemImage: "xmark.octagon.fill")
                .font(.caption)
                .foregroundStyle(.red)
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.red.opacity(0.1), in: RoundedRectangle(cornerRadius: 8))
        case .unknown:
            EmptyView()
        }
    }
}

private struct ThoughtCard: View {
    let text: String
    @State private var expanded = false

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            Text(text)
                .font(.caption)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.top, 4)
        } label: {
            Label("思考过程（\(text.count) 字）", systemImage: "brain")
                .font(.caption.weight(.medium))
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
    }
}

private struct ToolCallCard: View {
    let toolCall: ToolCallItem
    @State private var expanded = false

    private var statusColor: Color {
        switch toolCall.status {
        case "completed": return .green
        case "failed": return .red
        default: return .blue
        }
    }

    private var statusLabel: String {
        switch toolCall.status {
        case "completed": return "完成"
        case "failed": return "失败"
        case "pending": return "等待"
        default: return "执行中"
        }
    }

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: 6) {
                if let cmd = toolCall.detail?.command, !cmd.isEmpty {
                    Text("$ \(cmd)")
                        .font(.system(.caption2, design: .monospaced))
                        .foregroundStyle(.primary)
                        .textSelection(.enabled)
                        .padding(8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color(.tertiarySystemFill), in: RoundedRectangle(cornerRadius: 6))
                }
                if let changes = toolCall.detail?.changes, !changes.isEmpty {
                    VStack(alignment: .leading, spacing: 3) {
                        ForEach(Array(changes.enumerated()), id: \.offset) { _, change in
                            HStack(spacing: 6) {
                                Text(change.kind.uppercased())
                                    .font(.caption2.monospaced().bold())
                                    .foregroundStyle(change.kind == "delete" ? .red : .green)
                                Text(change.path)
                                    .font(.caption2.monospaced())
                                    .lineLimit(1)
                            }
                        }
                    }
                }
                let out = toolCall.detail?.output ?? toolCall.output ?? toolCall.detail?.result
                if let out, !out.isEmpty {
                    ScrollView(.horizontal, showsIndicators: true) {
                        Text(String(out.prefix(2400)))
                            .font(.system(.caption2, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Color(.tertiarySystemFill), in: RoundedRectangle(cornerRadius: 6))
                }
                if let err = toolCall.detail?.error, !err.isEmpty {
                    Text(err)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.red)
                }
            }
            .padding(.top, 6)
        } label: {
            HStack(spacing: 7) {
                if toolCall.status == "in_progress" || toolCall.status == "pending" {
                    ProgressView()
                        .controlSize(.mini)
                } else {
                    Image(systemName: toolCall.status == "completed" ? "checkmark.circle.fill" : "xmark.circle.fill")
                        .font(.caption)
                        .foregroundStyle(statusColor)
                }
                Text(toolCall.title)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                Spacer(minLength: 4)
                Text(statusLabel)
                    .font(.caption2)
                    .foregroundStyle(statusColor)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
    }
}

// MARK: - Inline Question Card inside SessionView

private struct InlineQuestionCard: View {
    @Environment(AppStore.self) private var store
    let question: HumanItem

    @State private var customReply = ""
    @State private var isSubmitting = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label("Agent 需要你的决定", systemImage: "hand.raised.fill")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.orange)
                Spacer()
                Text(question.actionBadge)
                    .font(.caption2.weight(.semibold))
                    .padding(.horizontal, 7)
                    .padding(.vertical, 2)
                    .background(.orange.opacity(0.18), in: Capsule())
                    .foregroundStyle(.orange)
            }

            Text(question.text)
                .font(.subheadline)
                .fixedSize(horizontal: false, vertical: true)

            if !question.options.isEmpty {
                FlowLayout(spacing: 6) {
                    ForEach(question.options, id: \.self) { opt in
                        Button(opt) {
                            Task { await submit(opt) }
                        }
                        .font(.caption.weight(.medium))
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                        .disabled(isSubmitting)
                    }
                }
            }

            HStack(spacing: 6) {
                TextField("或输入自定义回复…", text: $customReply)
                    .font(.footnote)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 6)
                    .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
                    .disabled(isSubmitting)

                Button("回复") {
                    let trimmed = customReply.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !trimmed.isEmpty else { return }
                    Task { await submit(trimmed) }
                }
                .font(.caption.weight(.semibold))
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(isSubmitting || customReply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(12)
        .background(Color(.systemBackground), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.orange.opacity(0.45), lineWidth: 1.2)
        )
        .shadow(color: .black.opacity(0.08), radius: 8, y: 2)
    }

    private func submit(_ answer: String) async {
        isSubmitting = true
        defer { isSubmitting = false }
        try? await store.answerQuestion(id: question.id, answer: answer)
        customReply = ""
    }
}

// MARK: - Model Switcher & Fullscreen Image Sheet

private struct ModelPickerSheet: View {
    @Environment(AppStore.self) private var store
    @Environment(\.dismiss) private var dismiss

    let agent: Agent
    @State private var plugin: PluginInfo?
    @State private var selectedModel: String
    @State private var selectedEffort: String
    @State private var isLoading = false
    @State private var isSaving = false
    @State private var errorMessage: String?

    init(agent: Agent) {
        self.agent = agent
        _selectedModel = State(initialValue: agent.model ?? "")
        _selectedEffort = State(initialValue: agent.currentEffort ?? "")
    }

    private var models: [String] {
        guard let plugin else { return [] }
        return plugin.models.isEmpty ? plugin.allModels : plugin.models
    }

    private var efforts: [String] {
        plugin?.availableEfforts(for: selectedModel.isEmpty ? nil : selectedModel) ?? []
    }

    var body: some View {
        NavigationStack {
            Form {
                if isLoading {
                    ProgressView("读取模型列表…")
                } else {
                    Section("模型（\(agent.plugin)）") {
                        Picker("模型", selection: $selectedModel) {
                            Text("默认模型").tag("")
                            ForEach(models, id: \.self) { model in
                                Text(model).tag(model)
                            }
                        }
                        .onChange(of: selectedModel) { _, _ in
                            if !efforts.contains(selectedEffort) {
                                selectedEffort = ""
                            }
                        }
                        if !efforts.isEmpty {
                            Picker("推理力度", selection: $selectedEffort) {
                                Text("默认").tag("")
                                ForEach(efforts, id: \.self) { effort in
                                    Text(effort).tag(effort)
                                }
                            }
                        }
                    }
                }
                if let errorMessage {
                    Section {
                        Text(errorMessage).font(.footnote).foregroundStyle(.red)
                    }
                }
            }
            .navigationTitle("切换模型")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("取消") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("保存") {
                        Task { await save() }
                    }
                    .fontWeight(.semibold)
                    .disabled(isSaving)
                }
            }
            .task {
                isLoading = true
                defer { isLoading = false }
                let list = (try? await store.fetchPlugins(node: agent.node)) ?? store.localPlugins
                plugin = list.first { $0.name == agent.plugin }
            }
        }
    }

    private func save() async {
        isSaving = true
        errorMessage = nil
        defer { isSaving = false }
        do {
            try await store.setModel(
                agentId: agent.id,
                model: selectedModel.isEmpty ? nil : selectedModel,
                effort: efforts.contains(selectedEffort) ? selectedEffort : ""
            )
            dismiss()
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

private struct FullScreenImageSheet: View {
    let image: UIImage
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ZStack {
                Color.black.ignoresSafeArea()
                Image(uiImage: image)
                    .resizable()
                    .scaledToFit()
            }
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("完成") { dismiss() }
                        .tint(.white)
                }
            }
            .toolbarBackground(.black, for: .navigationBar)
            .toolbarColorScheme(.dark, for: .navigationBar)
        }
    }
}
