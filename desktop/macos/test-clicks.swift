// Build this executable with SSHI.swift and -DPROFILE_CLICK_TEST on macOS.
import AppKit
import Foundation

@main
struct TestProfileClicks {
    static func testForwarding() throws {
        var draft = ForwardDraft()
        let local = try draft.arguments(profile: "Example PC", language: .english)
        precondition(local == ["forward", "--profile", "Example PC", "--kind", "local", "--bind-host",
                               "127.0.0.1", "--bind-port", "8080", "--target-host", "127.0.0.1", "--target-port", "80"])
        draft.changeKind(to: .dynamic)
        draft.targetHost = "unused.invalid:9"
        draft.targetPort = ""
        let proxy = try draft.arguments(profile: "RPI", language: .russian)
        precondition(proxy == ["forward", "--profile", "RPI", "--kind", "dynamic",
                               "--bind-host", "127.0.0.1", "--bind-port", "1080"])
        draft.bindHost = "::"
        precondition(draft.accessAddress == "[::1]:1080")
        draft.bindPort = "9000"
        draft.changeKind(to: .remote)
        draft.targetHost = "[2001:db8::2]"
        draft.targetPort = "65535"
        let remote = try draft.arguments(profile: "RPI", language: .english)
        precondition(remote == ["forward", "--profile", "RPI", "--kind", "remote", "--bind-host",
                                "::", "--bind-port", "9000", "--target-host", "[2001:db8::2]", "--target-port", "65535"])
        precondition(ForwardDraft.displayAddress("::1", "8080") == "[::1]:8080")
        precondition(ForwardDraft.displayAddress("[::1]", "8080") == "[::1]:8080")
        for badPort in ["0", "65536", "-80", "+80", "", "8:9"] {
            draft.targetPort = badPort
            do {
                _ = try draft.arguments(profile: "RPI", language: .russian)
                preconditionFailure("Invalid forwarding port was accepted")
            } catch {
                precondition(error.localizedDescription.contains("должен быть числом"))
            }
        }
        draft.targetPort = "80"
        for badHost in ["[localhost]", "https://foo", "x:22", "a b", "-host", "a..b", "999.12.10.7"] {
            draft.targetHost = badHost
            precondition((try? draft.arguments(profile: "RPI", language: .english)) == nil)
        }
    }

    static func testLanguage() {
        let suite = "sshi.languages.test." + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        precondition(AppLanguage.current(in: defaults) == .russian)
        defaults.set("en", forKey: AppLanguage.preferenceKey)
        let restored = AppLanguage.current(in: UserDefaults(suiteName: suite)!)
        precondition(restored == .english && restored.whatsNew == "What’s new in 0.5.2")
        precondition(ForwardKind.remote.title(restored) == "Access from there")
        precondition(AppLanguage.russian.commandError("Error: duplicate connection name 'old'").contains("уже существует"))
        precondition(restored.commandError("Error: invalid SSH username").contains("Check the SSH username"))
        defaults.set("unknown", forKey: AppLanguage.preferenceKey)
        precondition(AppLanguage.current(in: defaults) == .russian)
    }

