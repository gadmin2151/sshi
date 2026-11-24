import AppKit
import Foundation
import SwiftUI
import UniformTypeIdentifiers

private enum FileSide: String, Codable {
    case local, remote
    var opposite: Self { self == .local ? .remote : .local }
}

private struct FileEntry: Decodable, Identifiable {
    let name: String
    let kind: String
    let size: UInt64?
    var id: String { name }
    var canCopy: Bool { kind == "file" || kind == "directory" }
}

private struct FileListing: Decodable {
    let side: FileSide
    let path: String
    let parent: String?
    let entries: [FileEntry]
}

private struct FileCommand: Encodable {
    let action: String
    let side: FileSide
    var path: String? = nil
    var reconnect: Bool = false
    var source: String? = nil
    var destination: String? = nil
    var names: [String]? = nil
    var destination_side: FileSide? = nil
    var move_files: Bool? = nil
    var name: String? = nil
    var new_name: String? = nil
    var text: String? = nil
    var revision: String? = nil
}

private struct FileProgress: Decodable {
    let name: String
    let bytes: UInt64
    let total: UInt64?
}

private struct FileEvent: Decodable {
    let type: String
    let listing: FileListing?
    let progress: FileProgress?
    let files: UInt64?
    let entries: Int?
    let text: String?
    let revision: String?
    let warning: String?
    let message: String?
}

private final class FileWorker: @unchecked Sendable {
    private let name: String
    private var session: Process?
    private var input: FileHandle?
    private var output: FileHandle?
    private var buffer = Data()

    init(_ name: String) { self.name = name }

    // Only the active request touches the pipes. Close the input to end the
    // remote SFTP channel when the window and its last request are gone.
    deinit { try? input?.close() }

    private func start() throws {
        let proc = Process()
        let requests = Pipe()
        let responses = Pipe()
        proc.executableURL = Client.binary
        proc.arguments = ["__files", "--", name]
        proc.standardInput = requests
        proc.standardOutput = responses
        proc.standardError = FileHandle.nullDevice
        try proc.run()
        session = proc
        input = requests.fileHandleForWriting
        output = responses.fileHandleForReading
        buffer.removeAll()
    }

    func send(_ command: FileCommand, progress: (FileProgress) -> Void) throws -> FileEvent {
        if session?.isRunning != true { try start() }
        guard let input, let output else { throw ClientError(message: "SSHI file session unavailable") }
        var request = try JSONEncoder().encode(command)
        request.append(10)
        try input.write(contentsOf: request)
        while true {
            while buffer.firstIndex(of: 10) == nil {
                let block = output.availableData
                guard !block.isEmpty else { throw ClientError(message: "SSHI file session closed") }
                buffer.append(block)
            }
            guard let end = buffer.firstIndex(of: 10) else { continue }
            let event = try JSONDecoder().decode(FileEvent.self, from: buffer[..<end])
            buffer.removeSubrange(...end)
            if let update = event.progress { progress(update) } else { return event }
        }
    }
}

private struct DraggedFiles: Codable {
    let session: UUID
    let side: FileSide
    let names: [String]
}

private let dragType = "com.sshi.pane-selection"

private struct FileClipboard {
    let side: FileSide
    let path: String
    let names: [String]
    let move: Bool
}

private struct FileDraft: Identifiable {
    let id = UUID()
    let side: FileSide
    let path: String
    let name: String
    let text: String
    let revision: String
}

private struct FileNameRequest: Identifiable {
    let id = UUID()
    let side: FileSide
    let path: String
    let original: String?
}

private struct DeleteFiles: Identifiable {
    let id = UUID()
    let side: FileSide
    let path: String
    let names: [String]
}

private enum FilePrompt: Identifiable {
    case edit(FileDraft), name(FileNameRequest)

    var id: UUID {
        switch self {
        case .edit(let draft): return draft.id
        case .name(let form): return form.id
        }
    }
}

