import AppKit
import Combine
import Foundation
import SwiftUI
import UniformTypeIdentifiers

struct Profile: Codable, Equatable, Identifiable {
    let name: String
    let user: String
    let endpoint: String
    let relay: String

    var id: String { name }
    var destination: String { "\(user)@\(endpoint)" }
    var prefix: [String] { ["profile", "run", "--name", name, "--"] }
    func socket(_ action: String) -> [String] { ["socket", action, "--", name] }
}

struct ReleaseUpdate: Decodable {
    let current: String
    let latest: String
    let available: Bool
    let release_url: URL
    let download_url: URL?

    static func fetch() async throws -> Self {
        try await Task.detached(priority: .utility) {
            try JSONDecoder().decode(Self.self, from: Client.run(["update", "--json"]))
        }.value
    }
}

struct RelaySettings: Decodable {
    let relay_url: String?
}

struct ClientError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

enum Client {
    static var binary: URL {
        Bundle.main.bundleURL.appendingPathComponent("Contents/Resources/bin/sshi")
    }

    static func execute(_ executable: URL, _ arguments: [String], input: String? = nil) throws -> Data {
        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        let output = Pipe()
        process.standardOutput = output
        process.standardError = output
        let stdin = Pipe()
        process.standardInput = stdin
        try process.run()
        if let input = input {
            stdin.fileHandleForWriting.write(Data(input.utf8))
        }
        stdin.fileHandleForWriting.closeFile()
        let contents = output.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            let detail = String(decoding: contents, as: UTF8.self)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            throw ClientError(message: AppLanguage.current().commandError(detail))
        }
        return contents
    }

    @discardableResult
    static func run(_ arguments: [String], input: String? = nil) throws -> Data {
        try execute(binary, arguments, input: input)
    }

    static func terminal(_ arguments: [String], hold: Bool) throws {
        // Terminal user's login shell may differ. AppleScript quotes every argument,
        // then /bin/sh interprets only the fixed wrapper and passes data as argv.
        let script = """
        on run argv
            set commandLine to ""
            repeat with element in argv
                set commandLine to commandLine & " " & quoted form of (contents of element)
            end repeat
            tell application "Terminal"
                do script commandLine
                activate
            end tell
        end run
        """
        let wrapper = """
        hold=$1
        prompt=$2
        shift 2
        "$@"
        result=$?
        if [ "$hold" = yes ] || [ "$result" != 0 ]; then
            printf '\\n%s ' "$prompt"
            read -r answer
        fi
        exit "$result"
        """
        _ = try execute(URL(fileURLWithPath: "/usr/bin/osascript"), [
            "-e", script, "/bin/sh", "-c", wrapper, "sshi-terminal",
            hold ? "yes" : "no", tr("Нажмите Enter, чтобы завершить.", "Press Enter to finish."), binary.path
        ] + arguments)
    }
}

enum Panel {
    case edit(Profile), recent, token(Profile), invite(Profile), key(Profile), forward(Profile), settings
}

struct SheetItem: Identifiable {
    let id = UUID()
    enum Content {
        case editor(Panel)
        case invitation(InvitationStart)
        case socket(Profile)
    }
    let content: Content
}

@MainActor
final class Workspace: ObservableObject {
    @Published var profiles: [Profile] = []
    @Published var selected: String?
    @Published var query = ""
    @Published var sheet: SheetItem?
    @Published var feedback: String?
    @Published var busy = false
    @Published var update: ReleaseUpdate?
    @Published var checkingUpdate = false
    @Published var updateError: String?

    func checkUpdates(fetch: () async throws -> ReleaseUpdate = ReleaseUpdate.fetch) async {
        guard !checkingUpdate else { return }
        checkingUpdate = true
        updateError = nil
        defer { checkingUpdate = false }
        do {
            update = try await fetch()
        } catch { updateError = error.localizedDescription }
    }