    static func testProfileNotes() throws {
        let legacy = Data(#"{"name":"Lab","user":"pi","endpoint":"test","relay":""}"#.utf8)
        var profile = try JSONDecoder().decode(Profile.self, from: legacy)
        precondition(profile.notes == nil)
        let socketKey = profile.socketKey
        profile.notes = "Проверить журнал\nCheck the log"
        let saved = try JSONEncoder().encode(profile)
        let restored = try JSONDecoder().decode(Profile.self, from: saved)
        precondition(restored.notes == profile.notes && restored.socketKey == socketKey)
    }

    @MainActor
    static func testInvitations() async throws {
        let peer = String(repeating: "ab", count: 32)
        let code = String(repeating: "42", count: 32)
        let relay = "https://relay.example.org/"
        let link = "sshi://invite/v1?endpoint=\(peer)&user=pi&relay=https%3A%2F%2Frelay.example.org%2F#\(code)"
        let info = try JSONSerialization.data(withJSONObject: ["user": "pi", "endpoint": peer, "relay": relay])
        var calls: [([String], String?)] = []
        var others: [Profile] = []
        var deny = false
        var saveFails = false
        let run: InvitationEntry.Runner = { args, input in
            calls.append((args, input))
            switch Array(args.prefix(2)) {
            case ["invitation", "inspect"]: return info
            case ["profile", "list"]: return try JSONEncoder().encode(others)
            case ["invitation", "redeem"] where deny: throw ClientError(message: "Invitation denied")
            case ["profile", "save"] where saveFails: throw ClientError(message: "Could not save")
            default: return Data()
            }
        }
        let raw = InvitationEntry(run: run)
        raw.changeEndpoint(peer)
        precondition(raw.name == String(peer.prefix(12)), "propose a name for a pasted receiver")
        raw.user = "pi"
        raw.relay = relay
        raw.name = "Home"
        raw.changeInput(code)
        precondition(!raw.needsPreview, "legacy code uses the separately entered receiver")
        others = [Profile(name: "HOME", user: "another", endpoint: peer, relay: relay)]
        do {
            _ = try await raw.finish(invited: true, language: .english)
            preconditionFailure("must not overwrite an existing connection")
        } catch { precondition(calls.map { $0.0.first } == ["profile"]) }
        others = []
        raw.token = "private-test-token"
        deny = true
        do {
            _ = try await raw.finish(invited: true, language: .english)
            preconditionFailure("must not save a denied invitation")
        } catch { precondition(error.localizedDescription.contains("declined") && raw.input == code && !raw.granted) }
        precondition(calls.contains { $0.0 == ["config", "set-token", "--relay", relay] && $0.1 == "private-test-token\n" })
        precondition(calls.last?.0 == ["invitation", "redeem", "--endpoint", peer, "--relay", relay] && calls.last?.1 == code + "\n")
        deny = false
        saveFails = true
        do {
            _ = try await raw.finish(invited: true, language: .english)
            preconditionFailure("test save must fail")
        } catch { precondition(raw.granted && raw.input.isEmpty) }
        let previous = calls.count
        raw.name = "Pi"
        saveFails = false
        let name = try await raw.finish(invited: true, language: .english)
        precondition(name == "Pi" && calls.dropFirst(previous).allSatisfy { $0.0.first == "profile" })

        let full = InvitationEntry(run: run)
        full.relay = "https://previous-relay.example.org/"
        full.token = "token-for-previous-relay"
        full.changeInput(link)
        precondition(full.needsPreview)
        try await full.inspect(.english)
        precondition(full.user == "pi" && full.relay == relay && full.endpoint == peer && !full.needsPreview)
        precondition(full.token.isEmpty, "a token must not move to another invitation’s relay")
        precondition(calls.last?.0 == ["invitation", "inspect"] && calls.last?.1 == link + "\n")
        full.name = "Pi from link"
        _ = try await full.finish(invited: true, language: .english)
        precondition(calls.contains { $0.0 == ["invitation", "redeem"] && $0.1 == link + "\n" })
        precondition(calls.last?.0 == ["profile", "save", "--name", "Pi from link", "--user", "pi",
                                       "--endpoint", peer, "--relay", relay])
        precondition(!calls.flatMap { $0.0 }.contains { $0.contains(code) || $0.contains("private-test-token") })
        precondition(!calls.contains { $0.1?.contains("token-for-previous-relay") == true })
        print("Invitation preview, paste, profile conflicts, denial and retry after grant passed")
    }

    @MainActor
    static func testSocketLifecycle(_ profile: Profile) async throws {
        var calls: [String] = []
        var launches = 0
        var active = false
        var refused = false
        var directory: URL?
        let manager = FileManager.default
        let helper = Client.binary.deletingLastPathComponent().appendingPathComponent("sshi-socket-start")
        let session = SocketSession(run: { selected, op in
            precondition(selected == profile)
            calls.append(op)
            switch op {
            case "batch":
                if refused { throw ClientError(message: "Permission denied (publickey)") }
                active = true
                return "/tmp/test.sock"
            case "path": return "/tmp/test.sock"
            case "command": return "ssh -S /tmp/test.sock -o ProxyCommand=false -- example"
            case "close": active = false; return ""
            default:
                if !active { throw ClientError(message: "No active SSH control socket") }
                return "Master running"
            }
        }, launch: { selected, dir in
            launches += 1
            directory = dir
            if launches == 1 {
                let testCLI = dir.appendingPathComponent("fixture-cli")
                let script = """
                #!/bin/sh
                test "$1" = socket && test "$2" = start && test "$3" = -- && test -t 2 || exit 8
                echo 'fixture stdout is private'
                echo 'fixture public key was denied' >&2
                exit 42
                """
                try script.write(to: testCLI, atomically: true, encoding: .utf8)
                try manager.setAttributes([.posixPermissions: 0o700], ofItemAtPath: testCLI.path)
                let process = Process()
                process.executableURL = helper
                process.arguments = [dir.path, testCLI.path, selected.name]
                process.standardOutput = FileHandle.nullDevice
                process.standardError = FileHandle.nullDevice
                try process.run()
                process.waitUntilExit()
                precondition(process.terminationStatus == 42)
            } else {
                active = true
                try "0\n".write(to: dir.appendingPathComponent("result"), atomically: true, encoding: .utf8)
            }
        })
        await session.check(profile, .english)
        precondition(calls == ["check"] && launches == 0 && session.status == "Off")
        session.start(profile, .english)
        await session.monitor?.value
        precondition(session.active && session.status == "On" && launches == 0)
        let command = try await session.value(profile, "command")
        precondition(command.contains("/tmp/test.sock"))
        await session.check(profile, .english, closing: true)
        precondition(!session.active && session.status == "Off")

        refused = true
        session.start(profile, .english)
        await session.monitor?.value
        precondition(session.terminalRetry && launches == 0)
        session.start(profile, .english, interactive: true)
        await session.monitor?.value
        precondition(session.showDetails && session.lastError == "fixture public key was denied")
        precondition(!session.pending && !manager.fileExists(atPath: directory!.path))
        session.start(profile, .english, interactive: true)
        await session.monitor?.value
        precondition(session.active && session.status == "On" && launches == 2)
        active = false
        await session.check(profile, .english)
        precondition(!session.active && session.status == "Off")
        session.dispose()
        print("Inline socket state detected quiet startup, terminal fallback, close and expiry")
    }

    @MainActor
    static func testSocketAndUpdates() async throws {
        let profiles = try JSONDecoder().decode([Profile].self, from: Client.run(["profile", "list", "--json"]))
        let selected = profiles.first { $0.name == "Second" }!
        try await testSocketLifecycle(selected)
        let path = String(decoding: try Client.run(selected.socket("path")), as: UTF8.self).trimmingCharacters(in: .newlines)
        let command = String(decoding: try Client.run(selected.socket("command")), as: UTF8.self)
        precondition(path.hasPrefix("/tmp/") && path.hasSuffix(".sock"))
        precondition(command.contains(path) && command.contains("ProxyCommand=false") && command.contains(selected.destination))
        precondition(!FileManager.default.fileExists(atPath: path), "reading socket information must be offline")
        for op in ["start", "check", "close"] {
            precondition(selected.socket(op) == ["socket", op, "--", "Second"])
        }
        precondition(selected.socket("start", batch: true) == ["socket", "start", "--batch", "--", "Second"])
        let workspace = Workspace()
        let release = """
        {"current":"1.0.0","latest":"1.0.1","available":true,"release_url":"https://github.com/gadmin2151/sshi/releases/tag/v1.0.1","download_url":null}
        """
        await workspace.checkUpdates {
            try JSONDecoder().decode(ReleaseUpdate.self, from: Data(release.utf8))
        }
        precondition(workspace.update?.available == true && workspace.update?.latest == "1.0.1")
        precondition(workspace.update?.download_url == nil && !workspace.checkingUpdate)
        await workspace.checkUpdates { throw ClientError(message: "offline") }
        precondition(workspace.updateError == "offline" && workspace.update?.available == true)
        await workspace.checkUpdates {
            let current = release.replacingOccurrences(of: "\"available\":true", with: "\"available\":false")
                                 .replacingOccurrences(of: "1.0.1", with: "1.0.0")
            return try JSONDecoder().decode(ReleaseUpdate.self, from: Data(current.utf8))
        }
        precondition(workspace.update?.available == false && workspace.updateError == nil)
        print("Offline socket preview, profile arguments and update notification states passed")
    }

    @MainActor
    static func main() async throws {
        setvbuf(stdout, nil, _IOLBF, 0)
        try await testSocketAndUpdates()
        try await testInvitations()
        try runWindowTests()
    }

    @MainActor
    static func runWindowTests() throws {
        try testForwarding()
        testLanguage()
        try testProfileNotes()
        print("Forwarding arguments, address preview, validation and persistent language passed")
        let application = NSApplication.shared
        application.setActivationPolicy(.regular)
        application.finishLaunching()
        print("AppKit initialized")
        let window = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 650, height: 220),
                              styleMask: [.titled], backing: .buffered, defer: false)
        let workspace = Workspace()
        let profiles = try JSONDecoder().decode([Profile].self, from: Client.run(["profile", "list", "--json"]))
        precondition(profiles.count == 2, "the test needs its own two-profile fixture")
        let names = profiles.map(\.name)
        var opened: [String] = []
        let grid = ProfileGrid(workspace) { opened.append($0) }
        workspace.profiles = profiles
        workspace.selected = names[0]
        let scroll = NSScrollView(frame: window.contentView!.bounds)
        scroll.autoresizingMask = [.width, .height]
        grid.frame = scroll.contentView.bounds
        grid.autoresizingMask = [.width]
        scroll.documentView = grid
        window.contentView!.addSubview(scroll)
        grid.update(profiles, selected: names[0], language: .english)
        print("Connection grid populated")
        window.makeKeyAndOrderFront(nil)
        application.activate(ignoringOtherApps: true)
        window.makeFirstResponder(grid)

