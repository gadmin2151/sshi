import AppKit
import Darwin
import Foundation
import SwiftUI

@MainActor
struct SocketSheet: View {
    @ObservedObject var workspace: Workspace
    let profile: Profile
    let language: AppLanguage
    @StateObject private var session = SocketSession()
    @State private var copied = ""
    @State private var confirmStop = false

    private func field(_ title: String, _ value: String, _ copy: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.headline)
            Text(value).font(.system(.body, design: .monospaced))
                .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            Button(copy) {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(value, forType: .string)
                copied = language("Скопировано.", "Copied.")
            }.disabled(value.isEmpty)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 6) {
                Text(language("SSH-сокет", "SSH control socket")).font(.title2.bold())
                Text(profile.name).font(.headline)
                Text(session.status).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Button(language("Запустить", "Start socket")) { session.start(profile, language) }
                    .buttonStyle(.borderedProminent).disabled(session.active || session.pending)
                Button(language("Проверить", "Check status")) { Task { await session.check(profile, language) } }
                Button(language("Остановить…", "Stop socket…")) { confirmStop = true }.disabled(!session.active)
            }.disabled(session.busy || session.command.isEmpty)
            if !session.lastError.isEmpty {
                DisclosureGroup(isExpanded: $session.showDetails) {
                    ScrollView {
                        Text(session.lastError).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }.frame(height: 76)
                } label: { Text(language("Подробности последней ошибки", "Last error details")) }
            }
            Divider()
            field(language("Путь на этом Mac", "Path on this Mac"), session.path, language("Скопировать путь", "Copy path"))
            field(language("Команда SSH", "SSH command"), session.command, language("Скопировать команду", "Copy command"))
            Text(language("По умолчанию сокет закрывается после 11 ч 40 мин без активных сеансов.",
                          "By default, the socket closes after 11 hours and 40 minutes without an active session."))
                .font(.callout).foregroundColor(.secondary).fixedSize(horizontal: false, vertical: true)
            HStack {
                Text(copied).foregroundColor(.secondary)
                Spacer()
                Button(language("Готово", "Done")) { workspace.sheet = nil }.keyboardShortcut(.cancelAction)
            }
        }
        .padding(24).frame(width: 510)
        .task {
            await session.open(profile, language)
            if session.lastError.isEmpty { session.lastError = workspace.socketErrors[session.path] ?? "" }
        }
        .onChange(of: session.lastError) { value in
            if !session.path.isEmpty && !value.isEmpty { workspace.socketErrors[session.path] = value }
        }
        .onDisappear { session.dispose() }
        .alert(language("Остановить SSH-сокет?", "Stop this SSH control socket?"), isPresented: $confirmStop) {
            Button(language("Отмена", "Cancel"), role: .cancel) {}
            Button(language("Остановить", "Stop"), role: .destructive) {
                Task { await session.check(profile, language, closing: true) }
            }
        } message: {
            Text(language("Сеансы, использующие этот сокет, будут закрыты.", "Sessions using this socket will disconnect."))
        }
    }
}

@MainActor
final class SocketSession: ObservableObject {
    typealias Run = (Profile, String) async throws -> String
    typealias Launch = (Profile, URL) async throws -> Void
    @Published var path = ""
    @Published var command = ""
    @Published var status = ""
    @Published var lastError = ""
    @Published var showDetails = false
    @Published var busy = false
    @Published var pending = false
    @Published var active = false
    private(set) var monitor: Task<Void, Never>?
    private let run: Run
    private let launch: Launch
    private var report: URL?
    private var launchTime: TimeInterval?
    private var disposed = false

    init(run: @escaping Run = SocketSession.output, launch: @escaping Launch = SocketSession.terminal) {
        self.run = run
        self.launch = launch
    }

    nonisolated static func output(_ profile: Profile, _ operation: String) async throws -> String {
        try await Task.detached(priority: .userInitiated) {
            String(decoding: try Client.run(profile.socket(operation)), as: UTF8.self).trimmingCharacters(in: .newlines)
        }.value
    }

    nonisolated static func terminal(_ profile: Profile, _ directory: URL) async throws {
        try await Task.detached(priority: .userInitiated) {
            let helper = Client.binary.deletingLastPathComponent().appendingPathComponent("sshi-socket-start")
            try Client.terminal([directory.path, Client.binary.path, profile.name], hold: false, executable: helper)
        }.value
    }

    func open(_ profile: Profile, _ language: AppLanguage) async {
        status = language("Проверяем сокет…", "Checking socket…")
        do {
            path = try await run(profile, "path")
            command = try await run(profile, "command")
            await check(profile, language)
        } catch {
            if !disposed { fail(language("Не удалось прочитать параметры сокета.", "Could not read socket details."), error.localizedDescription) }
        }
    }

    private func stopMonitoring() {
        monitor?.cancel()
        monitor = nil
        pending = false
        launchTime = nil
        if let directory = report {
            try? FileManager.default.removeItem(at: directory)
            report = nil
        }
    }

