import AppKit
import Combine
import Foundation
import SwiftUI
import UniformTypeIdentifiers

struct Profile: Codable, Hashable, Identifiable {
    let name: String
    let user: String
    let endpoint: String
    let relay: String
    var notes: String? = nil

    var id: String { name }
    var destination: String { "\(user)@\(endpoint)" }
    var socketKey: [String] { [name, user, endpoint, relay] }
    var prefix: [String] { ["profile", "run", "--name", name, "--"] }
    func socket(_ action: String, batch: Bool = false) -> [String] {
        ["socket", action] + (batch ? ["--batch"] : []) + ["--", name]
    }
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

    static func terminal(_ arguments: [String], hold: Bool, executable: URL = Client.binary, socketLogin: Bool = false) throws {
        // Terminal user's login shell may differ. AppleScript quotes every argument,
        // then /bin/sh interprets only the fixed wrapper and passes data as argv.
        let script = """
        on run argv
            set commandLine to "\(socketLogin ? "exec" : "")"
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
        if [ "$hold" = yes ] || { [ "$hold" != never ] && [ "$result" != 0 ]; }; then
            printf '\\n%s ' "$prompt"
            read -r answer
        fi
        exit "$result"
        """
        _ = try execute(URL(fileURLWithPath: "/usr/bin/osascript"), [
            "-e", script, "/bin/sh", "-c", wrapper, "sshi-terminal",
            hold ? "yes" : socketLogin ? "never" : "on-error", tr("Нажмите Enter, чтобы завершить.", "Press Enter to finish."), executable.path
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
        case files(Profile)
        case notes(Profile)
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
    var socketErrors: [[String]: String] = [:]
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
        let search = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !search.isEmpty else { return profiles }
        return profiles.filter {
            $0.name.localizedCaseInsensitiveContains(search) ||
                $0.destination.localizedCaseInsensitiveContains(search)
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

@MainActor
private enum WorkspaceTheme {
    static let canvas = color(0x0e1627)
    static let surface = color(0x151f33)
    static let selected = color(0x182a49)
    static let border = color(0x354964)
    static let selectedBorder = color(0x5592ff)
    static let primary = color(0x225fdf)
    static let text = color(0xf0f4ff)
    static let muted = color(0xa9b9d4)
    private static let badges = [
        ("desktopcomputer", color(0x225eb1)), ("server.rack", color(0x9a5707)),
        ("cpu", color(0xbd2755)), ("network", color(0x087d77)),
        ("terminal", color(0x6943bc)), ("externaldrive", color(0x326f9e))
    ]

    static func badge(_ profile: Profile) -> (symbol: String, color: NSColor) {
        let index = profile.name.utf8.enumerated().reduce(0) { sum, entry in
            (sum + (entry.offset + 1) * Int(entry.element)) % badges.count
        }
        return badges[index]
    }

    private static func color(_ rgb: UInt32) -> NSColor {
        NSColor(srgbRed: CGFloat((rgb >> 16) & 255) / 255, green: CGFloat((rgb >> 8) & 255) / 255,
                blue: CGFloat(rgb & 255) / 255, alpha: 1)
    }
}

@MainActor
private struct ProfileDock: View {
    @ObservedObject var workspace: Workspace
    let profile: Profile
    let language: AppLanguage
    let stacked: Bool

    var body: some View {
        Group {
            if stacked {
                VStack(alignment: .trailing, spacing: 12) {
                    details
                    actions
                }
            } else {
                HStack(spacing: 14) {
                    details
                    actions
                }
            }
        }
        .padding(12)
        .background(Color(nsColor: WorkspaceTheme.surface), in: RoundedRectangle(cornerRadius: 11))
        .overlay(RoundedRectangle(cornerRadius: 11).strokeBorder(Color(nsColor: WorkspaceTheme.border))
            .allowsHitTesting(false))
    }

    private var details: some View {
        let badge = WorkspaceTheme.badge(profile)
        let endpoint = String(profile.endpoint.prefix(8)) + "…" + String(profile.endpoint.suffix(7))
        let relay = profile.relay.isEmpty ? language("Из настроек SSHI", "From SSHI settings") : profile.relay
        return HStack(spacing: 10) {
            Image(systemName: badge.symbol).font(.system(size: 20, weight: .medium))
                .foregroundColor(.white).frame(width: 39, height: 39)
                .background(Color(nsColor: badge.color), in: RoundedRectangle(cornerRadius: 9))
                .accessibilityHidden(true)
            Text(verbatim: profile.name).font(.system(size: 16, weight: .semibold))
                .lineLimit(1).help(profile.name)
            Divider().frame(height: 34)
            field("Endpoint ID", endpoint).help(profile.endpoint)
            Button {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(profile.endpoint, forType: .string)
            } label: {
                Image(systemName: "doc.on.doc")
            }
            .help(language("Скопировать Endpoint ID", "Copy Endpoint ID"))
            .accessibilityLabel(language("Скопировать Endpoint ID", "Copy Endpoint ID"))
            Divider().frame(height: 34)
            field("Relay", relay).help(relay)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func field(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundColor(Color(nsColor: WorkspaceTheme.muted))
            Text(verbatim: value).font(.system(size: 12, design: .monospaced))
        }
        .lineLimit(1)
        .truncationMode(.middle)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var actions: some View {
        HStack(spacing: 8) {
            Menu(language("Действия", "Actions")) {
                Button(language("Изменить профиль…", "Edit profile…")) { workspace.show(.edit(profile)) }
                Button(language("Заметки…", "Notes…")) { workspace.sheet = SheetItem(content: .notes(profile)) }
                Button(language("SSH-сокет…", "SSH control socket…")) { workspace.sheet = SheetItem(content: .socket(profile)) }
                Button(language("Пробросить порт…", "Forward a port…")) { workspace.show(.forward(profile)) }
                Divider()
                Button(language("Ввести приглашение…", "Redeem invitation…")) {
                    workspace.showInvitation(.join(profile))
                }
                Button(language("Создать приглашение…", "Create invitation…")) { workspace.show(.invite(profile)) }
                Button(language("Установить SSH-ключ…", "Install SSH key…")) { workspace.show(.key(profile)) }
                Button(language("Токен этого relay…", "Relay token…")) { workspace.show(.token(profile)) }
            }
            Button {
                workspace.sheet = SheetItem(content: .files(profile))
            } label: { Label(language("Файлы", "Files"), systemImage: "folder") }
                .buttonStyle(.bordered)
            Button {
                workspace.launch(["connect", profile.name], hold: false)
            } label: { Label(language("Открыть терминал", "Open terminal"), systemImage: "arrow.up.right.square") }
                .buttonStyle(.borderedProminent)
        }
        .fixedSize()
        .controlSize(.large)
    }
}

@MainActor
private struct ProfileNotesSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var workspace: Workspace
    let profile: Profile
    let language: AppLanguage
    @State private var notes: String
    @State private var error = ""
    @FocusState private var focused: Bool

    init(workspace: Workspace, profile: Profile, language: AppLanguage) {
        self.workspace = workspace
        self.profile = profile
        self.language = language
        _notes = State(initialValue: profile.notes ?? "")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(language("Заметки", "Notes")).font(.headline)
            Text(verbatim: profile.name).foregroundColor(.secondary)
            TextEditor(text: $notes).focused($focused)
                .accessibilityLabel(language("Текст заметки", "Note text"))
                .frame(minHeight: 210).border(Color.secondary.opacity(0.22))
            if !error.isEmpty { Text(error).foregroundColor(.red).textSelection(.enabled) }
            HStack {
                Spacer()
                Button(language("Отмена", "Cancel")) { dismiss() }.keyboardShortcut(.cancelAction)
                Button(language("Сохранить", "Save")) { save() }
                    .buttonStyle(.borderedProminent).keyboardShortcut("s", modifiers: .command)
                    .disabled(notes == (profile.notes ?? ""))
            }
        }
        .padding(18).frame(width: 515)
        .onAppear { focused = true }
    }

    private func save() {
        do {
            try Client.run(["profile", "notes", "--name", profile.name, "--set"], input: notes)
            try workspace.reload()
            dismiss()
        } catch { self.error = error.localizedDescription }
    }
}

@MainActor
private final class ProfileCard: NSCollectionViewItem {
    let name = NSTextField(labelWithString: "")
    private let address = NSTextField(labelWithString: "")
    private let badge = NSImageView()

    override func loadView() {
        view = NSView()
        view.wantsLayer = true
        view.layer?.cornerRadius = 12
        badge.wantsLayer = true
        badge.layer?.cornerRadius = 9
        badge.contentTintColor = .white
        badge.imageScaling = .scaleNone
        badge.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(badge)
        name.font = .systemFont(ofSize: 17, weight: .semibold)
        name.textColor = WorkspaceTheme.text
        name.lineBreakMode = .byTruncatingTail
        address.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        address.textColor = WorkspaceTheme.muted
        address.lineBreakMode = .byTruncatingTail
        for field in [name, address] {
            field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        }
        let stack = NSStackView(views: [name, address])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 5
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            badge.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 13),
            badge.centerYAnchor.constraint(equalTo: view.centerYAnchor),
            badge.widthAnchor.constraint(equalToConstant: 46),
            badge.heightAnchor.constraint(equalTo: badge.widthAnchor),
            stack.leadingAnchor.constraint(equalTo: badge.trailingAnchor, constant: 12),
            stack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -13),
            stack.centerYAnchor.constraint(equalTo: view.centerYAnchor),
            name.widthAnchor.constraint(equalTo: stack.widthAnchor),
            address.widthAnchor.constraint(equalTo: stack.widthAnchor)
        ])
        updateSelection()
    }

    override var isSelected: Bool {
        didSet { if isViewLoaded { updateSelection() } }
    }

    func configure(_ profile: Profile, delegate: NSTextFieldDelegate) {
        let style = WorkspaceTheme.badge(profile)
        let symbol = NSImage(systemSymbolName: style.symbol, accessibilityDescription: nil)
        badge.image = symbol?.withSymbolConfiguration(.init(pointSize: 23, weight: .medium))
        badge.layer?.backgroundColor = style.color.cgColor
        name.stringValue = profile.name
        name.delegate = delegate
        name.isEditable = false
        name.isSelectable = false
        address.stringValue = profile.user + " · " + String(profile.endpoint.prefix(8)) + "…"
        view.toolTip = profile.name + "\n" + profile.destination
        view.setAccessibilityLabel(profile.name)
    }

    private func updateSelection() {
        view.layer?.backgroundColor = (isSelected ? WorkspaceTheme.selected : WorkspaceTheme.surface).cgColor
        view.layer?.borderColor = (isSelected ? WorkspaceTheme.selectedBorder : WorkspaceTheme.border).cgColor
        view.layer?.borderWidth = isSelected ? 2 : 1
    }
}

@MainActor
final class ProfileGrid: NSCollectionView, NSTextFieldDelegate {
    // AppKit forwards some collection callbacks through NSCollectionView itself.
    // Using the view as its own delegate can recurse through those selectors.
    @MainActor
    private final class CollectionProvider: NSObject, NSCollectionViewDataSource, NSCollectionViewDelegateFlowLayout {
        weak var grid: ProfileGrid?