        func drain(_ duration: TimeInterval) {
            let end = Date(timeIntervalSinceNow: duration)
            while Date() < end {
                if let event = application.nextEvent(matching: .any, until: end, inMode: .default, dequeue: true) {
                    application.sendEvent(event)
                }
            }
        }
        var number = 0
        func cardTarget(at index: Int) -> (NSView, NSPoint) {
            let path = IndexPath(item: index, section: 0)
            guard let card = grid.item(at: path) else {
                fatalError("card \(index) must be onscreen, active items \(grid.indexPathsForVisibleItems()), grid \(grid.frame)")
            }
            let bounds = card.view.bounds
            let position = card.view.convert(NSPoint(x: bounds.midX, y: bounds.midY), to: nil)
            precondition(grid.cardPath(atWindowPoint: position) == path,
                         "card \(index) must be hit at its center, bounds \(bounds), visible grid \(grid.visibleRect)")
            guard let target = grid.hitTest(grid.superview!.convert(position, from: nil)), target === grid else {
                fatalError("a card click must reach the connection grid")
            }
            return (target, position)
        }

        func click(_ index: Int, count: Int = 1) {
            let (target, position) = cardTarget(at: index)
            // Deliver the pair to the hit-tested view without relying on WindowServer tracking
            // state for events generated by the test.
            number += 1
            for kind: NSEvent.EventType in [.leftMouseDown, .leftMouseUp] {
                let event = NSEvent.mouseEvent(with: kind, location: position, modifierFlags: [],
                                               timestamp: ProcessInfo.processInfo.systemUptime,
                                               windowNumber: window.windowNumber, context: nil,
                                               eventNumber: number, clickCount: count,
                                               pressure: kind == .leftMouseDown ? 1 : 0)!
                if kind == .leftMouseDown {
                    target.mouseDown(with: event)
                } else {
                    target.mouseUp(with: event)
                }
            }
            drain(0.025)
        }

