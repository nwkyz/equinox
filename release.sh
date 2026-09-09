#!/usr/bin/env bash
# One-shot release packaging: `cargo build --release`, then produce
#   dist/equinox_<version>_<arch>.deb     (dpkg-deb — any Debian/Ubuntu box)
#   dist/equinox-<version>.tar.gz         (source tarball, for rpmbuild)
#   dist/equinox_<version>_<arch>.rpm     (only where rpmbuild exists)
#   dist/equinox-<version>.flatpak        (portable flatpak bundle)
#
# This script only PACKAGES — it never installs anything into the system or
# the user environment. "Install" is an explicit, separate step:
#
# Usage:
#   ./release.sh                # build all artifacts (deb/rpm/flatpak bundle)
#   ./release.sh --no-flatpak   # skip flatpak (when flatpak tooling is missing)
#   ./release.sh --install-flatpak  # ALSO install the flatpak into the USER
#                                   # layer (opt-in; adds flathub remote and
#                                   # fetches the platform runtime if missing)
#
# The flatpak bundle can be installed on any machine with:
#   flatpak install --user ./dist/equinox-<version>.flatpak
# (the org.gnome.Platform runtime is fetched from flathub on first install)
#
# Prerequisites on Ubuntu/Debian:
#   sudo apt install flatpak-builder       # for the flatpak bundle
#   (rpm needs rpmbuild: sudo apt install rpm; native on Fedora/RHEL)
set -euo pipefail
cd "$(dirname "$0")"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$VERSION" ] || VERSION=0.1.0
ARCH=$(dpkg --print-architecture 2>/dev/null || uname -m)
DIST=dist
NO_FLATPAK=0
FLATPAK_INSTALL=0
for arg in "$@"; do
    case "$arg" in
        --no-flatpak) NO_FLATPAK=1 ;;
        --install-flatpak) FLATPAK_INSTALL=1 ;;
        *) echo "unknown argument: $arg (usage: [--no-flatpak|--install-flatpak])" >&2; exit 1 ;;
    esac
done

mkdir -p "$DIST"

# Keep dist/ to the CURRENT release only: stale per-version artifacts from
# every previous beta (deb/rpm/tar.gz/flatpak) must not pile up across runs.
rm -f "$DIST"/equinox_*.deb "$DIST"/equinox-*.rpm "$DIST"/equinox-*.tar.gz "$DIST"/equinox-*.flatpak

echo "==> cargo build --release"
cargo build --release

# ---------------------------------------------------------------------------
# deb (dpkg-deb)
# ---------------------------------------------------------------------------
if command -v dpkg-deb >/dev/null 2>&1; then
    echo "==> Packaging .deb (arch $ARCH, version $VERSION)"
    root="$DIST/deb-root"
    rm -rf "$root"
    mkdir -p "$root"

    for b in equinox-gui equinox-supervisor equinox-daemon; do
        install -Dm755 "target/release/$b" -t "$root/usr/bin"
    done
    # Absolute Exec — GNOME launches apps from the systemd user PATH where
    # /usr/bin is implicit, but absolute paths are the distro convention.
    install -Dm644 data/github.nwkyz.Equinox.desktop \
        "$root/usr/share/applications/github.nwkyz.Equinox.desktop"
    sed -i "s|^Exec=equinox-gui$|Exec=/usr/bin/equinox-gui|" \
        "$root/usr/share/applications/github.nwkyz.Equinox.desktop"
    install -Dm644 data/equinox.svg \
        "$root/usr/share/icons/hicolor/scalable/apps/equinox.svg"
    for loc in zh_CN yue lzh ja ru bo; do
        install -Dm644 "target/i18n/$loc/LC_MESSAGES/equinox.mo" \
            "$root/usr/share/locale/$loc/LC_MESSAGES/equinox.mo" 2>/dev/null || true
    done
    install -Dm644 packaging/flatpak/github.nwkyz.Equinox.metainfo.xml \
        "$root/usr/share/metainfo/github.nwkyz.Equinox.metainfo.xml"

    mkdir -p "$root/DEBIAN"
    cat > "$root/DEBIAN/control" <<EOF
Package: equinox
Version: $VERSION
Section: graphics
Priority: optional
Architecture: $ARCH
Maintainer: nwkyz
Installed-Size: $(du -sk "$root" | cut -f1)
Depends: libadwaita-1-0, libgtk-4-1, libsoup-3.0-0, libgdk-pixbuf-2.0-0, libglib2.0-0, libc6
Description: Multi-source daily wallpaper manager
 GTK4/libadwaita wallpaper manager with independent per-source schedules,
 a supervised session-resident background (no systemd dependency) and
 pluggable GNOME/KDE/XFCE wallpaper backends.
Trigger: hicolor-icon-theme, desktop-file-utils
EOF
    dpkg-deb --build --root-owner-group "$root" "$DIST/equinox_${VERSION}_${ARCH}.deb" >/dev/null
    rm -rf "$root"
    echo "    -> $DIST/equinox_${VERSION}_${ARCH}.deb"
else
    echo "==> dpkg-deb 不在本机,跳过 .deb(在 Debian/Ubuntu 上构建)"
fi

