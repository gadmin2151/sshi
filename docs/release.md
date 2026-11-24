# SSHI 0.3.1

## SSH-сокет в карточке подключения

- В Linux и macOS статус сокета виден в основном окне выбранного профиля. Нажмите **«Включить»**, затем **«Скопировать команду»**. При входе по ключу сокет запускается в фоне.
- Если SSH требует пароль или подтверждение отпечатка, приложение покажет кнопку **«Войти в терминале»**. Ошибки и подтверждение остановки выводятся в той же карточке.
- CLI поддерживает `sshi socket start --batch PROFILE` для запуска в режиме OpenSSH BatchMode.

## English

- On Linux and macOS, turn on the SSH control socket directly in the selected connection’s card. The app shows its status and offers **Copy command** when it is ready.
- Configured keys can start the socket in the background. If SSH needs interactive sign-in, the card offers a terminal and displays startup errors inline.
- Use `sshi socket start --batch PROFILE` to start without interactive prompts in the CLI.

[Руководство / Setup guide](https://github.com/gadmin2151/sshi/blob/v0.3.1/README.md) · [Передача файлов в 0.3.0 / File transfer in 0.3.0](https://github.com/gadmin2151/sshi/releases/tag/v0.3.0)