        let settle = NSEvent.doubleClickInterval + 0.12
        drain(0.2)
        window.contentView?.layoutSubtreeIfNeeded()
        let (contextTarget, position) = cardTarget(at: 1)
        let rightClick = NSEvent.mouseEvent(with: .rightMouseDown, location: position, modifierFlags: [],
                                           timestamp: ProcessInfo.processInfo.systemUptime,
                                           windowNumber: window.windowNumber, context: nil,
                                           eventNumber: 0, clickCount: 1, pressure: 1)!
        guard let menu = contextTarget.menu(for: rightClick) else { fatalError("right-click must show profile actions") }
        precondition(grid.selectedIndex == 1 && workspace.selected == names[1] && opened.isEmpty)
        let notes = menu.indexOfItem(withTitle: "Notes…")
        precondition(notes >= 0)
        menu.performActionForItem(at: notes)
        guard case .notes(let selected)? = workspace.sheet?.content, selected == profiles[1] else {
            fatalError("the context menu must open notes for the right-clicked profile")
        }
        workspace.sheet = nil
        workspace.selected = names[0]
        grid.update(profiles, selected: names[0], language: .english)

        click(0)
        drain(settle)
        precondition(opened.isEmpty && window.firstResponder === grid, "first click on initial selection")
        click(0)
        click(0, count: 2)
        drain(settle)
        precondition(opened == [names[0]] && window.firstResponder === grid,
                     "double-click on an armed card opens once and cancels rename, opened \(opened), grid focused \(window.firstResponder === grid)")
        click(0)
        click(1, count: 2)
        drain(settle)
        precondition(opened == [names[0]], "two adjacent cards cannot open one another")
        click(0)
        click(1)
        precondition(grid.selectedIndex == 1 && workspace.selected == names[1], "other card selects immediately")
        drain(settle)
        click(1)
        drain(settle)
        guard let editor = window.firstResponder as? NSTextView else { fatalError("slow click must edit inline") }
        editor.string = "Discarded"
        editor.doCommand(by: #selector(NSResponder.cancelOperation(_:)))
        drain(0.1)
        precondition(workspace.selected == names[1] && window.firstResponder === grid, "Escape cancels rename")
        click(1)
        drain(settle)
        click(1)
        drain(settle)
        guard let savedEditor = window.firstResponder as? NSTextView else { fatalError("could not edit again") }
        savedEditor.string = "Renamed"
        savedEditor.insertNewline(nil)
        drain(0.15)
        let result = try JSONDecoder().decode([Profile].self, from: Client.run(["profile", "list", "--json"]))
        guard let renamed = result.first(where: { $0.name == "Renamed" }) else { fatalError("rename not saved") }
        precondition(renamed.endpoint == profiles[1].endpoint && renamed.user == profiles[1].user
                     && renamed.relay == profiles[1].relay && opened == [names[0]], "rename changed connection data")
        print("AppKit selection, context menu, double-click and inline rename passed")
    }
}