    func offerUpdate(_ language: AppLanguage) {
        guard let update = update, update.available else { return }
        let alert = NSAlert()
        alert.messageText = language("Обновить SSHI до \(update.latest)?", "Update SSHI to \(update.latest)?")
        let steps = language("Откройте скачанный пакет .pkg, завершите установку и снова откройте SSHI.",
                             "Open the downloaded .pkg, finish the installation, and reopen SSHI.")
        alert.informativeText = update.download_url == nil
            ? language("На странице релиза выберите пакет для Mac. ", "On the release page, choose the Mac package. ") + steps
            : steps
        alert.addButton(withTitle: update.download_url == nil ? language("Открыть релиз", "Open release") : language("Скачать пакет", "Download package"))
        alert.addButton(withTitle: language("Отмена", "Cancel"))
        if alert.runModal() == .alertFirstButtonReturn {
            if !NSWorkspace.shared.open(update.download_url ?? update.release_url) {
                updateError = language("Не удалось открыть ссылку в браузере.", "Could not open the link in your browser.")
                feedback = updateError
            }
        }
    }

    var visible: [Profile] {
        guard !query.isEmpty else { return profiles }
        return profiles.filter {
            $0.name.localizedCaseInsensitiveContains(query) ||
                $0.destination.localizedCaseInsensitiveContains(query)
        }
    }

    var current: Profile? { visible.first { $0.name == selected } }

    func perform(_ work: () throws -> Void) {
        do { try work() }
        catch { feedback = error.localizedDescription }
    }

    func reload(select: String? = nil) throws {
        let result = try Client.run(["profile", "list", "--json"])
        profiles = try JSONDecoder().decode([Profile].self, from: result)
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        if let select = select {
            query = ""
            selected = select
        } else if current == nil {
            selected = visible.first?.name
        }
    }

    func launch(_ arguments: [String], hold: Bool = true, onError: ((String) -> Void)? = nil) {
        // The initial Terminal automation consent may block until the user responds.
        busy = true
        DispatchQueue.global(qos: .userInitiated).async {
            do {
                try Client.terminal(arguments, hold: hold)
                DispatchQueue.main.async { self.busy = false }
            } catch {
                DispatchQueue.main.async {
                    self.busy = false
                    if let onError = onError { onError(error.localizedDescription) }
                    else { self.feedback = error.localizedDescription }
                }
            }
        }
    }

    func copyId() {
        perform {
            let data = try Client.run(["id"])
            let id = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(id, forType: .string)
            feedback = tr("Мой Endpoint ID скопирован.", "Your Endpoint ID has been copied.") + "\n\n" + id
        }
    }

    func backup(restore: Bool) {
        let panel: NSSavePanel
        if restore {
            let open = NSOpenPanel()
            open.allowsMultipleSelection = false
            open.canChooseDirectories = false
            panel = open
        } else {
            panel = NSSavePanel()
            panel.nameFieldStringValue = "sshi-connections.json"
        }
        panel.title = restore ? tr("Импорт подключений", "Import connections") : tr("Экспорт подключений", "Export connections")
        panel.prompt = restore ? tr("Импортировать", "Import") : tr("Экспортировать", "Export")
        panel.allowedContentTypes = [.json]
        guard panel.runModal() == .OK, let url = panel.url else { return }
        perform {
            try Client.run([restore ? "restore" : "backup", url.path])
            if restore { try reload() }
            feedback = restore ? tr("Подключения импортированы.", "Connections imported.") : tr("Резервная копия сохранена.", "Backup saved.")
        }
    }

    func show(_ panel: Panel) { sheet = SheetItem(content: .editor(panel)) }
    func showInvitation(_ start: InvitationStart) { sheet = SheetItem(content: .invitation(start)) }

    func remove(_ profile: Profile) {
        let alert = NSAlert()
        alert.messageText = tr("Удалить подключение «\(profile.name)»?", "Delete the connection “\(profile.name)”?")
        alert.addButton(withTitle: tr("Отмена", "Cancel"))
        alert.addButton(withTitle: tr("Удалить", "Delete"))
        guard alert.runModal() == .alertSecondButtonReturn else { return }
        perform {
            try Client.run(["profile", "remove", "--name", profile.name])
            try reload()
            sheet = nil
        }
    }
}

struct ProfileDetail: View {
    @ObservedObject var workspace: Workspace
    let profile: Profile
    let language: AppLanguage