        func collectionView(_ collectionView: NSCollectionView, numberOfItemsInSection section: Int) -> Int {
            grid?.profiles.count ?? 0
        }

        func collectionView(_ collectionView: NSCollectionView,
                            itemForRepresentedObjectAt indexPath: IndexPath) -> NSCollectionViewItem {
            let card = collectionView.makeItem(withIdentifier: ProfileGrid.cardID, for: indexPath) as! ProfileCard
            if let grid = grid {
                _ = card.view
                card.configure(grid.profiles[indexPath.item], delegate: grid)
            }
            return card
        }

        func collectionView(_ collectionView: NSCollectionView, layout collectionViewLayout: NSCollectionViewLayout,
                            sizeForItemAt indexPath: IndexPath) -> NSSize {
            let width = max(collectionView.bounds.width, 1)
            let columns = CGFloat(max(1, min(3, Int((width + 12) / 242))))
            return NSSize(width: floor((width - 12 * (columns - 1)) / columns), height: 108)
        }

        func collectionView(_ collectionView: NSCollectionView, didSelectItemsAt indexPaths: Set<IndexPath>) {
            if let index = indexPaths.first?.item { grid?.selectionChanged(to: index) }
        }
    }

    private struct CardPress {
        let name: String
        let location: NSPoint
        let count: Int
    }