@MainActor
private final class FileBrowserModel: ObservableObject {
    @Published var local: FileListing?
    @Published var remote: FileListing?
    @Published var busy = false
    @Published var status = ""
    @Published var progress: FileProgress?
    @Published var clipboard: FileClipboard?
    @Published var prompt: FilePrompt?
    let id = UUID()
    let language: AppLanguage
    private let worker: FileWorker
    private var requestID = UUID()

    init(profile: Profile, language: AppLanguage) {
        self.language = language
        worker = FileWorker(profile.name)
    }

    func listing(_ side: FileSide) -> FileListing? { side == .local ? local : remote }

    func browse(_ side: FileSide, path: String, reconnect: Bool = false, status after: String? = nil) async {
        let working = side == .remote && (remote == nil || reconnect)
            ? language("Подключаемся к удалённой машине…", "Connecting to the remote computer…")
            : language("Открываем папку…", "Opening folder…")
        if let event = await execute(FileCommand(action: "list", side: side, path: path, reconnect: reconnect), working),
           let listing = event.listing {
            if side == .local { local = listing } else { remote = listing }
            status = after ?? language("Готово", "Ready")
        }
    }

    func open(_ side: FileSide, entry: FileEntry) async {
        guard let listing = listing(side) else { return }
        if entry.kind == "directory" {
            await browse(side, path: (listing.path as NSString).appendingPathComponent(entry.name))
        } else if entry.kind == "file" {
            let command = FileCommand(action: "read_text", side: side, path: listing.path, name: entry.name)
            if let event = await execute(command, language("Открываем файл…", "Opening file…")),
               let text = event.text, let revision = event.revision {
                status = language("Готово", "Ready")
                prompt = .edit(FileDraft(side: side, path: listing.path, name: entry.name, text: text, revision: revision))
            }
        }
    }

    func save(_ draft: FileDraft, text: String, revision: String) async -> FileEvent? {
        let command = FileCommand(action: "save_text", side: draft.side, path: draft.path, name: draft.name, text: text, revision: revision)
        let result = await execute(command, language("Сохраняем…", "Saving…"))
        if let result { status = result.warning ?? language("Сохранено.", "Saved.") }
        return result
    }

    func askName(_ side: FileSide, original: String? = nil) {
        guard let listing = listing(side) else { return }
        prompt = .name(FileNameRequest(side: side, path: listing.path, original: original))
    }

    func name(_ form: FileNameRequest, as name: String) async {
        let command = form.original.map {
            FileCommand(action: "rename", side: form.side, path: form.path, name: $0, new_name: name)
        } ?? FileCommand(action: "new_folder", side: form.side, path: form.path, name: name)
        await mutate(command, working: language("Обновляем папку…", "Updating folder…"))
    }

    func delete(_ form: DeleteFiles) async {
        let command = FileCommand(action: "delete", side: form.side, path: form.path, names: form.names)
        await mutate(command, working: language("Удаляем…", "Deleting…"))
    }

    private func mutate(_ command: FileCommand, working: String) async {
        guard !busy else { return }
        if await execute(command, working) != nil { status = language("Готово", "Ready") }
        await refresh([command.side])
    }

    func remember(_ side: FileSide, names: [String], move: Bool) {
        guard let source = listing(side), !names.isEmpty else { return }
        clipboard = FileClipboard(side: side, path: source.path, names: names, move: move)
        status = move
            ? language("Вырезано — \(names.count). Выберите папку и нажмите «Вставить».", "Cut — \(names.count). Choose a folder and select Paste.")
            : language("Скопировано в буфер — \(names.count). Выберите папку и нажмите «Вставить».", "Copied — \(names.count). Choose a folder and select Paste.")
    }

    func paste(_ side: FileSide) async {
        guard let clipboard else { return }
        await transfer(clipboard, to: side)
    }

