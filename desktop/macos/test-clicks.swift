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
        precondition(restored == .english && restored.whatsNew == "What’s new in 0.2.1")
        precondition(ForwardKind.remote.title(restored) == "Access from there")
        precondition(AppLanguage.russian.commandError("Error: duplicate connection name 'old'").contains("уже существует"))
        precondition(restored.commandError("Error: invalid SSH username").contains("Check the SSH username"))
        defaults.set("unknown", forKey: AppLanguage.preferenceKey)
        precondition(AppLanguage.current(in: defaults) == .russian)
    }

    @MainActor
    static func main() throws {
        try testForwarding()
        testLanguage()
        print("Forwarding arguments, address preview, validation and persistent language passed")
        let application = NSApplication.shared
        application.setActivationPolicy(.regular)
        application.finishLaunching()
        let window = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 320, height: 220),
                              styleMask: [.titled], backing: .buffered, defer: false)
        let workspace = Workspace()
        let profiles = try JSONDecoder().decode([Profile].self, from: Client.run(["profile", "list", "--json"]))
        precondition(profiles.count == 2, "the test needs its own two-profile fixture")
        let names = profiles.map(\.name)
        var opened: [String] = []
        let table = ProfileTable(workspace) { opened.append($0) }
        workspace.profiles = profiles
        workspace.selected = names[0]
        let scroll = NSScrollView(frame: window.contentView!.bounds)
        scroll.autoresizingMask = [.width, .height]
        table.frame = scroll.contentView.bounds
        table.autoresizingMask = [.width]
        scroll.documentView = table
        window.contentView!.addSubview(scroll)
        table.update(profiles, selected: names[0])
        window.makeKeyAndOrderFront(nil)
        application.activate(ignoringOtherApps: true)
        window.makeFirstResponder(table)

        func drain(_ duration: TimeInterval) {
            let end = Date(timeIntervalSinceNow: duration)
            while Date() < end {
                if let event = application.nextEvent(matching: .any, until: end, inMode: .default, dequeue: true) {
                    application.sendEvent(event)
                }
            }
        }
        var number = 0
        func click(_ row: Int, count: Int = 1) {
            let rect = table.rect(ofRow: row)
            let position = table.convert(NSPoint(x: rect.midX, y: rect.midY), to: nil)
            for kind: NSEvent.EventType in [.leftMouseDown, .leftMouseUp] {
                number += 1
                let event = NSEvent.mouseEvent(with: kind, location: position, modifierFlags: [],
                                               timestamp: ProcessInfo.processInfo.systemUptime,
                                               windowNumber: window.windowNumber, context: nil,
                                               eventNumber: number, clickCount: count, pressure: 1)!
                application.postEvent(event, atStart: false)
            }
            drain(0.025)
        }

        let settle = NSEvent.doubleClickInterval + 0.12
        drain(0.2)
        click(0)
        drain(settle)
        precondition(opened.isEmpty && window.firstResponder === table, "first click on initial selection")
        click(0)
        click(0, count: 2)
        drain(settle)
        precondition(opened == [names[0]] && window.firstResponder === table,
                     "double-click on an armed row opens once and cancels rename")
        click(0)
        click(1, count: 2)
        drain(settle)
        precondition(opened == [names[0]], "two adjacent rows cannot open one another")
        click(0)
        click(1)
        precondition(table.selectedRow == 1 && workspace.selected == names[1], "other row selects immediately")
        drain(settle)
        click(1)
        drain(settle)
        guard let editor = window.firstResponder as? NSTextView else { fatalError("slow click must edit inline") }
        editor.string = "Discarded"
        editor.doCommand(by: #selector(NSResponder.cancelOperation(_:)))
        drain(0.1)
        precondition(workspace.selected == names[1] && window.firstResponder === table, "Escape cancels rename")
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
        print("AppKit selection, fast double-click, pending rename cancellation and slow-click rename passed")
    }
}
