import Darwin
import SwiftUI

enum ForwardKind: String, CaseIterable {
    case local, remote, dynamic

    var defaultPort: String { self == .dynamic ? "1080" : "8080" }

    func title(_ language: AppLanguage) -> String {
        switch self {
        case .local: return language("Открыть здесь", "Access from here")
        case .remote: return language("Открыть там", "Access from there")
        case .dynamic: return "SOCKS5"
        }
    }
}

struct ForwardDraft {
    var kind: ForwardKind = .local
    var bindHost = "127.0.0.1"
    var bindPort = "8080"
    var targetHost = "127.0.0.1"
    var targetPort = "80"

    mutating func changeKind(to newKind: ForwardKind) {
        if bindPort == kind.defaultPort { bindPort = newKind.defaultPort }
        kind = newKind
    }

    static func displayAddress(_ host: String, _ port: String) -> String {
        let host = host.trimmingCharacters(in: .whitespacesAndNewlines)
        let wrapped = host.contains(":") && !host.hasPrefix("[") ? "[\(host)]" : host
        return "\(wrapped.isEmpty ? "…" : wrapped):\(port.isEmpty ? "…" : port)"
    }

    var accessAddress: String {
        switch bindHost.trimmingCharacters(in: .whitespacesAndNewlines) {
        case "0.0.0.0": return Self.displayAddress("127.0.0.1", bindPort)
        case "::", "[::]": return Self.displayAddress("::1", bindPort)
        default: return Self.displayAddress(bindHost, bindPort)
        }
    }

    private static func validHost(_ host: String) -> Bool {
        let raw = host.hasPrefix("[") && host.hasSuffix("]") ? String(host.dropFirst().dropLast()) : host
        var v6 = in6_addr()
        if !raw.utf8.contains(0), inet_pton(AF_INET6, raw, &v6) == 1 { return true }
        let dns = host.hasSuffix(".") ? String(host.dropLast()) : host
        let labels = dns.split(separator: ".", omittingEmptySubsequences: false)
        if labels.count == 4 && labels.allSatisfy({ !$0.isEmpty && $0.utf8.allSatisfy { (48...57).contains($0) } }) {
            var v4 = in_addr()
            return inet_pton(AF_INET, dns, &v4) == 1
        }
        return !dns.isEmpty && dns.utf8.count <= 253 &&
            labels.allSatisfy { label in
                !label.isEmpty && label.utf8.count <= 63 && label.first != "-" && label.last != "-" &&
                    label.utf8.allSatisfy { (65...90).contains($0) || (97...122).contains($0) ||
                        (48...57).contains($0) || $0 == 45 }
            }
    }

    func arguments(profile: String, language: AppLanguage) throws -> [String] {
        func endpoint(_ host: String, _ port: String, _ description: String) throws -> (String, String) {
            let host = host.trimmingCharacters(in: .whitespacesAndNewlines)
            let port = port.trimmingCharacters(in: .whitespacesAndNewlines)
            guard Self.validHost(host) else {
                throw ClientError(message: language("Введите IP-адрес или имя узла для поля «\(description)».",
                                                    "Enter an IP address or hostname for “\(description)”."))
            }
            guard port.utf8.allSatisfy({ (48...57).contains($0) }), let number = UInt16(port), number > 0 else {
                throw ClientError(message: language("Порт для поля «\(description)» должен быть числом от 1 до 65535.",
                                                    "The port for “\(description)” must be a number from 1 to 65535."))
            }
            return (host, String(number))
        }
        let bind = try endpoint(bindHost, bindPort, language("Слушать", "Listen"))
        var args = ["forward", "--profile", profile, "--kind", kind.rawValue,
                    "--bind-host", bind.0, "--bind-port", bind.1]
        if kind != .dynamic {
            let target = try endpoint(targetHost, targetPort, language("Сервис", "Service"))
            args += ["--target-host", target.0, "--target-port", target.1]
        }
        return args
    }
}

private struct TunnelArrow: Shape {
    let left: Bool