    func copy(_ side: FileSide, names: [String]) async {
        guard let source = listing(side), !names.isEmpty else { return }
        let allowed = Set(source.entries.filter(\.canCopy).map(\.name))
        guard names.allSatisfy(allowed.contains) else { return }
        await transfer(FileClipboard(side: side, path: source.path, names: names, move: false), to: side.opposite)
    }

    private func transfer(_ data: FileClipboard, to side: FileSide) async {
        guard let destination = listing(side), !busy else { return }
        let request = FileCommand(action: "copy", side: data.side, source: data.path, destination: destination.path,
                                  names: data.names, destination_side: side, move_files: data.move)
        let working = data.move ? language("Перемещаем…", "Moving…") : language("Передаём файлы…", "Transferring files…")
        if let result = await execute(request, working) {
            if let entries = result.entries {
                status = language("Перемещено объектов — \(entries)", "Items moved — \(entries)")
                clipboard = nil
            } else if let count = result.files {
                status = language("Скопировано файлов — \(count)", "Files copied — \(count)")
            }
        }
        await refresh(data.move && data.side != side ? [side, data.side] : [side])
    }

    func refresh(_ sides: [FileSide]) async {
        for side in sides {
            if let path = listing(side)?.path { await browse(side, path: path, status: status) }
        }
    }

    private func execute(_ request: FileCommand, _ working: String) async -> FileEvent? {
        guard !busy else { return nil }
        busy = true
        status = working
        progress = nil
        let id = UUID()
        requestID = id
        defer { busy = false; progress = nil }
        do {
            let worker = worker
            let reply = try await Task.detached(priority: .userInitiated) {
                try worker.send(request) { update in
                    DispatchQueue.main.async {
                        if self.busy && self.requestID == id { self.progress = update }
                    }
                }
            }.value
            if let failure = reply.message {
                status = failure
                return nil
            }
            return reply
        } catch {
            status = error.localizedDescription
            return nil
        }
    }
}

@MainActor
struct FileBrowser: View {
    @Environment(\.dismiss) private var dismiss
    let profile: Profile
    let language: AppLanguage
    @StateObject private var model: FileBrowserModel
    @State private var localSelection = Set<String>()
    @State private var remoteSelection = Set<String>()
    @StateObject private var login = SocketSession()