    private func field(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.subheadline).foregroundColor(.secondary)
            Text(value).font(.system(.body, design: .monospaced)).textSelection(.enabled)
        }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 19) {
                Image(systemName: "terminal.fill").font(.system(size: 26)).foregroundColor(.accentColor)
                Text(profile.name).font(.largeTitle.bold()).textSelection(.enabled)
                field(language("Пользователь SSH", "SSH user"), profile.user)
                field("Endpoint ID", profile.endpoint)
                field("Relay", profile.relay.isEmpty ? language("Из настроек SSHI", "From SSHI settings") : profile.relay)
                HStack(spacing: 12) {
                    Button {
                        workspace.launch(["connect", profile.name], hold: false)
                    } label: { Label(language("Открыть терминал", "Open terminal"), systemImage: "terminal") }
                        .buttonStyle(.borderedProminent).controlSize(.large)
                    Button(language("Изменить", "Edit")) { workspace.show(.edit(profile)) }
                }
                Divider()
                HStack(spacing: 16) {
                    Menu(language("Доступ", "Access")) {
                        Button(language("Токен этого relay…", "Relay token…")) { workspace.show(.token(profile)) }
                        Button(language("Ввести приглашение…", "Redeem invitation…")) {
                            workspace.showInvitation(.join(profile))
                        }
                        Button(language("Создать приглашение…", "Create invitation…")) { workspace.show(.invite(profile)) }
                        Button(language("Установить SSH-ключ…", "Install SSH key…")) { workspace.show(.key(profile)) }
                    }.fixedSize()
                    Button(language("Пробросить порт…", "Forward a port…")) { workspace.show(.forward(profile)) }
                }
                Button(language("SSH-сокет…", "SSH control socket…")) {
                    workspace.sheet = SheetItem(content: .socket(profile))
                }
            }.padding(30).frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

private final class ProfileCell: NSTableCellView {
    let name = NSTextField(labelWithString: "")
    let address = NSTextField(labelWithString: "")

    init() {
        super.init(frame: .zero)
        name.font = .systemFont(ofSize: NSFont.systemFontSize, weight: .semibold)
        name.lineBreakMode = .byTruncatingTail
        address.font = .monospacedSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        address.textColor = .secondaryLabelColor
        address.lineBreakMode = .byTruncatingTail
        textField = name
        let stack = NSStackView(views: [name, address])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 3
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor),
            name.widthAnchor.constraint(equalTo: stack.widthAnchor),
            address.widthAnchor.constraint(equalTo: stack.widthAnchor)
        ])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }
}

@MainActor
final class ProfileTable: NSTableView, NSTableViewDataSource, NSTableViewDelegate, NSTextFieldDelegate {
    private let workspace: Workspace
    private let openProfile: (String) -> Void
    private var profiles: [Profile] = []
    private var syncing = false
    private var lastClicked: String?
    private var previousMouseDown: String?
    private var doubleClickName: String?
    private var renameTimer: Timer?
    private var editingName: String?