    private static let cardID = NSUserInterfaceItemIdentifier("profileCard")
    private let workspace: Workspace
    private let openProfile: (String) -> Void
    private let provider = CollectionProvider()
    private var profiles: [Profile] = []
    private var syncing = false
    private var lastClicked: String?
    private var pressedCard: CardPress?
    private var renameTimer: Timer?
    private var editingName: String?

    init(_ workspace: Workspace, openProfile: @escaping (String) -> Void) {
        self.workspace = workspace
        self.openProfile = openProfile
        super.init(frame: .zero)
        let flow = NSCollectionViewFlowLayout()
        flow.minimumInteritemSpacing = 12
        flow.minimumLineSpacing = 12
        collectionViewLayout = flow
        backgroundColors = [WorkspaceTheme.canvas]
        isSelectable = true
        allowsMultipleSelection = false
        allowsEmptySelection = false
        register(ProfileCard.self, forItemWithIdentifier: Self.cardID)
        provider.grid = self
        delegate = provider
        dataSource = provider
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

    var selectedIndex: Int? { selectionIndexPaths.first?.item }

    func update(_ profiles: [Profile], selected: String?, language: AppLanguage = .current()) {
        setAccessibilityLabel(language("Подключения", "Connections"))
        syncing = true
        defer { syncing = false }
        if self.profiles != profiles {
            resetClicks()
            selectionIndexPaths = []
            self.profiles = profiles
            reloadData()
        }
        let index = profiles.firstIndex { $0.name == selected }
        if selectedIndex != index {
            resetClicks()
            let path = index.map { IndexPath(item: $0, section: 0) }
            selectionIndexPaths = path.map { Set([$0]) } ?? []
            if let path = path,
               let frame = collectionViewLayout?.layoutAttributesForItem(at: path)?.frame {
                scrollToVisible(frame)
            }
        }
    }

    override func setFrameSize(_ newSize: NSSize) {
        let widthChanged = newSize.width != frame.width
        super.setFrameSize(newSize)
        if widthChanged { collectionViewLayout?.invalidateLayout() }
    }

    private func selectionChanged(to index: Int) {
        guard !syncing, profiles.indices.contains(index) else { return }
        let name = profiles[index].name
        if name != lastClicked { resetClicks() }
        workspace.selected = name
    }

    private func resetClicks() {
        renameTimer?.invalidate()
        renameTimer = nil
        lastClicked = nil
        pressedCard = nil
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        guard let target = super.hitTest(point) else { return nil }
        // The collection handles selection across the card. The field handles inline edits.
        if target is NSTextView || (target as? NSTextField)?.isEditable == true {
            return target
        }
        return self
    }

    override func mouseDown(with event: NSEvent) {
        renameTimer?.invalidate()
        renameTimer = nil
        window?.makeFirstResponder(self)
        guard let path = indexPathForItem(at: convert(event.locationInWindow, from: nil)),
              profiles.indices.contains(path.item), editingName == nil else {
            resetClicks()
            return
        }
        let name = profiles[path.item].name
        if selectedIndex != path.item {
            resetClicks()
            selectionIndexPaths = [path]
            workspace.selected = name
        }
        // AppKit's default mouse-down tracker waits for a release before returning.
        pressedCard = CardPress(name: name, location: event.locationInWindow, count: event.clickCount)
    }

    override func mouseUp(with event: NSEvent) {
        let press = pressedCard
        pressedCard = nil
        let position = event.locationInWindow
        guard let press = press,
              abs(press.location.x - position.x) <= 6, abs(press.location.y - position.y) <= 6,
              let path = indexPathForItem(at: convert(position, from: nil)),
              path.item == selectedIndex, profiles.indices.contains(path.item),
              profiles[path.item].name == press.name else {
            resetClicks()
            return
        }
        let repeated = lastClicked == press.name
        if press.count == 2 {
            resetClicks()
            // The window server counts nearby clicks across card boundaries.
            if repeated { openProfile(press.name) }
        } else if press.count == 1 {
            if repeated {
                renameTimer = Timer.scheduledTimer(timeInterval: NSEvent.doubleClickInterval,
                                                    target: self, selector: #selector(startRename(_:)),
                                                    userInfo: nil, repeats: false)
            }
            lastClicked = press.name
        }
    }

    override func resignFirstResponder() -> Bool {
        resetClicks()
        return super.resignFirstResponder()
    }

    @objc private func startRename(_ timer: Timer) {
        guard window?.isKeyWindow == true, window?.attachedSheet == nil, window?.firstResponder === self,
              let index = selectedIndex, profiles.indices.contains(index), lastClicked == profiles[index].name
        else { return }
        beginRename(index)
    }

    private func beginRename(_ index: Int) {
        guard let card = item(at: IndexPath(item: index, section: 0)) as? ProfileCard else { return }
        editingName = profiles[index].name
        resetClicks()
        card.name.isEditable = true
        card.name.selectText(nil)
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
        if let index = selectedIndex, profiles.indices.contains(index) {
            switch event.keyCode {
            case 36, 76:
                openProfile(profiles[index].name)
                return
            case 120:  // F2
                beginRename(index)
                return
            default: break
            }
        }
        super.keyDown(with: event)
    }
}

@MainActor
private struct ProfileGallery: NSViewRepresentable {
    @ObservedObject var workspace: Workspace
    let language: AppLanguage

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        let grid = ProfileGrid(workspace) { workspace.launch(["connect", $0], hold: false) }
        grid.frame = scroll.contentView.bounds
        grid.autoresizingMask = [.width]
        scroll.documentView = grid
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        (scroll.documentView as? ProfileGrid)?.update(workspace.visible, selected: workspace.selected, language: language)
    }
}

@MainActor
struct WorkspaceView: View {
    @StateObject private var workspace = Workspace()
    @AppStorage(AppLanguage.preferenceKey) private var language: AppLanguage = .russian