    init(profile: Profile, language: AppLanguage) {
        self.profile = profile
        self.language = language
        _model = StateObject(wrappedValue: FileBrowserModel(profile: profile, language: language))
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 13) {
                Image(systemName: "folder.fill").font(.title2).foregroundColor(.accentColor)
                VStack(alignment: .leading, spacing: 3) {
                    Text(language("Файлы", "Files")).font(.title2.bold())
                    Text(profile.name).font(.subheadline).foregroundColor(.secondary)
                }
                Spacer()
                Button(language("Войти в терминале…", "Sign in via Terminal…")) {
                    if login.active {
                        Task { await model.browse(.remote, path: model.remote?.path ?? "", reconnect: true) }
                    } else {
                        login.start(profile, language, interactive: true)
                    }
                }.disabled(model.busy || login.pending)
                Button(language("Готово", "Done")) { dismiss() }
                    .keyboardShortcut(.cancelAction).disabled(model.busy)
            }.padding(15)
            Divider()
            HStack(spacing: 8) {
                Button {
                    Task { await model.copy(.local, names: localSelection.sorted()) }
                } label: { Label(language("Загрузить", "Upload"), systemImage: "arrow.right") }
                    .disabled(model.local == nil || model.remote == nil || localSelection.isEmpty)
                Button {
                    Task { await model.copy(.remote, names: remoteSelection.sorted()) }
                } label: { Label(language("Скачать", "Download"), systemImage: "arrow.left") }
                    .disabled(model.local == nil || model.remote == nil || remoteSelection.isEmpty)
                Spacer()
                Text(language("Перетащите файлы в соседнюю панель", "Drag files to the other pane"))
                    .font(.callout).foregroundColor(.secondary)
            }.padding(.horizontal, 15).padding(.vertical, 8).disabled(model.busy)
            Divider()
            HSplitView {
                BrowserPane(title: language("Этот Mac", "This Mac"), side: .local, model: model, selection: $localSelection)
                BrowserPane(title: profile.name, side: .remote, model: model, selection: $remoteSelection)
            }.disabled(model.busy)
            Divider()
            VStack(alignment: .leading, spacing: 6) {
                if let progress = model.progress {
                    if let total = progress.total, total > 0 {
                        ProgressView(value: Double(min(progress.bytes, total)), total: Double(total))
                    } else { ProgressView() }
                    Text("\(progress.name) · \(ByteCountFormatter.string(fromByteCount: Int64(clamping: progress.bytes), countStyle: .file))")
                        .lineLimit(1)
                } else {
                    Text(login.pending ? login.status : login.lastError.isEmpty ? model.status : login.lastError)
                        .lineLimit(3).textSelection(.enabled)
                }
            }.font(.callout).frame(maxWidth: .infinity, alignment: .leading).padding(12)
        }
        .frame(minWidth: 850, idealWidth: 1000, minHeight: 500, idealHeight: 620)
        .interactiveDismissDisabled(model.busy)
        .task {
            await model.browse(.local, path: "")
            if !Task.isCancelled { await model.browse(.remote, path: "") }
        }
        .onChange(of: login.active) { active in
            if active { Task { await model.browse(.remote, path: model.remote?.path ?? "", reconnect: true) } }
        }
        .onDisappear { login.dispose() }
        .sheet(item: $model.prompt) { prompt in
            switch prompt {
            case .edit(let draft): FileEditorSheet(model: model, draft: draft)
            case .name(let form): FileNameSheet(model: model, form: form)
            }
        }
    }
}

@MainActor
private struct FileNameSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var model: FileBrowserModel
    let form: FileNameRequest
    @State private var name: String
    @FocusState private var focused: Bool

    init(model: FileBrowserModel, form: FileNameRequest) {
        self.model = model
        self.form = form
        _name = State(initialValue: form.original ?? "")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 11) {
            Text(form.original == nil ? model.language("Новая папка", "New folder") : model.language("Переименовать", "Rename"))
                .font(.headline)
            Text(form.path).font(.callout).foregroundColor(.secondary).lineLimit(1).truncationMode(.middle)
            TextField(model.language("Имя", "Name"), text: $name).focused($focused)
            HStack {
                Spacer()
                Button(model.language("Отмена", "Cancel")) { dismiss() }.keyboardShortcut(.cancelAction)
                Button(form.original == nil ? model.language("Создать", "Create") : model.language("Переименовать", "Rename")) {
                    let result = name
                    dismiss()
                    Task { await model.name(form, as: result) }
                }.keyboardShortcut(.defaultAction)
                    .disabled(name.isEmpty || name == "." || name == ".." || name == form.original || name.contains("/"))
            }
        }.padding(20).frame(width: 420).onAppear { focused = true }
    }
}

