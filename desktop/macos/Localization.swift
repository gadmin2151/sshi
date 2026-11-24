import Foundation

enum AppLanguage: String, CaseIterable {
    case russian = "ru"
    case english = "en"

    static let preferenceKey = "interfaceLanguage"
    static let releaseNotes = URL(string: "https://github.com/gadmin2151/sshi/releases/tag/v0.4.1")!

    static func current(in defaults: UserDefaults = .standard) -> Self {
        defaults.string(forKey: preferenceKey).flatMap(Self.init(rawValue:)) ?? .russian
    }

    func callAsFunction(_ russian: String, _ english: String) -> String {
        self == .english ? english : russian
    }

    var whatsNew: String { self("Что нового в 0.4.1", "What’s new in 0.4.1") }

    func commandError(_ detail: String) -> String {
        let message = detail.lowercased()
        if message.contains("connection name already in use") || message.contains("duplicate connection name") {
            return self("Подключение с таким именем уже существует. Выберите другое имя.",
                        "A connection already uses this name. Choose another name.")
        }
        if message.contains("connection name must be") {
            return self("Укажите имя от 1 до 256 байт без пробелов по краям и управляющих символов.",
                        "Enter a name of 1–256 bytes without surrounding spaces or control characters.")
        }
        if message.contains("invalid ssh username") {
            return self("Проверьте имя пользователя SSH.", "Check the SSH username.")
        }
        if message.contains("invalid iroh endpoint id") || message.contains("invalid value") && message.contains("--endpoint") {
            return self("Проверьте Endpoint ID удалённого компьютера.", "Check the remote computer’s Endpoint ID.")
        }
        if message.contains("an authenticated relay requires https") {
            return self("Для relay с токеном нужен адрес HTTPS.", "A relay with a token needs an HTTPS URL.")
        }
        if message.contains("no relay configured") || message.contains("set a relay url first") {
            return self("Укажите адрес relay в настройках SSHI и повторите попытку.",
                        "Set the relay URL in SSHI settings, then try again.")
        }
        if message.contains("invalid connection relay") || message.contains("invalid relay url") ||
            message.contains("http or https relay url") || message.contains("url may not contain a query or fragment") {
            return self("Укажите адрес relay с http:// или https://, без параметров и фрагмента.",
                        "Enter an http:// or https:// relay URL without a query or fragment.")
        }
        if message.contains("relay token must contain") {
            return self("Введите токен relay без пробелов и переносов строк.",
                        "Enter the relay token without spaces or line breaks.")
        }
        if message.contains("read sshi connection backup json") || message.contains("unsupported sshi backup version") {
            return self("Выберите файл резервной копии подключений, созданный поддерживаемой версией SSHI.",
                        "Choose a connections backup created by a supported version of SSHI.")
        }
        if message.contains("choose a new filename") && message.contains("file exists") {
            return self("Сохраните резервную копию под новым именем.", "Save the backup under a new name.")
        }
        let summary = self("Команда завершилась с ошибкой.", "The command failed.")
        return detail.isEmpty ? summary : summary + "\n\n" + self("Сообщение команды", "Command output") + "\n" + detail
    }
}

func tr(_ russian: String, _ english: String) -> String {
    AppLanguage.current()(russian, english)
}
