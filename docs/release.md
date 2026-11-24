# SSHI 0.5.1

## Системная служба в Linux

- Пакеты Debian, RPM и Arch включают `sshid.service`. Служба использует Endpoint ID и настройки relay пользователя `root`.
- Включите службу командой `sudo systemctl enable --now sshid`. Для перезапуска используйте `sudo systemctl restart sshid`, для просмотра журнала — `sudo journalctl -u sshid`.
- Если `root` уже запустил получатель как пользовательскую службу `sshi`, остановите её перед запуском `sshid`.

## System service on Linux

- Debian, RPM, and Arch packages include `sshid.service`. It uses root’s Endpoint ID and relay settings.
- Enable and start the service with `sudo systemctl enable --now sshid`. Restart it with `sudo systemctl restart sshid` and view its logs with `sudo journalctl -u sshid`.
- If root is already running the receiver as the `sshi` user service, stop that service before starting `sshid`.

[Руководство / Setup guide](https://github.com/gadmin2151/sshi/blob/v0.5.1/README.md)
