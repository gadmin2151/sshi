Name:           sshi
Version:        %{sshi_version}
Release:        1
Summary:        SSH connections over an Iroh relay
License:        MIT
URL:            https://github.com/gadmin2151/sshi
Source0:        payload.tar
Requires:       openssh-clients, ca-certificates
Recommends:     python3-gobject, gtk3, gobject-introspection
Suggests:       openssh-server
%global debug_package %{nil}
# The incoming executable is already stripped for the package's target CPU.
%global __os_install_post %{nil}

%description
SSHI provides a native GTK connection manager and command-line utilities
for reaching an OpenSSH server through an Iroh relay.

%prep
%build
%install
mkdir -p "%{buildroot}"
tar xf "%{SOURCE0}" -C "%{buildroot}"

%files
%dir /etc/sshi
%config(noreplace) /etc/sshi/config.toml
/usr/bin/sshi*
/usr/lib/systemd/user/sshi.service
/usr/share/applications/sshi.desktop
/usr/share/pixmaps/sshi.png
/usr/share/man/man1/sshi*.1.gz
%dir /usr/share/doc/sshi
%license /usr/share/doc/sshi/copyright
