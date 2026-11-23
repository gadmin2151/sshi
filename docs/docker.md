# Docker Compose

Скопируйте проект на оба компьютера. На **получателе** нужен Linux с локальным Docker Engine и работающим `sshd`. Контейнер `receiver` использует сеть хоста, чтобы обращаться к его `127.0.0.1:22`. На **инициаторе** нужны Docker Compose и команда `ssh`. OpenSSH запускает контейнер `initiator` на время подключения. [Сеть хоста в Docker](https://docs.docker.com/engine/network/drivers/host/)

На **обоих компьютерах** в корне проекта создайте `.env` при первой настройке.

```bash
install -m 600 .env.example .env
```

В `.env` уже указан `https://iroh1.cmon.systems/`. Добавьте строку `IROH_RELAY_ACCESS_TOKEN=...` с токеном этого relay на каждой машине.

### 1. Получите Endpoint ID инициатора

На компьютере, **с которого подключаетесь**, соберите образ и создайте ключ iroh. Команда `keygen` выводит публичный Endpoint ID инициатора. Этот ID понадобится для настройки получателя.

```bash
docker compose build initiator
docker compose run --rm --no-deps -T initiator keygen --key /keys/client.key
```

### 2. Запустите получателя

На компьютере, **к которому подключаетесь**, впишите Endpoint ID инициатора в `IROH_SSH_CLIENT_ID` в `.env`. Для нескольких клиентов перечислите их ID через запятую в одной строке. Если `sshd` слушает другой порт на loopback, измените `IROH_SSH_TARGET`.

```bash
docker compose build receiver
docker compose run --rm --no-deps -T receiver keygen --key /keys/server.key
docker compose up -d receiver
docker compose logs -f receiver
```

Возьмите **Endpoint ID получателя** из строки `Ready` в журнале. Docker хранит оба ключа в именованных томах, поэтому при повторном запуске достаточно `docker compose up -d receiver`.

### 3. Подключитесь с инициатора

Из корня проекта на инициаторе выполните команду, подставив учётную запись на получателе и Endpoint ID из строки `Ready`.

```bash
ssh -o 'ProxyCommand=./iroh-ssh-proxy %h' SSH_USER@RECEIVER_ENDPOINT_ID
```

Для короткой команды из любого каталога сохраните в `~/.ssh/config` адрес получателя и абсолютный путь к `iroh-ssh-proxy` на инициаторе.

```ssh-config
Host iroh-receiver
    HostName RECEIVER_ENDPOINT_ID
    User SSH_USER
    ProxyCommand /absolute/path/to/iroh-ssh/iroh-ssh-proxy %h
    ControlMaster auto
    ControlPath ~/.ssh/iroh-%C
    ControlPersist 60s
```

После этого достаточно `ssh iroh-receiver`. При первом подключении сверьте хост-ключ `sshd` получателя. Для копирования файлов можно использовать `scp ./file.txt iroh-receiver:`.


Образ Compose использует самостоятельную команду `iroh-ssh`. Приглашения, именованные профили и пользовательская служба входят в пакет `sshi`.
