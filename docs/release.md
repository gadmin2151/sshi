# SSHI 0.2.4

Исправлено преждевременное закрытие фонового SSH-сокета на Linux и macOS. Теперь после запуска `sshi socket start` можно закрыть терминал входа и продолжить работать с сокетом из другого окна. Сокет закрывается командой `sshi socket close` или после заданного срока бездействия.

## SSH-сокеты в Linux

В окне **SSH-сокет…** клиент GTK автоматически проверяет состояние соединения. После входа окно показывает, когда мастер OpenSSH готов и скопированную команду можно использовать в другом терминале. Если запуск завершается ошибкой, её сообщение появляется в этом же окне.

## English

Background SSH control sockets on Linux and macOS now survive closing the sign-in terminal. Reuse the socket from another window until its idle timeout, or close it with `sshi socket close`.

The GTK **SSH control socket…** dialog checks the connection status automatically. After sign-in, it shows when the OpenSSH master is ready and the copied command can be used in another terminal. If startup fails, the dialog displays the error.

[Руководство и установка / Setup guide](https://github.com/gadmin2151/sshi/blob/v0.2.4/README.md) · [Изменения в 0.2.3 / Previous release](https://github.com/gadmin2151/sshi/releases/tag/v0.2.3) · [Все выпуски / All releases](https://github.com/gadmin2151/sshi/releases)