    init(_ workspace: Workspace, openProfile: @escaping (String) -> Void) {
        self.workspace = workspace
        self.openProfile = openProfile
        super.init(frame: .zero)
        headerView = nil
        style = .sourceList
        rowHeight = 45
        allowsMultipleSelection = false
        allowsEmptySelection = false
        columnAutoresizingStyle = .lastColumnOnlyAutoresizingStyle
        let column = NSTableColumn(identifier: .init("profile"))
        column.title = tr("Подключения", "Connections")
        addTableColumn(column)
        delegate = self
        dataSource = self
        target = self
        action = #selector(singleClick(_:))
        doubleAction = #selector(doubleClick(_:))
        setAccessibilityLabel(column.title)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

    func update(_ profiles: [Profile], selected: String?, language: AppLanguage = .current()) {
        let title = language("Подключения", "Connections")
        if tableColumns.first?.title != title {
            tableColumns.first?.title = title
            setAccessibilityLabel(title)
        }
        syncing = true
        defer { syncing = false }
        if self.profiles != profiles {
            resetClicks()
            self.profiles = profiles
            reloadData()
        }
        let row = profiles.firstIndex { $0.name == selected }
        if selectedRow != (row ?? -1) {
            resetClicks()
            selectRowIndexes(row.map { IndexSet(integer: $0) } ?? [], byExtendingSelection: false)
            if let row = row { scrollRowToVisible(row) }
        }
    }

    func numberOfRows(in tableView: NSTableView) -> Int { profiles.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = makeView(withIdentifier: .init("profileCell"), owner: self) as? ProfileCell ?? ProfileCell()
        cell.identifier = .init("profileCell")
        let profile = profiles[row]
        cell.name.stringValue = profile.name
        cell.name.delegate = self
        cell.name.isEditable = false
        cell.name.isSelectable = false
        cell.address.stringValue = profile.user + "@" + String(profile.endpoint.prefix(10)) + "…"
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !syncing else { return }
        let name = profiles.indices.contains(selectedRow) ? profiles[selectedRow].name : nil
        if name != lastClicked { resetClicks() }
        workspace.selected = name
    }

    func tableView(_ tableView: NSTableView, typeSelectStringFor tableColumn: NSTableColumn?, row: Int) -> String? {
        profiles[row].name
    }

    private func resetClicks() {
        renameTimer?.invalidate()
        renameTimer = nil
        lastClicked = nil
    }

    override func mouseDown(with event: NSEvent) {
        // A second mouse-down can arrive while AppKit is still tracking its mouse-up.
        renameTimer?.invalidate()
        let row = row(at: convert(event.locationInWindow, from: nil))
        let name = profiles.indices.contains(row) ? profiles[row].name : nil
        // WindowServer counts clicks by location. Adjacent rows can share the double-click slop.
        doubleClickName = event.clickCount == 2 && name == previousMouseDown ? name : nil
        previousMouseDown = name
        if row != selectedRow { resetClicks() }
        super.mouseDown(with: event)
    }

    override func resignFirstResponder() -> Bool {
        resetClicks()
        return super.resignFirstResponder()
    }

    @objc private func singleClick(_ sender: NSTableView) {
        guard clickedRow == selectedRow, profiles.indices.contains(clickedRow), editingName == nil else { return }
        let name = profiles[clickedRow].name
        if lastClicked == name {
            renameTimer?.invalidate()
            renameTimer = Timer.scheduledTimer(timeInterval: NSEvent.doubleClickInterval,
                                                target: self, selector: #selector(startRename(_:)),
                                                userInfo: nil, repeats: false)
        }
        lastClicked = name
    }

    @objc private func doubleClick(_ sender: NSTableView) {
        resetClicks()
        guard profiles.indices.contains(clickedRow), clickedRow == selectedRow,
              profiles[clickedRow].name == doubleClickName else { return }
        openProfile(profiles[clickedRow].name)
    }

    @objc private func startRename(_ timer: Timer) {
        guard window?.isKeyWindow == true, window?.attachedSheet == nil, window?.firstResponder === self,
              profiles.indices.contains(selectedRow), lastClicked == profiles[selectedRow].name,
              let cell = view(atColumn: 0, row: selectedRow, makeIfNecessary: false) as? ProfileCell else { return }
        editingName = profiles[selectedRow].name
        resetClicks()
        cell.name.isEditable = true
        cell.name.selectText(nil)
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        if selector == #selector(NSResponder.cancelOperation(_:)), let old = editingName {
            textView.string = old
            window?.makeFirstResponder(self)
            return true
        }
        return false
    }

    func controlTextDidEndEditing(_ notification: Notification) {
        guard let field = notification.object as? NSTextField, let old = editingName else { return }
        let new = field.stringValue
        editingName = nil
        field.isEditable = false
        field.isSelectable = false
        field.stringValue = old
        let movement = (notification.userInfo?["NSTextMovement"] as? NSNumber)?.intValue
        if movement != NSCancelTextMovement && new != old {
            workspace.perform {
                try Client.run(["profile", "rename", "--name", old, "--to", new])
                try workspace.reload(select: new)
            }
        }
    }

    override func keyDown(with event: NSEvent) {
        resetClicks()
        if (event.keyCode == 36 || event.keyCode == 76) && profiles.indices.contains(selectedRow) {
            openProfile(profiles[selectedRow].name)
        } else {
            super.keyDown(with: event)
        }
    }
}

@MainActor
private struct ProfileSidebar: NSViewRepresentable {
    @ObservedObject var workspace: Workspace
    let language: AppLanguage

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        let table = ProfileTable(workspace) { workspace.launch(["connect", $0], hold: false) }
        table.frame = scroll.contentView.bounds
        table.autoresizingMask = [.width]
        scroll.documentView = table
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        (scroll.documentView as? ProfileTable)?.update(workspace.visible, selected: workspace.selected, language: language)
    }
}

