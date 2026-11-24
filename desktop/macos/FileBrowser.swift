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

@MainActor
private final class FileBrowserModel: ObservableObject {
    @Published var local: FileListing?
    @Published var remote: FileListing?
    @Published var busy = false
    @Published var status = ""
    @Published var progress: FileProgress?
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

    func copy(_ side: FileSide, names: [String]) async {
        guard let source = listing(side), let target = listing(side.opposite), !names.isEmpty else { return }
        let allowed = Set(source.entries.filter(\.canCopy).map(\.name))
        guard names.allSatisfy(allowed.contains) else { return }
        let request = FileCommand(action: "copy", side: side, source: source.path, destination: target.path, names: names)
        if let event = await execute(request, language("Передаём файлы…", "Transferring files…")),
           let count = event.files {
            let done = language("Скопировано файлов — \(count)", "Files copied — \(count)")
            await browse(side.opposite, path: target.path, status: done)
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

    var listing: FileListing? { model.listing(side) }

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 6) {
                Text(title).font(.headline).lineLimit(1)
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
                        if entry.kind == "directory", let directory = listing?.path {
                            let path = directory.hasSuffix("/") ? directory + entry.name : directory + "/" + entry.name
                            Task { await model.browse(side, path: path) }
                        }
                    })
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
