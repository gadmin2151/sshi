## SSHI 0.2.2 · Первое подключение

**Если у вас уже есть ID получателя и код**, откройте новое подключение по приглашению. Вставьте ID удалённого компьютера в поле получателя, код — в поле приглашения. Укажите пользователя SSH на удалённом компьютере и подтвердите вход.

### Добавлено

- На macOS, Windows и Linux приглашение можно принять из пустого списка подключений. В той же форме можно скопировать свой публичный Endpoint ID для владельца сервера.
- Владелец может передать одну ссылку с ID получателя, пользователем SSH и адресом relay. Клиент показывает эти параметры перед подключением. Токен частного relay вводится отдельно.
- На принимающем компьютере с Linux или macOS команда `sshi invite "$CLIENT_ID" --share --user pi` создаёт ссылку для конкретного гостя. На уже доверенном компьютере полную ссылку можно выпустить через сохранённое SSH-подключение.
- При вводе просроченного кода или кода для другого устройства можно исправить данные и попробовать снова. В сохранённом профиле и резервной копии хранятся параметры подключения.
- Обычный интерактивный вход `sshi user@ID` или `sshi connect ИМЯ` предлагает вставить приглашение, когда получатель отклонил Iroh ID. SSHI показывает ID клиента и продолжает SSH-вход после подтверждения.

### Исправлено

- Для первого входа гостю больше не нужно заранее сохранять профиль и искать, куда ввести код. Обычный код, полученный от старой версии SSHI, работает вместе с ID получателя.
- На экране подключения отдельно обозначены ID этого компьютера и ID получателя. После выдачи доступа через Iroh выполняется обычная SSH-аутентификация.

## English · First connection

If the server owner sent you a receiver ID and a code, open a new connection and choose the invitation option. Paste the remote computer’s ID and the invitation into their respective fields, enter your SSH username, and confirm.

- Join from an empty profile library on macOS, Windows or Linux. Copy your computer’s public ID from the same form when you need an invitation.
- Paste a full invitation link to preview the receiver, SSH user and relay together. Older invitation codes work with a separately supplied receiver ID.
- Issue a shareable link on a Linux or macOS receiver with `sshi invite "$CLIENT_ID" --share --user pi`, or create one through an authorized connection. Invitations are tied to the guest’s identity and expire after ten minutes.
- After enrollment the client saves the connection and uses normal SSH authentication. On a private relay, obtain its access token from the administrator.
- On first interactive login, `sshi user@ID` or `sshi connect NAME` prompts for a code or link if the receiver denies that client’s Iroh ID, then continues the SSH login.

[Установка и первый вход / Setup and first connection](https://github.com/gadmin2151/sshi/blob/v0.2.2/README.md)
