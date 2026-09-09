# Equinox RPM spec (builds on Ubuntu-with-rpm AND Fedora/RHEL).
#
# Build on Fedora:
#   sudo dnf install cargo gcc gettext-devel \
#       gtk4-devel libadwaita-devel libsoup3-devel gdk-pixbuf2-devel
#   rpmbuild -bb ~/rpmbuild/SPECS/equinox.spec   (after ./release.sh staged it)
# Build on Debian/Ubuntu (non-root):
#   sudo apt install rpm
#   ./release.sh     # passes --nodeps + a user-local _dbpath automatically
#
# Ubuntu's rpm/debuginfo handling trips on an empty debugfiles.list, so skip
# the auto debuginfo subpackage everywhere (the app ships release binaries).
%define debug_package %{nil}

Name:           equinox
# Single source of truth is Cargo.toml; ./release.sh derives both macros from
# it and passes them with --define. rpm forbids '-' in Version, so Cargo's
# "0.1.1-a1" becomes the rpm package version "0.1.1a1"; the SOURCE tarball
# keeps the Cargo form ("equinox-0.1.1-a1.tar.gz"). Fallbacks keep a raw
# `rpmbuild -bb` working without release.sh.
%{!?_equinox_version:%global _equinox_version 0.1.3a7}
%{!?_equinox_source:%global _equinox_source 0.1.3-a7}
Version:        %{_equinox_version}
Release:        1%{?dist}
Summary:        Multi-source daily wallpaper manager
License:        GPL-3.0-or-later
Source0:        %{name}-%{_equinox_source}.tar.gz

BuildRequires:  cargo, gcc
BuildRequires:  gtk4-devel, libadwaita-devel, libsoup3-devel, gdk-pixbuf2-devel
BuildRequires:  gettext, gettext-devel
Requires:       gtk4, libadwaita, libsoup3, gdk-pixbuf2

%description
GTK4/libadwaita wallpaper manager where every source (Bing daily wallpaper,
Windows Spotlight, NASA APOD, Wikimedia POTD) is managed independently: its
own schedule, its own gallery, its own wallpaper rule. A session-resident
supervisor keeps the background running without systemd, and wallpapers are
set through pluggable GNOME/KDE/XFCE backends.

%prep
%setup -q -n %{name}-%{_equinox_source}

# Either a one-shot compile (Fedora manual path) or, when release.sh passes
# --define "_host_target $PWD/target", reuse the host-built release binaries
# so packaging takes seconds instead of a full rebuild.
%build
%if %{defined _host_target}
echo "using prebuilt release binaries from %{_host_target}"
%else
cargo build --release --locked
%endif

%install
%if %{defined _host_target}
install -Dm755 %{_host_target}/release/equinox-gui        %{buildroot}%{_bindir}/equinox-gui
install -Dm755 %{_host_target}/release/equinox-supervisor %{buildroot}%{_bindir}/equinox-supervisor
install -Dm755 %{_host_target}/release/equinox-daemon     %{buildroot}%{_bindir}/equinox-daemon
%else
install -Dm755 target/release/equinox-gui        %{buildroot}%{_bindir}/equinox-gui
install -Dm755 target/release/equinox-supervisor %{buildroot}%{_bindir}/equinox-supervisor
install -Dm755 target/release/equinox-daemon     %{buildroot}%{_bindir}/equinox-daemon
%endif
install -Dm644 data/github.nwkyz.Equinox.desktop \
    %{buildroot}%{_datadir}/applications/github.nwkyz.Equinox.desktop
sed -i "s|^Exec=equinox-gui$|Exec=%{_bindir}/equinox-gui|" \
    %{buildroot}%{_datadir}/applications/github.nwkyz.Equinox.desktop
install -Dm644 data/equinox.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/equinox.svg
install -Dm644 packaging/flatpak/github.nwkyz.Equinox.metainfo.xml \
    %{buildroot}%{_datadir}/metainfo/github.nwkyz.Equinox.metainfo.xml
%if %{defined _host_target}
for loc in zh_CN yue lzh ja ru bo; do
    install -Dm644 %{_host_target}/i18n/$loc/LC_MESSAGES/equinox.mo \
        %{buildroot}%{_datadir}/locale/$loc/LC_MESSAGES/equinox.mo
done
%else
for loc in zh_CN yue lzh ja ru bo; do
    install -Dm644 target/i18n/$loc/LC_MESSAGES/equinox.mo \
        %{buildroot}%{_datadir}/locale/$loc/LC_MESSAGES/equinox.mo
done
%endif

%post
gtk-update-icon-cache %{_datadir}/icons/hicolor &>/dev/null || :
update-desktop-database &>/dev/null || :

%postun
gtk-update-icon-cache %{_datadir}/icons/hicolor &>/dev/null || :
update-desktop-database &>/dev/null || :

%files
%{_bindir}/equinox-gui
%{_bindir}/equinox-supervisor
%{_bindir}/equinox-daemon
%{_datadir}/applications/github.nwkyz.Equinox.desktop
%{_datadir}/icons/hicolor/scalable/apps/equinox.svg
%{_datadir}/metainfo/github.nwkyz.Equinox.metainfo.xml
%{_datadir}/locale/*/LC_MESSAGES/equinox.mo

%changelog
* Sun Sep 06 2026 nwkyz - 0.1.0-1
- Initial package