    var body: some View {
        GeometryReader { area in
            VStack(alignment: .leading, spacing: 17) {
                HStack {
                    Text(language("Подключения", "Connections")).font(.system(size: 36, weight: .bold))
                    Spacer()
                    Button {
                        workspace.showInvitation(.join(nil))
                    } label: { Label(language("Новое подключение", "New connection"), systemImage: "plus") }
                        .keyboardShortcut("n").buttonStyle(.borderedProminent).controlSize(.large)
                }
                search
                if !workspace.visible.isEmpty {
                    ProfileGallery(workspace: workspace, language: language)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    Text(workspace.profiles.isEmpty
                         ? language("Добавьте подключение по приглашению или укажите Endpoint ID компьютера.",
                                    "Use an invitation or enter the computer’s Endpoint ID to add a connection.")
                         : language("По вашему запросу ничего не найдено.", "No matching connections."))
                        .foregroundColor(Color(nsColor: WorkspaceTheme.muted))
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
                if let profile = workspace.current {
                    ProfileDock(workspace: workspace, profile: profile, language: language, stacked: area.size.width < 1015)
                }
            }
            .padding(26)
            .frame(width: area.size.width, height: area.size.height)
        }
        .background(Color(nsColor: WorkspaceTheme.canvas))
        .foregroundColor(Color(nsColor: WorkspaceTheme.text))
        .tint(Color(nsColor: WorkspaceTheme.primary))
        .preferredColorScheme(.dark)
        .frame(minWidth: 575, idealWidth: 1180, minHeight: 440, idealHeight: 700)
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
            case .files(let profile): FileBrowser(profile: profile, language: language)
            case .notes(let profile): ProfileNotesSheet(workspace: workspace, profile: profile, language: language)
            case .socket(let profile):
                VStack(alignment: .leading, spacing: 14) {
                    Text(verbatim: profile.name).font(.headline)
                    SocketControls(workspace: workspace, profile: profile, language: language).id(profile.socketKey)
                    Button(language("Закрыть", "Close")) { workspace.sheet = nil }
                        .keyboardShortcut(.cancelAction).frame(maxWidth: .infinity, alignment: .trailing)
                }
                .padding(18).frame(width: 620)
            }
        }
        .alert("SSHI", isPresented: Binding(get: { workspace.feedback != nil }, set: { if !$0 { workspace.feedback = nil } })) {
            Button("OK") { workspace.feedback = nil }
        } message: { Text(workspace.feedback ?? "") }
    }

    private var search: some View {
        let caption = language("Поиск подключений", "Search connections")
        return HStack(spacing: 9) {
            Image(systemName: "magnifyingglass")
                .foregroundColor(Color(nsColor: WorkspaceTheme.muted)).accessibilityHidden(true)
            TextField(caption, text: $workspace.query).textFieldStyle(.plain)
        }
        .padding(11)
        .background(Color(nsColor: WorkspaceTheme.surface), in: RoundedRectangle(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Color(nsColor: WorkspaceTheme.border))
            .allowsHitTesting(false))
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
            WorkspaceView()
        }.commands { CommandGroup(replacing: .newItem) {} }
    }
}
#endif