@MainActor
struct WorkspaceView: View {
    @StateObject private var workspace = Workspace()
    @AppStorage(AppLanguage.preferenceKey) private var language: AppLanguage = .russian

    var body: some View {
        HSplitView {
            VStack(alignment: .leading, spacing: 0) {
                TextField(language("Поиск подключений", "Search connections"), text: $workspace.query)
                    .textFieldStyle(.roundedBorder).padding(12)
                ProfileSidebar(workspace: workspace, language: language)
            }.frame(minWidth: 185, idealWidth: 225, maxWidth: 350)
            Group {
                if let profile = workspace.current {
                    ProfileDetail(workspace: workspace, profile: profile, language: language)
                } else {
                    VStack(spacing: 14) {
                        Image(systemName: "terminal").font(.largeTitle).foregroundColor(.accentColor)
                        Text(workspace.query.isEmpty ? language("Подключения", "Connections") : language("Ничего не найдено", "No matches")).font(.title.bold())
                        if workspace.query.isEmpty {
                            Text(language("Подключитесь по приглашению или укажите адрес компьютера вручную.", "Connect with an invitation or enter a computer’s address manually."))
                                .foregroundColor(.secondary).multilineTextAlignment(.center)
                            Button(language("Новое подключение", "New connection")) { workspace.showInvitation(.join(nil)) }
                                .buttonStyle(.borderedProminent)
                            Button(language("Пригласить на этот Mac…", "Invite to this Mac…")) { workspace.showInvitation(.host) }
                        }
                    }.padding(40).frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }.frame(minWidth: 420, idealWidth: 535)
        }.frame(minHeight: 500)
        .environment(\.locale, Locale(identifier: language.rawValue))
        .onAppear {
            if CommandLine.arguments.contains("--smoke-test") {
                do {
                    try workspace.reload()
                    DispatchQueue.main.async {
                        guard NSApp.windows.contains(where: { $0.contentView != nil }) else {
                            fputs("No native window was created.\n", stderr)
                            exit(1)
                        }
                        print("SSHI native window ready")
                        exit(0)
                    }
                } catch {
                    fputs(error.localizedDescription + "\n", stderr)
                    exit(1)
                }
            } else {
                workspace.perform { try workspace.reload() }
                Task { await workspace.checkUpdates() }
            }
        }
        .onReceive(Timer.publish(every: 6 * 60 * 60, on: .main, in: .common).autoconnect()) { _ in
            if !CommandLine.arguments.contains("--smoke-test") { Task { await workspace.checkUpdates() } }
        }
        .onChange(of: workspace.query) { _ in
            if workspace.current == nil { workspace.selected = workspace.visible.first?.name }
        }
        .toolbar {
            ToolbarItemGroup(placement: .automatic) {
                if workspace.busy { ProgressView().controlSize(.small) }
                if let update = workspace.update, update.available {
                    Button(language("Обновить до \(update.latest)", "Update to \(update.latest)")) {
                        workspace.offerUpdate(language)
                    }
                }
                Button { workspace.showInvitation(.join(nil)) } label: { Label(language("Новое подключение", "New connection"), systemImage: "plus") }
                    .keyboardShortcut("n").help(language("Новое подключение", "New connection"))
                Button { workspace.show(.settings) } label: { Label(language("Настройки", "Settings"), systemImage: "gearshape") }
                    .keyboardShortcut(",", modifiers: .command).help(language("Настройки", "Settings"))
                Menu {
                    Button(language("Недавние подключения…", "Recent connections…")) { workspace.show(.recent) }
                    Button(language("Скопировать мой Endpoint ID", "Copy my Endpoint ID")) { workspace.copyId() }
                    Button(language("Пригласить на этот Mac…", "Invite to this Mac…")) { workspace.showInvitation(.host) }
                    Divider()
                    Button(language("Экспорт подключений…", "Export connections…")) { workspace.backup(restore: false) }
                    Button(language("Импорт подключений…", "Import connections…")) { workspace.backup(restore: true) }
                    Divider()
                    Link(language.whatsNew, destination: AppLanguage.releaseNotes)
                } label: { Label(language("Действия", "Actions"), systemImage: "ellipsis.circle") }
            }
        }
        .sheet(item: $workspace.sheet) { item in
            switch item.content {
            case .editor(let panel): Editor(workspace: workspace, panel: panel)
            case .invitation(let start): InvitationSheet(workspace: workspace, start: start)
            case .socket(let profile): SocketSheet(workspace: workspace, profile: profile, language: language)
            }
        }
        .alert("SSHI", isPresented: Binding(get: { workspace.feedback != nil }, set: { if !$0 { workspace.feedback = nil } })) {
            Button("OK") { workspace.feedback = nil }
        } message: { Text(workspace.feedback ?? "") }
    }
}

private struct SocketSheet: View {
    @ObservedObject var workspace: Workspace
    let profile: Profile
    let language: AppLanguage
    @State private var path = ""
    @State private var command = ""
    @State private var status = ""
    @State private var busy = false
    @State private var confirmStop = false

