# Сборка SSHI

Версия пакетов берётся из `Cargo.toml`. Для выпуска 0.5.2 используется Rust 1.96 и зафиксированный `Cargo.lock`.

## Linux

Локально нужны Rust, компилятор C, OpenSSH и инструменты выбранного формата пакетов. Для GTK-клиента установите Python 3, PyGObject, GTK 3 и терминал.

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release --bin sshi
sh packaging/build-deb.sh
```

Docker Buildx собирает три DEB с glibc из Debian 11. Для RPM используются те же исполняемые файлы. На машине сборки RPM нужны `dpkg-deb` и `rpmbuild`.

```sh
docker buildx build --platform linux/amd64 -f packaging/Dockerfile --output type=local,dest=dist .
sh packaging/build-rpm.sh dist/sshi_0.5.2_amd64.deb
sh packaging/build-rpm.sh dist/sshi_0.5.2_arm64.deb
```

На Arch Linux или CachyOS установите `base-devel` и соберите пакет для pacman из готового amd64 DEB под обычным пользователем.

```sh
bash packaging/build-arch.sh dist/sshi_0.5.2_amd64.deb dist
```

## Windows x64

Используйте Rust MSVC, Visual Studio Build Tools и Windows SDK. Выполните в PowerShell:

```powershell
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo test --locked --features windows-client --lib --bin sshi-windows
cargo build --locked --release --features windows-client --bin sshi-windows
```

Портативный файл находится в `target/release/sshi-windows.exe`.

## macOS Apple Silicon

На Mac с Xcode Command Line Tools и Rust выполните:

```sh
cargo test --locked
bash packaging/build-mac.sh
```

Скрипт компилирует SwiftUI-приложение и Rust CLI, создаёт пакет в `dist/` и проверяет локальную подпись приложения. Для установки используется штатный Installer. В выпуске используется ad hoc подпись приложения. Сертификат Developer ID и нотариальное заверение Apple в этой сборке отсутствуют.

## Выпуск

Workflow в `.github/workflows/release.yml` собирает и проверяет пакеты на Linux, Windows и macOS. Публикация запускается для тега версии или коммита в `main`, заголовок которого начинается с `Release SSHI `. По завершении всех платформ GitHub Releases получает пакеты и `SHA256SUMS`. Проверка источников и пакетов выполняется командой `python3 packaging/check-release.py`. В `--dist` передайте каталог только с артефактами этого выпуска.

Для сквозного теста установите локальный relay и укажите готовый amd64 DEB:

```sh
cargo install --locked --version 1.5.0 --debug --features server iroh-relay
tests/run-relay-smoke.sh dist/sshi_0.5.2_amd64.deb
```

Тест запускает отдельные SSH-сервер и relay, создаёт временные идентичности, активирует и отзывает приглашение, проверяет интерактивный вход, код возврата удалённой команды, параллельные сеансы и передачу данных через `-L`, `-R` и `-D`. Все три режима также проверяются через команду `sshi forward`, которую запускает графический клиент.

После установки пакета проверьте выбор карточек, форму проброса и переключение языка. Для GTK используйте следующие команды.

```sh
xvfb-run -a /usr/bin/python3 tests/profile_clicks.py /usr/bin/sshi
xvfb-run -a /usr/bin/python3 tests/gtk_forward.py /usr/bin/sshi
xvfb-run -a /usr/bin/python3 tests/gtk_invitation.py /usr/bin/sshi
```

На Apple Silicon выполните `bash desktop/macos/test-clicks.sh` в графическом сеансе.
