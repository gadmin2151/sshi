# SSHI 0.2.3

### Обновления с GitHub

- На Windows, macOS и Linux SSHI проверяет стабильные выпуски при открытии и пока работает. При появлении новой версии в главном окне появляется кнопка с её номером.
- Нажмите кнопку, чтобы скачать пакет для своего компьютера. На Windows после загрузки закройте старый EXE и запустите новый. На macOS и Linux откройте пакет в установщике. Проверку можно запустить вручную в настройках или командой `sshi update`.
- Для Arch Linux и CachyOS x86-64 в релиз добавлен пакет для pacman. На других Linux-системах клиент подбирает DEB или RPM с учётом архитектуры. Если подходящего пакета нет, он предлагает страницу выпуска.

### Локальный SSH-сокет

- В графическом клиенте Linux и macOS выберите профиль и откройте **SSH-сокет… → Запустить**. После входа можно скопировать команду для другого терминала или Codex. Она использует тот же SSH-вход через Iroh.
- В CLI доступны `sshi socket start`, `path`, `command`, `check` и `close` с именем профиля. Повторный запуск использует уже открытое соединение. Команда `close` закрывает также сеансы на этом сокете.
- По умолчанию мастер ждёт 700 минут после последнего сеанса. Чтобы задать другой срок, запустите новый сокет, например командой `sshi socket start --persist 30m "Мастерская"`. Для собственного пути OpenSSH поддерживаются флаги `-M`, `-N`, `-f`, `-S`, `-O` и `-o ControlPersist=…`.

### English

- Windows, macOS and Linux check GitHub for stable updates in the background. A versioned button opens the download for your system. You can also check from Settings or run `sshi update --json`. Arch and CachyOS x86-64 now have a pacman package.
- On Linux and macOS, select a saved connection and open **SSH control socket… → Start socket**. Sign in once, then copy the SSH command for another terminal or Codex. Use `sshi socket check PROFILE` for status and `sshi socket close PROFILE` to disconnect.
- Managed sockets default to a 700-minute idle timeout. The copied SSH command requires a running socket and fails if it has been closed.

[Руководство и установка / Setup guide](https://github.com/gadmin2151/sshi/blob/v0.2.3/README.md)