    private func output(_ operation: String) async throws -> String {
        let args = profile.socket(operation)
        return try await Task.detached(priority: .userInitiated) {
            String(decoding: try Client.run(args), as: UTF8.self).trimmingCharacters(in: .newlines)
        }.value
    }

    private func field(_ title: String, _ value: String, _ copy: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.headline)
            Text(value).font(.system(.body, design: .monospaced))
                .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            Button(copy) {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(value, forType: .string)
                status = language("Скопировано.", "Copied.")
            }.disabled(value.isEmpty)
        }
    }

    private func control(_ operation: String) {
        busy = true
        Task {
            defer { busy = false }
            do {
                _ = try await output(operation)
                status = operation == "check"
                    ? language("Сокет готов. Можно использовать команду SSH.", "The socket is ready. You can use the SSH command.")
                    : language("Сокет остановлен.", "The socket is stopped.")
            } catch {
                status = error.localizedDescription.localizedCaseInsensitiveContains("no active ssh control socket")
                    ? language("Сокет не запущен.", "The socket is stopped.") : error.localizedDescription
            }
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 13) {
            Text(language("SSH-сокет", "SSH control socket")).font(.title2.bold())
            Text(profile.name).font(.headline)
            Text(language("Запустите сокет и войдите через Терминал. Затем передайте команду Codex или выполните её в другом окне.",
                          "Start the socket and sign in through Terminal. Then give the command to Codex or run it in another window."))
                .fixedSize(horizontal: false, vertical: true)
            Divider()
            field(language("Путь на этом Mac", "Path on this Mac"), path, language("Скопировать путь", "Copy path"))
            field(language("Команда SSH", "SSH command"), command, language("Скопировать команду", "Copy command"))
            HStack {
                Button(language("Запустить", "Start socket")) {
                    status = language("Завершите вход в Терминале, затем нажмите «Проверить».",
                                      "Complete sign-in in Terminal, then select Check status.")
                    workspace.launch(profile.socket("start"), hold: false) { message in status = message }
                }.buttonStyle(.borderedProminent).disabled(workspace.busy)
                Button(language("Проверить", "Check status")) { control("check") }
                Button(language("Остановить…", "Stop socket…")) { confirmStop = true }
            }.disabled(busy || command.isEmpty)
            Text(language("По умолчанию сокет закрывается после 11 ч 40 мин без активных сеансов.",
                          "By default, the socket closes after 11 hours and 40 minutes without an active session."))
                .font(.callout).foregroundColor(.secondary).fixedSize(horizontal: false, vertical: true)
            if !status.isEmpty {
                Text(status).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Spacer()
                Button(language("Готово", "Done")) { workspace.sheet = nil }.keyboardShortcut(.cancelAction)
            }
        }
        .padding(24).frame(width: 510)
        .task {
            do {
                path = try await output("path")
                command = try await output("command")
            } catch { status = error.localizedDescription }
        }
        .alert(language("Остановить SSH-сокет?", "Stop this SSH control socket?"), isPresented: $confirmStop) {
            Button(language("Отмена", "Cancel"), role: .cancel) {}
            Button(language("Остановить", "Stop"), role: .destructive) { control("close") }
        } message: {
            Text(language("Сеансы, использующие этот сокет, будут закрыты.", "Sessions using this socket will disconnect."))
        }
    }
}

