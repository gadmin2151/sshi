import AppKit
import SwiftUI

struct InvitationDetails: Decodable {
    let endpoint: String
    let user: String
    let relay: String
}

@MainActor
final class InvitationEntry: ObservableObject {
    typealias Runner = @MainActor ([String], String?) async throws -> Data

    @Published var name: String
    @Published var user: String
    @Published var endpoint: String
    @Published var relay: String {
        didSet { if relay != oldValue { token = "" } }
    }
    @Published var token = ""
    @Published private(set) var input = ""
    @Published private(set) var details: InvitationDetails?
    @Published private(set) var granted = false
    let initial: Profile?
    private let run: Runner

    init(profile: Profile? = nil, run: @escaping Runner = InvitationEntry.execute) {
        initial = profile
        name = profile?.name ?? ""
        user = profile?.user ?? ""
        endpoint = profile?.endpoint ?? ""
        relay = profile?.relay ?? ""
        self.run = run
    }

    nonisolated static func execute(_ args: [String], _ input: String?) async throws -> Data {
        try await Task.detached(priority: .userInitiated) { try Client.run(args, input: input) }.value
    }

    static func isHexID(_ value: String) -> Bool {
        value.utf8.count == 64 && value.utf8.allSatisfy {
            (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0)
        }
    }

    var needsPreview: Bool { input.lowercased().hasPrefix("sshi:") && details == nil && !granted }
    var includedRelay: String? {
        guard let value = details?.relay, !value.isEmpty else { return nil }
        return value
    }

    func changeInput(_ value: String) {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed != input && !granted else { return }
        if details != nil {
            endpoint = initial?.endpoint ?? ""
            relay = initial?.relay ?? ""
        }
        details = nil
        input = trimmed
    }

    func changeEndpoint(_ value: String) {
        let new = value.trimmingCharacters(in: .whitespacesAndNewlines)
        let suggest = name.isEmpty || (!endpoint.isEmpty && name == String(endpoint.prefix(12)))
        endpoint = new
        if suggest && Self.isHexID(new) { name = String(new.prefix(12)) }
    }

    func inspect(_ language: AppLanguage) async throws {
        guard input.utf8.count <= 8192 else { throw invalidInput(language) }
        let decoded: InvitationDetails
        do {
            let data = try await run(["invitation", "inspect"], input + "\n")
            decoded = try JSONDecoder().decode(InvitationDetails.self, from: data)
        } catch {
            throw invalidInput(language)
        }
        guard initial == nil || initial?.endpoint.caseInsensitiveCompare(decoded.endpoint) == .orderedSame else {
            throw ClientError(message: language("Ссылка ведёт к другому компьютеру. Откройте её через «Новое подключение».",
                                                "This link is for another computer. Open it from New connection."))
        }
        changeEndpoint(decoded.endpoint)
        user = decoded.user
        if !decoded.relay.isEmpty { relay = decoded.relay }
        details = decoded
    }

    private func invalidInput(_ language: AppLanguage) -> ClientError {
        ClientError(message: language("Вставьте полную ссылку sshi://invite/… или код из 64 шестнадцатеричных символов.",
                                      "Paste the complete sshi://invite/… link or a 64-character hexadecimal code."))
    }

