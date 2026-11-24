import AppKit
import Darwin
import Foundation
import SwiftUI

@MainActor
struct SocketControls: View {
    @ObservedObject var workspace: Workspace
    let profile: Profile
    let language: AppLanguage
    @StateObject private var session = SocketSession()
    @State private var feedback = ""
    @State private var confirming = false

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 8) {
                Text(language("SSH-сокет", "SSH control socket")).font(.headline)
                Text(session.status.isEmpty ? language("Проверяем…", "Checking…") : session.status)
                    .font(.caption.weight(.medium)).padding(.horizontal, 8).padding(.vertical, 3)
                    .background(session.active ? Color.accentColor.opacity(0.19) : Color.secondary.opacity(0.12), in: Capsule())
            }
            if confirming && session.active {
                HStack {
                    Button(language("Остановить", "Stop"), role: .destructive) {
                        confirming = false
                        Task { await session.check(profile, language, closing: true) }
                    }
                    Button(language("Отмена", "Cancel")) { confirming = false }
                }.disabled(session.busy)
                Text(language("Сеансы через этот сокет завершатся.", "Sessions using this socket will disconnect."))
                    .font(.callout).foregroundColor(.secondary)
            } else if session.active {
                HStack {
                    Button {
                        copy("command")
                    } label: { Label(language("Скопировать команду", "Copy command"), systemImage: "doc.on.doc") }
                    Button(language("Путь", "Path")) { copy("path") }
                        .help(language("Скопировать путь к сокету", "Copy socket path"))
                    Button(language("Остановить…", "Stop socket…")) { confirming = true }
                }.disabled(session.busy)
            } else if !session.pending {
                HStack {
                    Button(language("Включить", "Enable")) { session.start(profile, language) }
                        .buttonStyle(.borderedProminent)
                    if session.terminalRetry {
                        Button(language("Войти в терминале…", "Sign in via Terminal…")) {
                            session.start(profile, language, interactive: true)
                        }
                    }
                }.disabled(session.busy || session.status.isEmpty)
            }
            if !feedback.isEmpty {
                Text(feedback).font(.callout).foregroundColor(.secondary)
            }
            if !session.lastError.isEmpty {
                DisclosureGroup(isExpanded: $session.showDetails) {
                    Text(session.lastError).font(.system(.callout, design: .monospaced)).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } label: {
                    Text(language("Ошибка сокета", "Socket error"))
                }
            }
        }
        .padding(14).frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
        .task {
            if let error = workspace.socketErrors[profile] {
                session.lastError = error
                session.terminalRetry = true
            }
            await session.check(profile, language)
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 5_000_000_000) }
                catch { return }
                if !session.pending { await session.check(profile, language) }
            }
        }
        .onChange(of: session.active) { _ in
            feedback = ""
            confirming = false
        }
        .onChange(of: session.lastError) { value in
            workspace.socketErrors[profile] = value.isEmpty ? nil : value
        }
        .onDisappear { session.dispose() }
    }

    private func copy(_ field: String) {
        Task {
            do {
                let value = try await session.value(profile, field)
                guard session.active else { return }
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(value, forType: .string)
                feedback = language("Скопировано.", "Copied.")
            } catch {
                session.lastError = error.localizedDescription
            }
        }
    }
}

@MainActor
final class SocketSession: ObservableObject {
    typealias Run = (Profile, String) async throws -> String
    typealias Launch = (Profile, URL) async throws -> Void
    @Published var status = ""
    @Published var lastError = ""
    @Published var showDetails = false
    @Published var busy = false
    @Published var pending = false
    @Published var active = false
    @Published var terminalRetry = false
    private(set) var monitor: Task<Void, Never>?
    private let run: Run
    private let launch: Launch
    private var report: URL?
    private var launchTime: TimeInterval?

    init(run: @escaping Run = SocketSession.output, launch: @escaping Launch = SocketSession.terminal) {
        self.run = run
        self.launch = launch
    }