struct Editor: View {
    @ObservedObject var workspace: Workspace
    let panel: Panel
    @State private var name = ""
    @State private var user = ""
    @State private var endpoint = ""
    @State private var relay = ""
    @State private var token = ""
    @State private var originalRelay = ""
    @State private var path = ""
    @State private var forward = ForwardDraft()
    @AppStorage(AppLanguage.preferenceKey) private var language: AppLanguage = .russian
    @State private var error = ""
    @State private var recent: [String] = []
    @State private var selectedRecent: String?

    private var title: String {
        switch panel {
        case .edit: return language("Настройки подключения", "Connection settings")
        case .settings: return language("Настройки", "Settings")
        case .recent: return language("Сохранить недавнее подключение", "Save a recent connection")
        case .token: return language("Токен relay", "Relay token")
        case .key: return language("Установить SSH-ключ", "Install SSH key")
        case .invite: return language("Создать приглашение", "Create invitation")
        case .forward: return language("Проброс порта", "Port forwarding")
        }
    }

    private var panelWidth: CGFloat {
        if case .forward = panel { return 555 }
        return 485
    }

    private var forwardError: String? {
        guard case .forward(let profile) = panel else { return nil }
        do { _ = try forward.arguments(profile: profile.name, language: language) }
        catch { return error.localizedDescription }
        return nil
    }

    private var primary: String {
        switch panel {
        case .invite: return language("Создать", "Create")
        case .forward: return language("Открыть туннель", "Open tunnel")
        case .key: return language("Установить", "Install")
        default: return language("Сохранить", "Save")
        }
    }

    @ViewBuilder private var form: some View {
        switch panel {
        case .edit:
            TextField(language("Имя", "Name"), text: $name)
            TextField(language("Пользователь SSH", "SSH user"), text: $user)
            TextField("Endpoint ID", text: $endpoint)
            TextField(language("Relay (из настроек, если пусто)", "Relay (leave blank to use settings)"), text: $relay)
        case .recent:
            if recent.isEmpty {
                Text(language("Здесь появятся успешные SSH-подключения.", "Successful SSH connections will appear here.")).foregroundColor(.secondary)
            } else {
                Picker(language("Подключение", "Connection"), selection: $selectedRecent) {
                    ForEach(recent, id: \.self) { entry in
                        Text(entry).truncationMode(.middle).tag(Optional(entry))
                    }
                }
                TextField(language("Имя", "Name"), text: $name)
            }
        case .token(let p):
            Text(p.relay.isEmpty ? language("Relay из настроек SSHI", "Relay from SSHI settings") : p.relay).foregroundColor(.secondary)
            SecureField(language("Новый токен", "New token"), text: $token)
        case .invite(let p):
            TextField(language("Endpoint ID нового ПК", "New computer’s Endpoint ID"), text: $endpoint)
            TextField(language("Пользователь службы (по умолчанию \(p.user))", "Service user (default: \(p.user))"), text: $user)
        case .settings:
            Picker(language("Язык интерфейса", "Interface language"), selection: $language) {
                Text(verbatim: "Русский").tag(AppLanguage.russian)
                Text(verbatim: "English").tag(AppLanguage.english)
            }
            TextField(language("Relay по умолчанию", "Default relay"), text: $relay)
            SecureField(language("Заменить токен (пусто — оставить текущий)", "Replace token (leave blank to keep it)"), text: $token)
            Link(language.whatsNew, destination: AppLanguage.releaseNotes)
            HStack {
                Button(language("Проверить обновления", "Check for updates")) {
                    Task { await workspace.checkUpdates() }
                }.disabled(workspace.checkingUpdate)
                if let update = workspace.update, update.available {
                    Button(language("Обновить…", "Update…")) { workspace.offerUpdate(language) }
                }
            }
            if workspace.checkingUpdate {
                Text(language("Проверяем GitHub Releases…", "Checking GitHub Releases…"))
            } else if let message = workspace.updateError {
                Text(message).foregroundColor(.red).textSelection(.enabled)
            } else if let update = workspace.update {
                Text(update.available
                     ? language("Установлена версия \(update.current). Доступна \(update.latest).", "Version \(update.current) is installed. Version \(update.latest) is available.")
                     : language("Установлена актуальная версия \(update.current).", "Version \(update.current) is up to date."))
            }
        case .key:
            HStack {
                TextField(language("Публичный SSH-ключ", "Public SSH key"), text: $path)
                Button(language("Выбрать…", "Choose…")) {
                    let picker = NSOpenPanel()
                    picker.canChooseDirectories = false
                    picker.showsHiddenFiles = true
                    picker.title = language("Публичный SSH-ключ", "Public SSH key")
                    picker.prompt = language("Выбрать", "Choose")
                    if picker.runModal() == .OK { path = picker.url?.path ?? "" }
                }
            }
            Text(language("Пустое поле использует стандартный ключ из ~/.ssh.", "Leave this blank to use the default key in ~/.ssh.")).foregroundColor(.secondary)
        case .forward:
            EmptyView()
        }
    }