    private func validate(invited: Bool, language: AppLanguage) throws -> Profile {
        let cleanName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !cleanName.isEmpty && cleanName.utf8.count <= 256 &&
                cleanName.rangeOfCharacter(from: .controlCharacters) == nil else {
            throw ClientError(message: language("Введите имя подключения длиной до 256 байт.",
                                                "Enter a connection name of up to 256 bytes."))
        }
        let login = user.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !login.isEmpty && login.utf8.count <= 256 && !login.hasPrefix("-") && !login.contains("@") &&
                login.rangeOfCharacter(from: CharacterSet.whitespacesAndNewlines.union(.controlCharacters)) == nil else {
            throw ClientError(message: language("Введите имя пользователя SSH на принимающем компьютере.",
                                                "Enter an SSH username on the receiving computer."))
        }
        let peer = ((invited ? details?.endpoint : nil) ?? endpoint).trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard invited ? Self.isHexID(peer) : !peer.isEmpty else {
            throw ClientError(message: language("Вставьте Endpoint ID принимающего компьютера (64 шестнадцатеричных символа).",
                                                "Paste the receiving computer’s Endpoint ID (64 hexadecimal characters)."))
        }
        let url = ((invited ? includedRelay : nil) ?? relay).trimmingCharacters(in: .whitespacesAndNewlines)
        if !url.isEmpty {
            guard let parts = URLComponents(string: url), ["https", "http"].contains(parts.scheme?.lowercased() ?? ""),
                  let host = parts.host, !host.isEmpty, parts.user == nil, parts.password == nil,
                  parts.query == nil, parts.fragment == nil else {
                throw ClientError(message: language("Проверьте адрес relay. Введите HTTP- или HTTPS-адрес без учётных данных.",
                                                    "Check the relay address. Enter an HTTP or HTTPS URL without credentials."))
            }
        }
        return Profile(name: cleanName, user: login, endpoint: peer, relay: url)
    }

    func finish(invited: Bool, language: AppLanguage) async throws -> String {
        if invited && !granted && details == nil && !Self.isHexID(input) { throw invalidInput(language) }
        let profile = try validate(invited: invited, language: language)
        let data = try await run(["profile", "list", "--json"], nil)
        let profiles = try JSONDecoder().decode([Profile].self, from: data)
        let existing = profiles.first { $0.name.caseInsensitiveCompare(profile.name) == .orderedSame }
        if let other = existing, other.user != profile.user || other.endpoint != profile.endpoint || other.relay != profile.relay {
            throw ClientError(message: language("Это имя занято другим подключением. Введите новое имя.",
                                                "Another connection uses this name. Enter a different name."))
        }
        if invited && !granted {
            if !token.isEmpty {
                if token.rangeOfCharacter(from: CharacterSet.whitespacesAndNewlines.union(.controlCharacters)) != nil {
                    throw ClientError(message: language("Токен relay должен быть одной строкой без пробелов.",
                                                        "The relay token must be one value without spaces or line breaks."))
                }
                let args = ["config", "set-token"] + (profile.relay.isEmpty ? [] : ["--relay", profile.relay])
                _ = try await run(args, token + "\n")
                token = ""
            }
            var args = ["invitation", "redeem"]
            if details == nil { args += ["--endpoint", profile.endpoint] }
            if includedRelay == nil && !profile.relay.isEmpty { args += ["--relay", profile.relay] }
            do { _ = try await run(args, input + "\n") }
            catch {
                let message = error.localizedDescription.lowercased()
                if message.contains("denied") || message.contains("expired") {
                    throw ClientError(message: language("Получатель отклонил приглашение. Проверьте срок действия и попросите выдать код для Endpoint ID этого Mac.",
                                                        "The receiver declined the invitation. Check its expiry and ask for a code issued to this Mac’s Endpoint ID."))
                }
                if message.contains("relay") && (message.contains("authentication") || message.contains("token")) {
                    throw ClientError(message: language("Relay отклонил авторизацию. Проверьте токен в настройках relay ниже.",
                                                        "The relay rejected authentication. Check the token under Relay settings below."))
                }
                throw ClientError(message: language("Не удалось связаться со службой приглашений. Проверьте Endpoint ID, relay и службу sshi на принимающем компьютере.",
                                                    "Could not reach the invitation service. Check the receiver’s Endpoint ID, relay and running sshi service."))
            }
            granted = true
            input = ""
        }
        if let existing = existing { return existing.name }
        var args = ["profile", "save", "--name", profile.name, "--user", profile.user, "--endpoint", profile.endpoint]
        if !profile.relay.isEmpty { args += ["--relay", profile.relay] }
        _ = try await run(args, nil)
        return profile.name
    }
}