    func dispose() {
        disposed = true
        stopMonitoring()
    }

    private func fail(_ message: String, _ detail: String) {
        stopMonitoring()
        status = message
        lastError = detail.isEmpty ? message : detail
        showDetails = true
    }

    func start(_ profile: Profile, _ language: AppLanguage) {
        guard !disposed, !busy, !active, !pending, !command.isEmpty else { return }
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("sshi-start-\(UUID().uuidString)")
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false,
                                                    attributes: [.posixPermissions: 0o700])
        } catch {
            fail(language("Не удалось начать запуск.", "Could not start SSH."), error.localizedDescription)
            return
        }
        report = directory
        pending = true
        showDetails = false
        status = language("Открываем Терминал…", "Opening Terminal…")
        monitor = Task {
            do {
                // Initial macOS permission to control Terminal can take time.
                try await launch(profile, directory)
                guard !Task.isCancelled else { return }
                launchTime = ProcessInfo.processInfo.systemUptime
                let deadline = ProcessInfo.processInfo.systemUptime + 180
                repeat {
                    await check(profile, language)
                    guard pending, !Task.isCancelled else { return }
                    try await Task.sleep(nanoseconds: 1_000_000_000)
                } while ProcessInfo.processInfo.systemUptime < deadline
                status = language("Завершите вход в Терминале и нажмите «Проверить».",
                                  "Finish signing in through Terminal, then select Check status.")
            } catch {
                if !Task.isCancelled { fail(language("Не удалось открыть Терминал.", "Could not open Terminal."), error.localizedDescription) }
            }
        }
    }

    private func completion(_ language: AppLanguage) throws -> (Int, String)? {
        guard let directory = report else { return nil }
        let result = directory.appendingPathComponent("result")
        if FileManager.default.fileExists(atPath: result.path) {
            let data = try Data(contentsOf: result)
            let parts = String(decoding: data, as: UTF8.self).split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
            guard let first = parts.first, let code = Int(first) else {
                throw ClientError(message: language("Не удалось прочитать результат SSH.", "Could not read the SSH result."))
            }
            let raw = parts.count > 1 ? String(parts[1]) : ""
            let detail = raw.replacingOccurrences(of: #"\x1b\[[0-?]*[ -/]*[@-~]"#, with: "", options: .regularExpression)
            return (code, detail.trimmingCharacters(in: .whitespacesAndNewlines))
        }
        let started = directory.appendingPathComponent("started")
        if let marker = try? String(contentsOf: started, encoding: .utf8),
           let pid = Int32(marker.trimmingCharacters(in: .whitespacesAndNewlines)) {
            if kill(pid, 0) == -1 && errno == ESRCH {
                if FileManager.default.fileExists(atPath: result.path) { return try completion(language) }
                return (1, language("Окно Терминала закрылось до завершения SSH.", "Terminal closed before SSH finished."))
            }
        } else if let time = launchTime, ProcessInfo.processInfo.systemUptime - time >= 15 {
            return (1, language("Терминал не запустил SSHI.", "Terminal did not start SSHI."))
        }
        return nil
    }

    func check(_ profile: Profile, _ language: AppLanguage, closing: Bool = false) async {
        guard !disposed, !busy else { return }
        busy = true
        defer { busy = false }
        // Snapshot the terminal result before checking the master, which may
        // become ready between the check and the next poll.
        let result: (Int, String)?
        do { result = try completion(language) }
        catch { fail(language("Не удалось прочитать результат SSH.", "Could not read the SSH result."), error.localizedDescription); return }
        do {
            _ = try await run(profile, closing ? "close" : "check")
            guard !disposed else { return }
            active = !closing
            stopMonitoring()
            status = active
                ? language("Сокет готов. Нажмите «Скопировать команду» для Codex или другого терминала.",
                           "The socket is ready. Select Copy command for Codex or another terminal.")
                : language("Сокет остановлен.", "The socket is stopped.")
        } catch {
            guard !disposed else { return }
            let detail = error.localizedDescription
            let stopped = detail.localizedCaseInsensitiveContains("no active ssh control socket")
            if stopped { active = false }
            if let (code, diagnostic) = result {
                let summary = code == 0
                    ? language("SSH завершил запуск, но сокет недоступен.", "SSH finished starting, but the socket is unavailable.")
                    : language("SSH завершился с кодом \(code).", "SSH exited with status \(code).")
                fail(summary, diagnostic.isEmpty && !stopped ? detail : diagnostic)
            } else if !stopped {
                fail(closing ? language("Не удалось остановить сокет.", "Could not stop the socket.")
                     : language("Не удалось проверить сокет.", "Could not check the socket."), detail)
            } else if pending {
                status = language("Ожидаем SSH. Если Терминал запросит пароль, введите его там.",
                                  "Waiting for SSH. If Terminal asks for a password, enter it there.")
            } else {
                status = language("Сокет не запущен.", "The socket is stopped.")
            }
        }
    }
}