    private func load() {
        switch panel {
        case .edit(let p):
            name = p.name
            user = p.user
            endpoint = p.endpoint
            relay = p.relay
        case .recent:
            do {
                let data = try Client.run(["list"])
                recent = String(decoding: data, as: UTF8.self).split(separator: "\n").map(String.init)
                selectedRecent = recent.first
            } catch { self.error = error.localizedDescription }
        case .settings:
            do {
                let data = try Client.run(["config", "show", "--json"])
                originalRelay = try JSONDecoder().decode(RelaySettings.self, from: data).relay_url ?? ""
                relay = originalRelay
            } catch { self.error = error.localizedDescription }
        default: break
        }
    }

    private func save() {
        do {
            switch panel {
            case .edit(let old):
                var args = ["profile", "save", "--name", name, "--user", user, "--endpoint", endpoint, "--previous", old.name]
                if !relay.isEmpty { args += ["--relay", relay] }
                try Client.run(args)
                try workspace.reload(select: name)
            case .settings:
                if !token.isEmpty { try Client.run(["config", "set-token", "--relay", relay], input: token + "\n") }
                if relay != originalRelay { try Client.run(["config", "set-relay", relay]) }
            case .token(let p):
                var args = ["config", "set-token"]
                if !p.relay.isEmpty { args += ["--relay", p.relay] }
                try Client.run(args, input: token + "\n")
            case .recent:
                guard let selection = selectedRecent, let split = selection.lastIndex(of: "@") else {
                    throw ClientError(message: language("Выберите подключение.", "Select a connection."))
                }
                let user = String(selection[..<split])
                let endpoint = String(selection[selection.index(after: split)...])
                try Client.run(["profile", "save", "--name", name, "--endpoint", endpoint, "--user", user])
                try workspace.reload(select: name)
            case .invite(let p):
                var args = ["issue-invite", "--name", p.name, "--client", endpoint, "--share"]
                if !user.isEmpty { args += ["--service-user", user] }
                workspace.launch(args)
            case .key(let p):
                var args = ["copy-id"]
                if !path.isEmpty { args += ["-i", (path as NSString).expandingTildeInPath] }
                workspace.launch(p.prefix + args + [p.destination])
            case .forward(let p):
                workspace.launch(try forward.arguments(profile: p.name, language: language), hold: false)
            }
            workspace.sheet = nil
        } catch { self.error = error.localizedDescription }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text(title).font(.title2.bold())
            if case .forward(let profile) = panel {
                ForwardForm(draft: $forward, profile: profile.name, language: language)
            } else {
                Form { form }
            }
            if let message = forwardError ?? (error.isEmpty ? nil : error) {
                Text(message).foregroundColor(.red).textSelection(.enabled)
            }
            HStack {
                if case .edit(let p) = panel {
                    Button(language("Удалить…", "Delete…"), role: .destructive) { workspace.remove(p) }
                }
                Spacer()
                Button(language("Отмена", "Cancel")) { workspace.sheet = nil }.keyboardShortcut(.cancelAction)
                Button(primary) { save() }.keyboardShortcut(.defaultAction).disabled(forwardError != nil)
            }
        }.padding(24).frame(width: panelWidth)
        .environment(\.locale, Locale(identifier: language.rawValue))
        .onAppear { load() }
    }
}

#if !PROFILE_CLICK_TEST
@main
struct SSHIDesktop: App {
    var body: some Scene {
        WindowGroup("SSHI") {
            WorkspaceView().accentColor(Color(red: 0.04, green: 0.48, blue: 0.44))
        }.commands { CommandGroup(replacing: .newItem) {} }
    }
}
#endif