# ---------------------------------------------------------------------------
# rpm — needs rpmbuild (`sudo apt install rpm` on Debian/Ubuntu; native on
# Fedora/RHEL). Uses the conventional ~/rpmbuild layout so Ubuntu's rpmbuild
# finds the sources/spec without the -tb "spec inside tarball" restriction.
# ---------------------------------------------------------------------------
if command -v rpmbuild >/dev/null 2>&1; then
    echo "==> Packaging .rpm (needs rpmbuild)"
    # --transform adds the {name}-{version}/ top dir so `%setup -q` in the
    # spec unpacks cleanly.
    tar czf "$DIST/equinox-$VERSION.tar.gz" \
        --transform "s#^#equinox-$VERSION/#" \
        --exclude=target --exclude=dist --exclude=.git --exclude=.commandcode \
        --exclude=.flatpak-builder .
    rpmtop="$HOME/rpmbuild"
    mkdir -p "$rpmtop"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
    # Stale rpms from every previous version accumulate in RPMS/<arch>/ and
    # would all be copied into dist/ below — purge them before the build so
    # only THIS version's rpm is produced and shipped.
    rm -rf "$rpmtop"/RPMS/*
    install -m644 "$DIST/equinox-$VERSION.tar.gz" "$rpmtop/SOURCES/"
    install -m644 packaging/rpm/equinox.spec "$rpmtop/SPECS/equinox.spec"
    # _topdir: explicit tree (Ubuntu rpm may not default to ~/rpmbuild);
    # _dbpath: Ubuntu cannot write the system /var/lib/rpm db, use a
    # user-local one so non-root builds succeed. --nodeps: the build
    # toolchain is already proven by the `cargo build` above, and Ubuntu's
    # empty user db can't resolve Fedora-named BuildRequires.
    # _enable_debug_packages 0: Ubuntu rpm 6 trips on an empty
    # debugfiles.list when auto-generating the debuginfo subpackage.
    # _host_target: point the spec at the host release build so the rpm is
    # assembled in seconds instead of recompiling the workspace.
    # _equinox_version / _equinox_source: derived from Cargo.toml (the ONE
    # source of truth) — rpm forbids '-' in Version, so "0.1.1-a1" becomes the
    # rpm package version "0.1.1a1" while the source tarball keeps the Cargo
    # form.
    RPM_VERSION="${VERSION//-/}"
    rpmbuild -bb --nodeps \
        --define "_topdir $rpmtop" \
        --define "_dbpath $rpmtop/rpmdb" \
        --define "_enable_debug_packages 0" \
        --define "_host_target $PWD/target" \
        --define "_equinox_version $RPM_VERSION" \
        --define "_equinox_source $VERSION" \
        "$rpmtop/SPECS/equinox.spec" >/dev/null
    # Copy ONLY the freshly built rpm (RPMS/ was purged above; the pattern
    # pins the exact version in case anything else lands there).
    find "$rpmtop/RPMS" -name "equinox-${RPM_VERSION}-*.rpm" -exec cp {} "$DIST/" \;
    echo "    -> $(ls "$DIST"/equinox-${RPM_VERSION}-*.rpm 2>/dev/null)"
else
    echo "==> rpmbuild 未安装,跳过 .rpm。要本机产出 .rpm(例如 Ubuntu):"
    echo "      sudo apt install rpm"
    echo "    Fedora/RHEL 自带 rpmbuild,直接 ./release.sh 即可"
fi

# ---------------------------------------------------------------------------
# flatpak — build a portable bundle (no install; installation is opt-in via
# --install-flatpak). "Install" side effects (remote-add, runtime fetch,
# app install) MUST NOT happen without an explicit flag.
# ---------------------------------------------------------------------------
if [ "$NO_FLATPAK" = "1" ]; then
    echo "==> flatpak 已跳过(--no-flatpak)"
elif [ "$FLATPAK_INSTALL" = "1" ]; then
    echo "==> flatpak: 构建并安装到用户层(--install-flatpak)"
    flatpak remote-add --user --if-not-exists \
        flathub https://dl.flathub.org/repo/flathub.flatpakrepo
    flatpak-builder --user --force-clean --install --install-deps-from=flathub \
        --disable-rofiles-fuse \
        "$DIST/flatpak-build" packaging/flatpak/github.nwkyz.Equinox.yml
    echo "    安装完成:flatpak run github.nwkyz.Equinox"
elif command -v flatpak-builder >/dev/null 2>&1; then
    echo "==> flatpak: only building a portable bundle (nothing is installed)"
    if ! flatpak info org.gnome.Platform//50 >/dev/null 2>&1; then
        echo "    org.gnome.Platform 50 runtime 未安装(构建需要它)。先装运行时:"
        echo "      flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo"
        echo "      flatpak install --user flathub org.gnome.Platform//50"
        echo "    (或直接 ./release.sh --install-flatpak,它会顺带拉取并安装)"
    else
        flatpak-builder --repo="$DIST/flatpak-repo" --force-clean \
            --disable-rofiles-fuse \
            "$DIST/flatpak-build" packaging/flatpak/github.nwkyz.Equinox.yml
        flatpak build-bundle "$DIST/flatpak-repo" \
            "$DIST/equinox-$VERSION.flatpak" github.nwkyz.Equinox master
        echo "    -> $DIST/equinox-$VERSION.flatpak(便携 bundle)"
        echo "       安装(目标机器):flatpak install --user $DIST/equinox-$VERSION.flatpak"
    fi
else
    echo "==> flatpak-builder 未安装,跳过 flatpak 构建。"
    echo "    请先安装: sudo apt install flatpak-builder"
    echo "    然后重新运行 ./release.sh(或 --install-flatpak)"
fi

echo
echo "==> 完成。产物在 $DIST/:"
ls -lh "$DIST" 2>/dev/null | sed 's/^/    /'