enum InvitationStart {
    case join(Profile?)
    case host
}

private func clipboard(_ value: String, confidential: Bool = false) {
    let item = NSPasteboardItem()
    item.setString(value, forType: .string)
    if confidential { item.setData(Data(), forType: .init("org.nspasteboard.ConcealedType")) }
    NSPasteboard.general.clearContents()
    NSPasteboard.general.writeObjects([item])
}

@MainActor
struct InvitationSheet: View {
    @ObservedObject var workspace: Workspace
    @AppStorage(AppLanguage.preferenceKey) private var language: AppLanguage = .russian
    @StateObject private var entry: InvitationEntry
    @State private var hosting: Bool
    @State private var manual = false
    @State private var relayOptions = false
    @State private var working = false
    @State private var error = ""
    @State private var ownID = ""
    @State private var copiedID = false
    @State private var guestID = ""
    @State private var shareLink = ""
    @State private var copiedLink = false
    private let startsOnHost: Bool

    init(workspace: Workspace, start: InvitationStart) {
        self.workspace = workspace
        switch start {
        case .join(let profile):
            _entry = StateObject(wrappedValue: InvitationEntry(profile: profile))
            _hosting = State(initialValue: false)
            startsOnHost = false
        case .host:
            _entry = StateObject(wrappedValue: InvitationEntry())
            _hosting = State(initialValue: true)
            startsOnHost = true
        }
    }

    private func perform(_ action: @escaping @MainActor () async throws -> Void) {
        guard !working else { return }
        working = true
        error = ""
        Task {
            defer { working = false }
            do { try await action() }
            catch { self.error = error.localizedDescription }
        }
    }

    private func paste(_ receive: (String) -> Void) {
        guard let text = NSPasteboard.general.string(forType: .string),
              !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            error = language("В буфере обмена нет текста.", "There is no text on the clipboard.")
            return
        }
        error = ""
        receive(text.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    private func join() {
        if !manual && entry.needsPreview {
            perform { try await entry.inspect(language) }
            return
        }
        perform {
            let name = try await entry.finish(invited: !manual, language: language)
            try workspace.reload(select: name)
            workspace.sheet = nil
            if !manual { workspace.launch(["connect", name], hold: false) }
        }
    }

    private func createInvitation() {
        perform {
            let guest = guestID.trimmingCharacters(in: .whitespacesAndNewlines)
            guard InvitationEntry.isHexID(guest) else {
                throw ClientError(message: language("Вставьте Endpoint ID компьютера гостя (64 шестнадцатеричных символа).",
                                                    "Paste the guest computer’s 64-character hexadecimal Endpoint ID."))
            }
            let data = try await InvitationEntry.execute(["invite", guest, "--share", "--user", NSUserName()], nil)
            let value = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
            guard value.hasPrefix("sshi://invite/v1?"), !value.contains("\n") else {
                throw ClientError(message: language("Не удалось прочитать ссылку приглашения.", "Could not read the invitation link."))
            }
            shareLink = value
            copiedLink = false
        }
    }

    private func label(_ text: String) -> some View { Text(text).font(.headline) }

    private func pasteButton(_ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Label(language("Вставить", "Paste"), systemImage: "doc.on.clipboard")
        }
    }

