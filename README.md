# dex

Your Android phone as a desktop. Each app runs in its own resizable window on
your Linux desktop, in a full-screen shell with a taskbar and an app launcher.

- **Nothing to install on the phone.** dex uses upstream
  [scrcpy-server](https://github.com/Genymobile/scrcpy) over adb. It needs no
  extra permissions and leaves no background service behind.
- **Real windows, real sizes.** Every window is its own Android virtual
  display. When you resize a window, the phone resizes that display to match,
  so apps re-layout instead of being stretched (tablet layouts included).
- **Sound and clipboard.** Phone audio plays on the PC, and text copied on the
  phone lands on the PC clipboard (Ctrl+V pastes the other way).
- **Fits your desktop.** Follows your light/dark preference live (from the
  XDG desktop portal, so GNOME, KDE and others), and any app window can pop
  out into a native window your window manager tiles, alt-tabs and moves
  between workspaces.
- **No network, no telemetry, no account.** dex only talks to adb.

## Requirements

- Linux with a GPU that supports Vulkan or OpenGL (wgpu)
- `adb`, and `scrcpy` 5.x installed: dex uses its `scrcpy-server` file
  (or set `DEX_SCRCPY_SERVER` and `DEX_SCRCPY_VERSION`)
- FFmpeg libraries (libavcodec, libswscale)
- A phone running Android 10+ with USB debugging enabled

## Optional: companion app for real icons

Without it, apps show as colored letter tiles. The companion is a 13 KB APK
with no UI, no network access and no background service: it only answers
icon requests from adb's shell user.

```sh
companion/build.sh                     # needs ANDROID_HOME (build-tools + a platform)
adb install companion/build/dex-companion.apk
```

Icons are fetched once per device and cached in `~/.cache/dex/`.

## Build and run

```sh
cargo run --release            # fullscreen
cargo run --release -- --windowed
cargo run --release -- -s SERIAL
cargo run --release -- --density 0.8   # smaller app UI (see below)
```

To put dex in your app menu (installs to `~/.local`, no root):

```sh
dist/install.sh            # binary, .desktop entry and icon
dist/install.sh --remove   # undo
```

| Key / action | Does |
|---|---|
| Ctrl+Space, or the ⊞ button | open the app launcher (type to search, Enter to open) |
| Drag a title bar / double-click it | move / maximize |
| Drag an edge or corner | resize (the phone display follows) |
| ← in the title bar | Android back |
| ⇱ in the title bar | pop out into a native window (closing it closes the app) |
| Right-click an app in the taskbar | pop out / pop back in, close, reopen at new density |
| Right-click ⊞ | display density |
| F11 | toggle fullscreen |

Typing, mouse, scrolling and paste (Ctrl+V) go to the focused window.
`DEX_THEME=dark|light` overrides the desktop's theme.

**Display density** sets how big Android draws its UI in app windows: 1.0 is
160 dpi per screen point. Phone apps can look oversized on a laptop; lower
values (e.g. 0.75) give a denser, more desktop-like layout. A display's
density is fixed once it exists, so changes apply to apps you open next (or
use "Reopen at new density"). `DEX_DENSITY` sets the default.

## How it works

```
 dex (Rust, egui + wgpu)                     phone
 ───────────────────────                     ─────
 shell.rs   windows, taskbar, launcher
 session.rs one per window ── adb forward ── scrcpy-server: virtual display
            H.264 in, input out                  + H.264 encoder + input injection
 decode.rs  ffmpeg H.264 → RGBA → texture
```

## Tests

```sh
cargo test --release                       # unit tests + UI tests against a fake phone
DEX_DEVICE_TEST=1 cargo test --release --test device   # end-to-end on a connected phone
cargo run --release --bin dex-probe        # headless stream/resize check, saves PNGs
```

The UI tests render offscreen (egui_kittest + wgpu), so they never open a
window. Set `DEX_SHOTS=<dir>` to save screenshots, and `DEX_AUDIO_SINK=null`
to decode audio without playing it. Set `DEX_DEBUG=1` to see
the phone-side server log.

## Roadmap

- Companion app: notification mirroring, a home screen for desktop mode
- Wireless adb pairing in the UI
- Settings: display density, wallpaper, bitrate

## License

Apache-2.0. scrcpy-server is © Genymobile, also under Apache-2.0. dex is not
affiliated with Genymobile.