    func path(in rect: CGRect) -> Path {
        let start = left ? rect.maxX : rect.minX
        let end = left ? rect.minX : rect.maxX
        let wing = end + (left ? 6 : -6)
        var path = Path()
        path.move(to: CGPoint(x: start, y: rect.midY))
        path.addLine(to: CGPoint(x: end, y: rect.midY))
        path.move(to: CGPoint(x: wing, y: rect.midY - 5))
        path.addLine(to: CGPoint(x: end, y: rect.midY))
        path.addLine(to: CGPoint(x: wing, y: rect.midY + 5))
        return path
    }
}

struct ForwardForm: View {
    @Binding var draft: ForwardDraft
    let profile: String
    let language: AppLanguage

    private var here: String { language("Этот Mac", "This Mac") }
    private var listener: String { language("Слушает", "Listening") }
    private var service: String { language("Сервис", "Service") }
    private var remote: Bool { draft.kind == .remote }

    private func node(_ icon: String, _ title: String, _ role: String, _ address: String) -> some View {
        VStack(spacing: 5) {
            Image(systemName: icon).font(.title2).foregroundColor(.accentColor)
            Text(title).font(.headline).lineLimit(1).truncationMode(.middle)
            Text(role).font(.caption).foregroundColor(.secondary)
            Text(address).font(.system(.caption, design: .monospaced)).lineLimit(2).truncationMode(.middle)
        }
        .frame(maxWidth: .infinity).padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.primary.opacity(0.045)))
        .accessibilityElement(children: .combine)
    }

    private func fields(_ label: String, _ host: Binding<String>, _ port: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(label).font(.headline)
            HStack {
                TextField(language("Адрес или имя узла", "Address or hostname"), text: host)
                    .accessibilityLabel(label + " · " + language("Адрес", "Address"))
                TextField(language("Порт", "Port"), text: port).frame(width: 85)
                    .accessibilityLabel(label + " · " + language("Порт", "Port"))
            }.textFieldStyle(.roundedBorder)
        }
    }

    private var instruction: String {
        let address = draft.accessAddress
        switch draft.kind {
        case .local: return language("На этом Mac откройте \(address).", "On this Mac, connect to \(address).")
        case .remote: return language("На \(profile) откройте \(address).", "On \(profile), connect to \(address).")
        case .dynamic: return language("В приложении на этом Mac укажите SOCKS5-прокси \(address).",
                                       "Set the SOCKS5 proxy in your app on this Mac to \(address).")
        }
    }

    var body: some View {
        let bind = ForwardDraft.displayAddress(draft.bindHost, draft.bindPort)
        let target = ForwardDraft.displayAddress(draft.targetHost, draft.targetPort)
        VStack(alignment: .leading, spacing: 14) {
            Picker(language("Тип проброса", "Forwarding mode"),
                   selection: Binding(get: { draft.kind }, set: { draft.changeKind(to: $0) })) {
                ForEach(ForwardKind.allCases, id: \.self) { kind in Text(kind.title(language)).tag(kind) }
            }.pickerStyle(.segmented).labelsHidden()
            HStack(spacing: 8) {
                node("laptopcomputer", here, remote ? service : listener, remote ? target : bind)
                VStack(spacing: 4) {
                    Text("SSH / Iroh").font(.caption).foregroundColor(.secondary)
                    TunnelArrow(left: remote).stroke(Color.accentColor, lineWidth: 2).frame(width: 58, height: 14)
                }.accessibilityHidden(true)
                node("server.rack", profile, remote ? listener : (draft.kind == .dynamic ? language("Выход в сеть", "Network exit") : service),
                     remote ? bind : (draft.kind == .dynamic ? language("Адрес из приложения", "App destination") : target))
            }
            fields(language("Слушать на", "Listen on") + " · " + (remote ? profile : here),
                   $draft.bindHost, $draft.bindPort)
            if draft.kind != .dynamic {
                fields(language("Адрес сервиса со стороны", "Service as reached from") + " · " + (remote ? here : profile),
                       $draft.targetHost, $draft.targetPort)
            }
            Divider()
            Text(instruction).textSelection(.enabled)
            Text(language("Чтобы остановить туннель, закройте его окно терминала.",
                          "Close the tunnel’s terminal window to stop forwarding."))
                .font(.caption).foregroundColor(.secondary)
        }
    }
}
