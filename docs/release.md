# SSHI 0.3.0

## Передача файлов

- Выберите сохранённое подключение и нажмите **«Файлы»**. Слева показан ваш компьютер, справа — выбранный получатель. Перетащите файлы или папки в другую панель. Кнопки загрузки и скачивания позволяют выполнить то же действие с клавиатуры.
- Поля пути, переход на уровень выше и обновление доступны с обеих сторон. Во время копирования виден прогресс. SSHI сохраняет существующий файл при совпадении имён.
- CLI поддерживает `sshi --cp` и `sshi cp`, выбор ключа через `-i` и копирование папок через `-r`. Передача использует SFTP через обычный Iroh-маршрут.

## Интерфейс

- В Linux, macOS и Windows основные действия **«Терминал»** и **«Файлы»** теперь рядом. Настройки подключения и вспомогательные SSH-инструменты доступны в выбранном профиле.
- На Linux и macOS при входе по паролю файловое окно предлагает воспользоваться терминалом. На Windows ввод и проверка отпечатка доступны в самом файловом окне.

## English

- Select a connection and open **Files** for a two-pane browser. Drag files or folders between your computer and the remote machine, or use Upload and Download.
- Use `sshi --cp` or `sshi cp` from the terminal. The `-r` flag copies folders. Transfers use SFTP over Iroh and preserve files at conflicting destinations.
- The Linux, macOS and Windows connection managers place the terminal and file browser actions together.

[Руководство / Setup guide](https://github.com/gadmin2151/sshi/blob/v0.3.0/README.md)