@MainActor
private struct FileEditorSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var model: FileBrowserModel
    let draft: FileDraft
    @State private var text: String
    @State private var savedText: String
    @State private var revision: String
    @State private var saved = false
    @State private var feedback = ""
    @State private var discard = false
    @FocusState private var focused: Bool

    init(model: FileBrowserModel, draft: FileDraft) {
        self.model = model
        self.draft = draft
        _text = State(initialValue: draft.text)
        _savedText = State(initialValue: draft.text)
        _revision = State(initialValue: draft.revision)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            Text(draft.name).font(.headline).textSelection(.enabled)
            Text((draft.path as NSString).appendingPathComponent(draft.name))
                .font(.callout).foregroundColor(.secondary).textSelection(.enabled)
            TextEditor(text: $text).font(.system(.body, design: .monospaced)).focused($focused)
                .accessibilityLabel(model.language("Текст файла", "File text"))
                .disabled(model.busy)
                .border(Color.secondary.opacity(0.22))
            HStack {
                Text(feedback).font(.callout).foregroundColor(.secondary).textSelection(.enabled)
                Spacer()
                Button(model.language("Закрыть", "Close")) {
                    if text == savedText { finish() } else { discard = true }
                }.keyboardShortcut(.cancelAction)
                Button(model.language("Сохранить", "Save")) { Task { await save() } }
                    .buttonStyle(.borderedProminent).keyboardShortcut("s", modifiers: .command)
                    .disabled(text == savedText)
            }.disabled(model.busy)
        }
        .padding(16).frame(minWidth: 660, minHeight: 480)
        .onAppear { focused = true }
        .interactiveDismissDisabled(model.busy || text != savedText)
        .alert(isPresented: $discard) {
            Alert(title: Text(model.language("Закрыть без сохранения?", "Close without saving?")),
                  primaryButton: .destructive(Text(model.language("Закрыть", "Close")), action: finish),
                  secondaryButton: .cancel(Text(model.language("Отмена", "Cancel"))))
        }
    }

    private func save() async {
        let value = text
        feedback = model.language("Сохраняем…", "Saving…")
        if let result = await model.save(draft, text: value, revision: revision), let nextRevision = result.revision {
            revision = nextRevision
            savedText = value
            saved = true
        }
        feedback = model.status
    }

    private func finish() {
        dismiss()
        if saved { Task { await model.refresh([draft.side]) } }
    }
}

@MainActor
private struct BrowserPane: View {
    let title: String
    let side: FileSide
    @ObservedObject var model: FileBrowserModel
    @Binding var selection: Set<String>
    @State private var address = ""
    @State private var target = false
    @State private var deletion: DeleteFiles?

