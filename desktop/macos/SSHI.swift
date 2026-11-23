import AppKit
import Foundation
import SwiftUI
import UniformTypeIdentifiers

struct Profile: Codable, Identifiable {
    let name: String
    let user: String
    let endpoint: String
    let relay: String

    var id: String { name }
    var destination: String { "\(user)@\(endpoint)" }
    var prefix: [String] { ["profile", "run", "--name", name, "--"] }
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
            throw ClientError(message: detail.isEmpty ? "Команда завершилась с ошибкой." : detail)
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
        shift
        "$@"
        result=$?
        if [ "$hold" = yes ] || [ "$result" != 0 ]; then
            printf '\\nНажмите Enter, чтобы завершить. '
            read -r answer
        fi
        exit "$result"
        """
        _ = try execute(URL(fileURLWithPath: "/usr/bin/osascript"), [
            "-e", script, "/bin/sh", "-c", wrapper, "sshi-terminal",
            hold ? "yes" : "no", binary.path
        ] + arguments)
    }
}

enum Panel {
    case edit(Profile?), recent, token(Profile), invite(Profile), key(Profile), forward(Profile), settings
}

struct SheetItem: Identifiable {
    let id = UUID()
    let panel: Panel
}

@MainActor
final class Workspace: ObservableObject {
    @Published var profiles: [Profile] = []
    @Published var selected: String?
    @Published var query = ""
    @Published var sheet: SheetItem?
    @Published var feedback: String?
    @Published var busy = false

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

    func launch(_ arguments: [String], hold: Bool = true) {
        // The initial Terminal automation consent may block until the user responds.
        busy = true
        DispatchQueue.global(qos: .userInitiated).async {
            do {
                try Client.terminal(arguments, hold: hold)
                DispatchQueue.main.async { self.busy = false }
            } catch {
                DispatchQueue.main.async {
                    self.busy = false
                    self.feedback = error.localizedDescription
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
            feedback = "Мой Endpoint ID скопирован.\n\n" + id
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
        panel.title = restore ? "Импорт подключений" : "Экспорт подключений"
        panel.allowedContentTypes = [.json]
        guard panel.runModal() == .OK, let url = panel.url else { return }
        perform {
            try Client.run([restore ? "restore" : "backup", url.path])
            if restore { try reload() }
            feedback = restore ? "Подключения импортированы." : "Резервная копия сохранена."
        }
    }

    func show(_ panel: Panel) { sheet = SheetItem(panel: panel) }

    func remove(_ profile: Profile) {
        let alert = NSAlert()
        alert.messageText = "Удалить подключение «\(profile.name)»?"
        alert.addButton(withTitle: "Отмена")
        alert.addButton(withTitle: "Удалить")
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
                field("Пользователь SSH", profile.user)
                field("Endpoint ID", profile.endpoint)
                field("Relay", profile.relay.isEmpty ? "Из настроек SSHI" : profile.relay)
                HStack(spacing: 12) {
                    Button {
                        workspace.launch(["connect", profile.name], hold: false)
                    } label: { Label("Открыть терминал", systemImage: "terminal") }
                        .buttonStyle(.borderedProminent).controlSize(.large)
                    Button("Изменить") { workspace.show(.edit(profile)) }
                }
                Divider()
                HStack(spacing: 16) {
                    Menu("Доступ") {
                        Button("Токен этого relay…") { workspace.show(.token(profile)) }
                        Button("Ввести приглашение…") {
                            workspace.launch(profile.prefix + ["copy-iid", "--invite", profile.destination])
                        }
                        Button("Создать приглашение…") { workspace.show(.invite(profile)) }
                        Button("Установить SSH-ключ…") { workspace.show(.key(profile)) }
                    }.fixedSize()
                    Button("Пробросить порт…") { workspace.show(.forward(profile)) }
                }
            }.padding(30).frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

@MainActor
struct WorkspaceView: View {
    @StateObject private var workspace = Workspace()

    var body: some View {
        HSplitView {
            VStack(alignment: .leading, spacing: 0) {
                TextField("Поиск подключений", text: $workspace.query)
                    .textFieldStyle(.roundedBorder).padding(12)
                List(selection: $workspace.selected) {
                    ForEach(workspace.visible) { profile in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(profile.name).font(.headline)
                            Text(profile.user + "@" + String(profile.endpoint.prefix(10)) + "…")
                                .font(.system(.caption, design: .monospaced)).foregroundColor(.secondary)
                        }.padding(.vertical, 4).tag(profile.name)
                    }
                }.listStyle(.sidebar)
            }.frame(minWidth: 185, idealWidth: 225, maxWidth: 350)
            Group {
                if let profile = workspace.current {
                    ProfileDetail(workspace: workspace, profile: profile)
                } else {
                    VStack(spacing: 14) {
                        Image(systemName: "terminal").font(.largeTitle).foregroundColor(.accentColor)
                        Text(workspace.query.isEmpty ? "Подключения" : "Ничего не найдено").font(.title.bold())
                        if workspace.query.isEmpty {
                            Text("Сохраните имя, пользователя SSH и Endpoint ID удалённого компьютера.")
                                .foregroundColor(.secondary).multilineTextAlignment(.center)
                            Button("Новое подключение") { workspace.show(.edit(nil)) }
                                .buttonStyle(.borderedProminent)
                        }
                    }.padding(40).frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }.frame(minWidth: 420, idealWidth: 535)
        }.frame(minHeight: 500)
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
            }
        }
        .onChange(of: workspace.query) { _ in
            if workspace.current == nil { workspace.selected = workspace.visible.first?.name }
        }
        .toolbar {
            ToolbarItemGroup(placement: .automatic) {
                if workspace.busy { ProgressView().controlSize(.small) }
                Button { workspace.show(.edit(nil)) } label: { Label("Новое подключение", systemImage: "plus") }
                    .keyboardShortcut("n").help("Новое подключение")
                Menu {
                    Button("Недавние подключения…") { workspace.show(.recent) }
                    Button("Скопировать мой Endpoint ID") { workspace.copyId() }
                    Button("Настройки relay…") { workspace.show(.settings) }
                    Divider()
                    Button("Экспорт подключений…") { workspace.backup(restore: false) }
                    Button("Импорт подключений…") { workspace.backup(restore: true) }
                } label: { Label("Настройки и данные", systemImage: "ellipsis.circle") }
            }
        }
        .sheet(item: $workspace.sheet) { item in Editor(workspace: workspace, panel: item.panel) }
        .alert("SSHI", isPresented: Binding(get: { workspace.feedback != nil }, set: { if !$0 { workspace.feedback = nil } })) {
            Button("OK") { workspace.feedback = nil }
        } message: { Text(workspace.feedback ?? "") }
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
    @State private var flag = "L"
    @State private var rule = ""
    @State private var error = ""
    @State private var recent: [String] = []
    @State private var selectedRecent: String?

    private var title: String {
        switch panel {
        case .edit(let old): return old == nil ? "Новое подключение" : "Настройки подключения"
        case .settings: return "Настройки relay"
        case .recent: return "Сохранить недавнее подключение"
        case .token: return "Токен relay"
        case .key: return "Установить SSH-ключ"
        case .invite: return "Создать приглашение"
        case .forward: return "Проброс порта"
        }
    }

    private var primary: String {
        switch panel {
        case .invite: return "Создать"
        case .forward: return "Открыть туннель"
        case .key: return "Установить"
        default: return "Сохранить"
        }
    }

    @ViewBuilder private var form: some View {
        switch panel {
        case .edit:
            TextField("Имя", text: $name)
            TextField("Пользователь SSH", text: $user)
            TextField("Endpoint ID", text: $endpoint)
            TextField("Relay (из настроек, если пусто)", text: $relay)
        case .recent:
            if recent.isEmpty {
                Text("Здесь появятся успешные SSH-подключения.").foregroundColor(.secondary)
            } else {
                Picker("Подключение", selection: $selectedRecent) {
                    ForEach(recent, id: \.self) { entry in
                        Text(entry).truncationMode(.middle).tag(Optional(entry))
                    }
                }
                TextField("Имя", text: $name)
            }
        case .token(let p):
            Text(p.relay.isEmpty ? "Relay из настроек SSHI" : p.relay).foregroundColor(.secondary)
            SecureField("Новый токен", text: $token)
        case .invite(let p):
            TextField("Endpoint ID нового ПК", text: $endpoint)
            TextField("Пользователь службы (по умолчанию \(p.user))", text: $user)
        case .settings:
            TextField("Relay по умолчанию", text: $relay)
            SecureField("Заменить токен (пусто — оставить текущий)", text: $token)
        case .key:
            HStack {
                TextField("Публичный SSH-ключ", text: $path)
                Button("Выбрать…") {
                    let picker = NSOpenPanel()
                    picker.canChooseDirectories = false
                    picker.showsHiddenFiles = true
                    if picker.runModal() == .OK { path = picker.url?.path ?? "" }
                }
            }
            Text("Пустое поле использует стандартный ключ из ~/.ssh.").foregroundColor(.secondary)
        case .forward:
            Picker("Направление", selection: $flag) {
                Text("Локальный").tag("L")
                Text("Удалённый").tag("R")
                Text("SOCKS").tag("D")
            }
            TextField("Правило OpenSSH", text: $rule)
        }
    }

    private func load() {
        switch panel {
        case .edit(let p):
            if let p = p {
                name = p.name
                user = p.user
                endpoint = p.endpoint
                relay = p.relay
            }
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
                var args = ["profile", "save", "--name", name, "--user", user, "--endpoint", endpoint]
                if let old = old { args += ["--previous", old.name] }
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
                    throw ClientError(message: "Выберите подключение.")
                }
                let user = String(selection[..<split])
                let endpoint = String(selection[selection.index(after: split)...])
                try Client.run(["profile", "save", "--name", name, "--endpoint", endpoint, "--user", user])
                try workspace.reload(select: name)
            case .invite(let p):
                var args = ["issue-invite", "--name", p.name, "--client", endpoint]
                if !user.isEmpty { args += ["--service-user", user] }
                workspace.launch(args)
            case .key(let p):
                var args = ["copy-id"]
                if !path.isEmpty { args += ["-i", (path as NSString).expandingTildeInPath] }
                workspace.launch(p.prefix + args + [p.destination])
            case .forward(let p):
                guard !rule.isEmpty else { throw ClientError(message: "Укажите правило OpenSSH.") }
                workspace.launch(p.prefix + ["-N", "-" + flag, rule, p.destination])
            }
            workspace.sheet = nil
        } catch { self.error = error.localizedDescription }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text(title).font(.title2.bold())
            Form { form }
            if !error.isEmpty { Text(error).foregroundColor(.red).textSelection(.enabled) }
            HStack {
                if case .edit(let old) = panel, let p = old {
                    Button("Удалить…", role: .destructive) { workspace.remove(p) }
                }
                Spacer()
                Button("Отмена") { workspace.sheet = nil }.keyboardShortcut(.cancelAction)
                Button(primary) { save() }.keyboardShortcut(.defaultAction)
            }
        }.padding(24).frame(width: 485)
        .onAppear { load() }
    }
}

@main
struct SSHIDesktop: App {
    var body: some Scene {
        WindowGroup("SSHI") {
            WorkspaceView().accentColor(Color(red: 0.04, green: 0.48, blue: 0.44))
        }.commands { CommandGroup(replacing: .newItem) {} }
    }
}