    nonisolated static func output(_ profile: Profile, _ operation: String) async throws -> String {
        try await Task.detached(priority: .userInitiated) {
            let args = operation == "batch" ? profile.socket("start", batch: true) : profile.socket(operation)
            return String(decoding: try Client.run(args), as: UTF8.self).trimmingCharacters(in: .newlines)
        }.value
    }

    nonisolated static func terminal(_ profile: Profile, _ directory: URL) async throws {
        try await Task.detached(priority: .userInitiated) {
            let helper = Client.binary.deletingLastPathComponent().appendingPathComponent("sshi-socket-start")
            try Client.terminal([directory.path, Client.binary.path, profile.name], hold: false, executable: helper, socketLogin: true)
        }.value
    }

    func value(_ profile: Profile, _ field: String) async throws -> String {
        try await run(profile, field)
    }

    private func stopMonitoring() {
        monitor = nil
        pending = false
        launchTime = nil
        if let directory = report {
            try? FileManager.default.removeItem(at: directory)
            report = nil
        }
    }

    func dispose() {
        monitor?.cancel()
        stopMonitoring()
    }

    private func setState(_ active: Bool, _ language: AppLanguage) {
        self.active = active
        status = active ? language("Включён", "On") : language("Выключен", "Off")
        if active {
            lastError = ""
            terminalRetry = false
        }
    }

    private func fail(_ detail: String, _ language: AppLanguage) {
        stopMonitoring()
        setState(false, language)
        lastError = detail
        showDetails = true
        terminalRetry = true
    }

    func start(_ profile: Profile, _ language: AppLanguage, interactive: Bool = false) {
        guard !busy, !active, !pending else { return }
        pending = true
        lastError = ""
        showDetails = false
        status = language("Включаем…", "Starting…")
        if interactive {
            startTerminal(profile, language)
        } else {
            monitor = Task {
                do {
                    _ = try await run(profile, "batch")
                    guard !Task.isCancelled else { return }
                    stopMonitoring()
                    setState(true, language)
                } catch {
                    if !Task.isCancelled { fail(error.localizedDescription, language) }
                }
            }
        }
    }

    private func startTerminal(_ profile: Profile, _ language: AppLanguage) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("sshi-start-\(UUID().uuidString)")
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false,
                                                    attributes: [.posixPermissions: 0o700])
        } catch {
            fail(error.localizedDescription, language)
            return
        }
        report = directory
        status = language("Вход в Терминале…", "Signing in via Terminal…")
        monitor = Task {
            do {
                try await launch(profile, directory)
                guard !Task.isCancelled else { return }
                launchTime = ProcessInfo.processInfo.systemUptime
                let deadline = ProcessInfo.processInfo.systemUptime + 180
                repeat {
                    if let (code, detail) = try completion(language) {
                        if code == 0 {
                            stopMonitoring()
                            setState(true, language)
                        } else {
                            fail(detail.isEmpty ? language("SSH завершился с кодом \(code).", "SSH exited with status \(code).") : detail, language)
                        }
                        return
                    }
                    try await Task.sleep(nanoseconds: 1_000_000_000)
                } while !Task.isCancelled && ProcessInfo.processInfo.systemUptime < deadline
                if !Task.isCancelled {
                    fail(language("Истекло время ожидания входа в Терминале.", "Timed out waiting for sign-in in Terminal."), language)
                }
            } catch {
                if !Task.isCancelled { fail(error.localizedDescription, language) }
            }
        }
    }

    func check(_ profile: Profile, _ language: AppLanguage, closing: Bool = false) async {
        guard !busy, !pending else { return }
        busy = true
        if closing { status = language("Останавливаем…", "Stopping…") }
        defer { busy = false }
        do {
            _ = try await run(profile, closing ? "close" : "check")
            if !Task.isCancelled { setState(!closing, language) }
        } catch {
            guard !Task.isCancelled else { return }
            let detail = error.localizedDescription
            if detail.localizedCaseInsensitiveContains("no active ssh control socket") {
                setState(false, language)
            } else {
                setState(active, language)
                lastError = detail
                showDetails = true
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

}