    var listing: FileListing? { model.listing(side) }

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text(title).font(.headline).lineLimit(1)
                    Spacer()
                    Menu(model.language("Действия", "Actions")) { commands(selection.sorted()) }
                        .fixedSize().disabled(listing == nil)
                }
                HStack(spacing: 6) {
                    Button {
                        if let path = listing?.parent { Task { await model.browse(side, path: path) } }
                    } label: { Image(systemName: "arrow.up") }
                        .help(model.language("На уровень выше", "Parent folder")).disabled(listing?.parent == nil)
                    TextField(model.language("Путь к папке", "Folder path"), text: $address)
                        .onSubmit { Task { await model.browse(side, path: address) } }
                    Button {
                        Task { await model.browse(side, path: address, reconnect: side == .remote) }
                    } label: { Image(systemName: "arrow.clockwise") }
                        .help(model.language("Обновить", "Refresh"))
                }
            }.padding(10)
            Divider()
            List(selection: $selection) {
                ForEach(listing?.entries ?? []) { entry in
                    HStack {
                        Image(systemName: entry.kind == "directory" ? "folder.fill" : entry.kind == "file" ? "doc" : "link")
                            .foregroundColor(entry.kind == "directory" ? .accentColor : .secondary)
                            .frame(width: 20)
                        Text(entry.name).lineLimit(1).truncationMode(.middle)
                        Spacer()
                        if let size = entry.size {
                            Text(ByteCountFormatter.string(fromByteCount: Int64(clamping: size), countStyle: .file))
                                .font(.system(.caption, design: .monospaced)).foregroundColor(.secondary)
                        }
                    }.contentShape(Rectangle()).tag(entry.name)
                    .simultaneousGesture(TapGesture(count: 2).onEnded {
                        Task { await model.open(side, entry: entry) }
                    })
                    .contextMenu { commands(selection.contains(entry.name) ? selection.sorted() : [entry.name]) }
                    .onDrag { drag(entry) }
                }
            }.listStyle(.plain)
            .padding(2)
            .overlay(RoundedRectangle(cornerRadius: 4).stroke(Color.accentColor.opacity(target ? 0.85 : 0), lineWidth: 2).allowsHitTesting(false))
            .onDrop(of: [dragType], isTargeted: $target, perform: drop)
        }
        .frame(minWidth: 280, maxWidth: .infinity, maxHeight: .infinity)
        .onChange(of: listing?.path) { path in address = path ?? ""; selection.removeAll() }
        .onChange(of: listing?.entries.map(\.name)) { names in selection.formIntersection(names ?? []) }
        .alert(item: $deletion) { form in
            let names = form.names.prefix(5).joined(separator: "\n")
            let extra = form.names.count > 5 ? "\n… +\(form.names.count - 5)" : ""
            return Alert(title: Text(model.language("Удалить без корзины?", "Delete permanently?")),
                         message: Text("\(title) · \(form.path)\n\n\(names)\(extra)"),
                         primaryButton: .destructive(Text(model.language("Удалить", "Delete"))) {
                             Task { await model.delete(form) }
                         }, secondaryButton: .cancel(Text(model.language("Отмена", "Cancel"))))
        }
    }

    @ViewBuilder
    private func commands(_ names: [String]) -> some View {
        let chosen = Set(names)
        let entries = listing?.entries.filter { chosen.contains($0.name) } ?? []
        let item = entries.count == 1 ? entries.first : nil
        let copyable = !entries.isEmpty && entries.allSatisfy(\.canCopy)
        Button(item?.kind == "directory" ? model.language("Открыть папку", "Open folder") : model.language("Открыть в редакторе", "Open in editor")) {
            if let item { Task { await model.open(side, entry: item) } }
        }.disabled(item?.canCopy != true)
        Divider()
        Button(model.language("Копировать", "Copy")) { model.remember(side, names: names, move: false) }.disabled(!copyable)
        Button(model.language("Вырезать", "Cut")) { model.remember(side, names: names, move: true) }.disabled(!copyable)
        Button(model.language("Вставить", "Paste")) { Task { await model.paste(side) } }.disabled(model.clipboard == nil || listing == nil)
        Divider()
        Button(model.language("Переименовать…", "Rename…")) {
            if let item { model.askName(side, original: item.name) }
        }.disabled(item == nil)
        Button(model.language("Новая папка…", "New folder…")) { model.askName(side) }.disabled(listing == nil)
        Button(model.language("Удалить…", "Delete…"), role: .destructive) {
            if let listing { deletion = DeleteFiles(side: side, path: listing.path, names: names) }
        }.disabled(entries.isEmpty)
    }

    private func drag(_ entry: FileEntry) -> NSItemProvider {
        let provider = NSItemProvider()
        guard entry.canCopy else { return provider }
        let names = selection.contains(entry.name)
            ? (listing?.entries ?? []).filter { selection.contains($0.name) && $0.canCopy }.map(\.name)
            : [entry.name]
        guard let payload = try? JSONEncoder().encode(DraggedFiles(session: model.id, side: side, names: names)) else { return provider }
        provider.registerDataRepresentation(forTypeIdentifier: dragType, visibility: .ownProcess) { complete in
            complete(payload, nil)
            return nil
        }
        return provider
    }

    private func drop(_ items: [NSItemProvider]) -> Bool {
        guard !model.busy, listing != nil,
              let entry = items.first(where: { $0.hasItemConformingToTypeIdentifier(dragType) }) else { return false }
        entry.loadDataRepresentation(forTypeIdentifier: dragType) { data, _ in
            guard let data, let drag = try? JSONDecoder().decode(DraggedFiles.self, from: data) else { return }
            Task { @MainActor in
                if drag.session == model.id, drag.side == side.opposite { await model.copy(drag.side, names: drag.names) }
            }
        }
        return true
    }
}
