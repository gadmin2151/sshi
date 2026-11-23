## Что нового в SSHI 0.2.1

### Добавлено

- В окне проброса портов появилась схема двух компьютеров. Стрелки показывают направление подключения, а подписи меняются по мере ввода адресов и портов. Можно открыть удалённый сервис на своём ПК, предоставить удалённому ПК доступ к локальному сервису или настроить SOCKS-прокси.
- В настройках Linux, Windows и macOS можно выбрать русский или английский язык. Программа запоминает выбор. Пункт «Что нового» открывает страницу текущего выпуска.
- В CLI Linux и macOS появилась команда для запуска проброса по имени профиля. Например, `sshi forward --profile "Мастерская" --kind local --bind-port 8080 --target-host 127.0.0.1 --target-port 80` открывает порт 8080 на вашем компьютере и направляет подключения на порт 80 получателя.

### Исправлено

- Первый щелчок выделяет профиль. Быстрый двойной щелчок открывает один терминал. Чтобы переименовать подключение, щёлкните по выделенному имени ещё раз после системного интервала двойного щелчка.
- Переименование меняет только имя подключения и сохраняет пользователя SSH, Endpoint ID и relay. В CLI доступна команда `sshi profile rename --name "Мастерская" --to "Сервер мастерской"`.

## English

### Added

- The forwarding window shows the two computers, the traffic direction and the addresses to use. Choose access to a remote service, access to a local service from the remote computer, or a local SOCKS proxy.
- Choose English or Russian in Settings on Linux, Windows and macOS. The app remembers your choice. Open **What’s new** to read the notes for your version.
- On Linux and macOS, `sshi forward` starts a tunnel from a saved profile using separate address and port arguments.

### Fixed

- A single click selects a connection. A fast double-click opens one terminal. Click the selected name again after a pause to rename it.
- Renaming a profile preserves its SSH user, Endpoint ID and relay settings. The CLI also supports `sshi profile rename`.

Контрольные суммы пакетов находятся в файле `SHA256SUMS`. [Инструкции по установке](https://github.com/gadmin2151/sshi/blob/v0.2.1/README.md)
