![Equinox - Daily glimpse](https://raw.githubusercontent.com/nwkyz/nwkyz-picbed/main/storage/equinox-banner3.png)

<p align="center"><b>Refresh your linux desktop everyday</b></p>  

A multi-source online wallpaper manager built with **GTK4 / libadwaita**. Refreshes at the interval you set.

## Features

- **Wallpaper sources**
  - Bing daily wallpaper
  - Windows Spotlight
  - NASA Astronomy Picture of the Day (APOD)
  - Wikimedia Picture of the Day (POTD)
  - *Working on: Local folder & Unsplash collections*
- **Scheduled update** - Auto update on your set interval.
- **Deduplicated storage** - the same image is stored once.
- **Background service** - No systemd (Optional); the daemon stays lean at idle (~11 MB) because image downloads run in a short-lived child process.
- **Multiple backends** - Tested on Ubuntu/Fedora with GNOME/Plasma/Xfce, X11 and Wayland.

## Quick Start (recommended)

### 1. Download package from Releases page.

### 2. Install

**Using system package manager:**

```bash
sudo dpkg -i equinox_0.1.2_amd64.deb     # For Debian/Ubuntu based distros

sudo rpm -Uvh equinox-0.1.2-1.x86_64.rpm # For Fedora/RedHat/SUSE based distros
```

**Using Flatpak package:**

```bash
flatpak install --user ./equinox-0.1.2.flatpak
flatpak run github.nwkyz.Equinox
```

> **Flatpak note:** the sandbox cannot reach the host's systemd user session, so the "systemd hosting" option is hidden there; and a background started from the sandboxed GUI stops when the GUI closes. For always-on background in flatpak, add a login autostart entry that runs:
> `flatpak run --command=equinox-supervisor github.nwkyz.Equinox` 
> (Usually this will be ran automatically during installation).

## Manually Build & Install

### Requirements

- A Linux desktop: GNOME 50 (GTK 4.22+ / libadwaita 1.9) verified; KDE Plasma 6
  and XFCE 4.20 are supported wallpaper targets.
- Rust toolchain 1.75+ (an offline build works when the deps are cached).

### Build

```bash
cargo build --release       # builds equinox-daemon + equinox-gui + supervisor
cargo test --workspace      # optional: run the test suite
```

### Package everything

```bash
./release.sh                # cargo build --release + packages the artifacts
```

Outputs land in `dist/`:

| Artifact                          | Notes |
|-----------------------------------|-------|
| `equinox_<ver>_<arch>.deb`        | for Debian/Ubuntu; includes all three binaries, desktop file, icon, six translation catalogs and AppStream metadata |
| `equinox-<ver>-1.<arch>.rpm`      | for Fedora/RHEL (built when `rpmbuild` is present) |
| `equinox-<ver>.tar.gz`            | source tarball for distro packagers (`packaging/rpm/equinox.spec` included) |
| `equinox-<ver>.flatpak`           | portable flatpak bundle (build-only by default; nothing is installed) |

- `--no-flatpak` skips the flatpak build.
- `--install-flatpak` also installs the flatpak into your *user* session
  (pulls the `org.gnome.Platform//50` runtime from Flathub as needed, no sudo).

The one optional build tool you may need to add: `sudo apt install
flatpak-builder` (for the flatpak bundle).

### Install into your user profile (no root)

```bash
./data/install.sh          # installs binaries, desktop file, icon, translations into ~/.local
```

Alternatively `systemd` users can run the provided user service:

```bash
systemctl --user enable --now equinox-supervisor.service
```

### Uninstall

```bash
pkill -x equinox-supervisor   # stop the background (and its daemon)
./data/uninstall.sh           # remove Equinox, keep your pictures and history
```

## Known Issues 

- **Flatpak limits** — no systemd hosting (see the "Quick Start" note above)
- **Headless sessions** — wallpaper backends that need a running desktop
  session (Plasma's `setWallpaper`, XFCE's xfconf daemon) require the session;
  where possible Equinox falls back to editing the backend's config file
  directly.
- **Plasma multi-monitor probing** currently checks up to 4 screens.
- In a flatpak sandbox the XFCE backend talks to the host's xfconf D-Bus
  service (the sandbox has no `xfconf-query`/`xrandr`).


## Help Translate

Equinox is English-source gettext. All user-visible strings go through
`gettext()`; English needs no catalogue — untranslated strings simply fall
back to English.

Current supported languages: 
中文 (普通话/粵語/文言), English, 日本語, Русский, བོད་ཡིག

To add or update a language:

1. Regenerate the template after any string change:
   ```sh
   xtr --package-name equinox --package-version 0.1.2 \
       -o po/equinox.pot $(find crates -name '*.rs' | sort)
   ```
2. Start a new catalogue (e.g. French):
   ```sh
   msginit --locale=fr_FR --input=po/equinox.pot --output=po/fr.po
   ```
3. Translate `po/<locale>.po` (fill the `msgstr` column).
4. Rebuild — `cargo build --release` compiles every `po/*.po` into
   `target/i18n/<locale>/LC_MESSAGES/equinox.mo` automatically.
5. Send us the finished `.po` (see the repo for how to report).

Switch the UI language at runtime via **menu → Preferences**.

## Screenshots

I will drop screenshots here later.

## Project layout

```
crates/
  equinox-core/    shared library: sources, config, storage, wallpaper
                   backends (GNOME/Plasma/XFCE), autostart, history
  equinox-daemon/  background service: per-source schedulers + D-Bus
                   (github.nwkyz.Equinox.Daemon1); ships the resident
                   equinox-supervisor watchdog
  equinox-gui/     libadwaita UI: Wallpaper / Sources / Gallery / History /
                   fullscreen viewer / task timeline / OOBE
data/              install scripts, desktop file, icon, optional systemd unit
packaging/         flatpak manifest & AppStream metadata, RPM spec
po/                gettext catalogues (equinox.pot + *.po)
```
## Already supported and tested on
| Distro                    | DE                   | Package    |
|---------------------------|----------------------|------------|
| Ubuntu 26.04 x86_64       | GNOME 50 Wayland     | dpkg       |
| Ubuntu 26.04 x86_64       | GNOME 50 Wayland     | flatpak    |
| Fedora 44 x86_64          | Plasma 6.6.4 Wayland | rpm        |
| Fedora 44 x86_64          | Plasma 6.6.4 Wayland | flatpak    |
| Xubuntu 26.04 x86_64      | Xfce 4.20 X11        | dpkg       |
| Xubuntu 26.04 x86_64      | Xfce 4.20 X11        | flatpak    |


## Adding a new wallpaper source

1. Add a file under `crates/equinox-core/src/sources/` implementing the
   `Source` trait.
2. Export it in `sources/mod.rs` and register it in `source.rs::registry()`.
3. Done — the GUI automatically builds the source's settings page, schedule,
   gallery and wallpaper rules.