    private var myIdentity: some View {
        HStack {
            VStack(alignment: .leading, spacing: 3) {
                Text(language("Мой Endpoint ID", "My Endpoint ID")).font(.headline)
                Text(language("Отправьте администратору, чтобы он выдал код для этого Mac.",
                              "Send this to the administrator to request an invitation for this Mac."))
                    .font(.caption).foregroundColor(.secondary)
            }
            Spacer()
            Button {
                perform {
                    if ownID.isEmpty {
                        let result = try await InvitationEntry.execute(["id"], nil)
                        ownID = String(decoding: result, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
                    }
                    clipboard(ownID)
                    copiedID = true
                }
            } label: {
                Label(copiedID ? language("Скопировано", "Copied") : language("Копировать ID", "Copy ID"),
                      systemImage: copiedID ? "checkmark" : "doc.on.doc")
            }
        }
    }

    private var connectionNameAndUser: some View {
        HStack {
            VStack(alignment: .leading) {
                label(language("Имя подключения", "Connection name"))
                TextField(language("Например, Raspberry Pi", "For example, Raspberry Pi"), text: $entry.name)
            }
            VStack(alignment: .leading) {
                label(language("Пользователь SSH", "SSH user"))
                TextField(language("Например, pi", "For example, pi"), text: $entry.user)
            }.frame(width: 175)
        }
    }

    private var receiver: some View {
        VStack(alignment: .leading) {
            label(language("Endpoint ID получателя", "Receiver Endpoint ID"))
            HStack {
                TextField(language("ID удалённого компьютера", "Remote computer ID"),
                          text: Binding(get: { entry.endpoint }, set: { entry.changeEndpoint($0) }))
                    .font(.system(.body, design: .monospaced))
                    .accessibilityLabel(language("Endpoint ID получателя", "Receiver Endpoint ID"))
                pasteButton { paste { entry.changeEndpoint($0) } }
            }.disabled(!manual && (entry.initial != nil || entry.details != nil || entry.granted))
        }
    }

    private var invitationCode: some View {
        VStack(alignment: .leading) {
            label(language("Код или ссылка приглашения", "Invitation code or link"))
            HStack {
                SecureField(language("Одноразовый код или sshi://invite/…", "One-time code or sshi://invite/…"),
                            text: Binding(get: { entry.input }, set: { entry.changeInput($0) }))
                    .accessibilityLabel(language("Код или ссылка приглашения", "Invitation code or link"))
                pasteButton {
                    paste { value in
                        entry.changeInput(value)
                        if entry.needsPreview { perform { try await entry.inspect(language) } }
                    }
                }
            }.disabled(entry.granted)
        }
    }

    @ViewBuilder private var joining: some View {
        if entry.initial == nil {
            Picker(language("Способ подключения", "Connection method"), selection: $manual) {
                Text(language("По приглашению", "Invitation")).tag(false)
                Text(language("Вручную", "Manual")).tag(true)
            }.pickerStyle(.segmented).labelsHidden().disabled(entry.granted)
                .onChange(of: manual) { value in
                    error = ""
                    if !value, let details = entry.details {
                        entry.changeEndpoint(details.endpoint)
                        if !details.relay.isEmpty { entry.relay = details.relay }
                    }
                }
        }
        if !manual {
            Text(language("Вставьте ID получателя и выданный вам код. Полная ссылка заполнит данные получателя.",
                          "Paste the receiver’s ID and your invitation code. A complete link fills in the receiver details."))
                .foregroundColor(.secondary)
        }
        receiver
        if !manual {
            if entry.granted {
                Label(language("Этот Mac уже допущен. Осталось сохранить подключение.",
                               "This Mac has been authorized. Finish saving the connection."),
                      systemImage: "checkmark.circle")
            } else { invitationCode }
        }
        connectionNameAndUser
        if manual {
            label("Relay")
            TextField(language("По умолчанию из настроек SSHI", "Default from SSHI settings"), text: $entry.relay)
        } else {
            if entry.details != nil {
                HStack {
                    Text("Relay").foregroundColor(.secondary)
                    Text(entry.relay.isEmpty ? language("Из настроек SSHI", "From SSHI settings") : entry.relay)
                        .textSelection(.enabled).lineLimit(2)
                }
            }
            DisclosureGroup(language("Настройки relay", "Relay settings"), isExpanded: $relayOptions) {
                VStack(alignment: .leading, spacing: 6) {
                    if entry.includedRelay == nil {
                        TextField(language("Адрес relay (по умолчанию из настроек)", "Relay URL (default from settings)"),
                                  text: $entry.relay)
                    }
                    SecureField(language("Токен этого relay (при необходимости)", "Token for this relay (if required)"),
                                text: $entry.token)
                }.padding(.top, 5)
            }.disabled(entry.granted)
            Divider()
            myIdentity
        }
    }

    @ViewBuilder private var host: some View {
        Label(language("Пользователь SSH на этом Mac", "SSH user on this Mac") + " · " + NSUserName(), systemImage: "desktopcomputer")
            .font(.headline)
        Text(language("Включите «Удалённый вход» в настройках общего доступа macOS и запустите sshi serve от текущего пользователя. Оставьте службу запущенной, пока гость подключается.",
                      "Enable Remote Login in macOS Sharing settings and run sshi serve as the current user. Keep the service running while the guest connects."))
            .textSelection(.enabled)
        label(language("Endpoint ID гостя", "Guest Endpoint ID"))
        HStack {
            TextField(language("Скопирован на компьютере гостя", "Copied on the guest computer"), text: $guestID)
                .font(.system(.body, design: .monospaced))
            pasteButton { paste { guestID = $0 } }
        }.disabled(!shareLink.isEmpty)
        if !shareLink.isEmpty {
            Divider()
            Label(language("Приглашение готово", "Invitation ready"), systemImage: "checkmark.circle")
                .font(.headline)
            Text(language("Отправьте ссылку гостю. Она содержит адрес этого Mac, пользователя SSH, relay и одноразовый код.",
                          "Send the link to the guest. It includes this Mac’s address, SSH user, relay and one-time code."))
            Button {
                clipboard(shareLink, confidential: true)
                copiedLink = true
            } label: {
                Label(copiedLink ? language("Ссылка скопирована", "Link copied") : language("Скопировать ссылку", "Copy invitation link"),
                      systemImage: copiedLink ? "checkmark" : "doc.on.doc")
            }.buttonStyle(.borderedProminent)
        }
    }

    private var primary: String {
        if hosting { return shareLink.isEmpty ? language("Создать приглашение", "Create invitation") : language("Готово", "Done") }
        if manual { return language("Сохранить", "Save") }
        if entry.needsPreview { return language("Проверить ссылку", "Preview link") }
        if entry.granted { return language("Сохранить и подключиться", "Save and connect") }
        return language("Принять и подключиться", "Accept and connect")
    }

    private var title: String {
        if hosting { return language("Пригласить на этот Mac", "Invite someone to this Mac") }
        if let profile = entry.initial { return language("Приглашение", "Invitation") + " · " + profile.name }
        return language("Новое подключение", "New connection")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(title).font(.title2.bold())
            Group {
                if hosting { host } else { joining }
            }.disabled(working)
            if !error.isEmpty { Text(error).foregroundColor(.red).textSelection(.enabled) }
            Divider()
            HStack {
                if !hosting && entry.initial == nil && !entry.granted {
                    Button(language("Пригласить на этот Mac…", "Invite to this Mac…")) { hosting = true; error = "" }
                } else if hosting && !startsOnHost && shareLink.isEmpty {
                    Button(language("Назад", "Back")) { hosting = false; error = "" }
                }
                Spacer()
                if working { ProgressView().controlSize(.small) }
                Button(language("Отмена", "Cancel")) { workspace.sheet = nil }.keyboardShortcut(.cancelAction)
                Button(primary) {
                    if hosting {
                        if shareLink.isEmpty { createInvitation() } else { workspace.sheet = nil }
                    } else { join() }
                }.keyboardShortcut(.defaultAction)
            }.disabled(working)
        }
        .padding(24).frame(width: 590).textFieldStyle(.roundedBorder)
        .interactiveDismissDisabled(working)
        .environment(\.locale, Locale(identifier: language.rawValue))
    }
}
