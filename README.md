<p align="center"><img src="assets/logo.svg" width="96" alt="Wade logo"></p>

# Wade

A card-row window switcher that replaces Alt+Tab on Windows.

Website: https://wade.erfjab.com/

- Shows your windows as live cards in one horizontal row, like recent apps on a phone.
- Uses the system's own desktop background, slightly dimmed.
- Runs in the background and starts with Windows.
- Uses no CPU while idle.

## Install

In PowerShell:

```powershell
irm https://raw.githubusercontent.com/erfjab/wade/master/install.ps1 | iex
```

The same command installs Wade the first time and updates it later. It only replaces
the program. If the download fails, the installed version is left as it was.

## Build from source

### Requirements

- Windows 10 or 11
- [Rust](https://rustup.rs) with the MSVC toolchain
- Windows 10/11 SDK (install it with the Visual Studio Build Tools)

### Build and install

Open PowerShell in the project folder and run:

```powershell
cargo build --release
taskkill /im wade.exe /f 2>$null
mkdir "$env:LOCALAPPDATA\Programs\Wade" -Force
copy target\release\wade.exe "$env:LOCALAPPDATA\Programs\Wade\"
& "$env:LOCALAPPDATA\Programs\Wade\wade.exe"
```

Always start Wade from the install folder, not from `target`. On launch it adds
itself to Windows startup using its own path.

## Update

Pull or edit the code, then run the same commands as in Install.

## Uninstall

1. Right-click the Wade tray icon and turn off **Run as administrator** (if on), then
   **Start with Windows**.
2. Right-click it again and choose **Exit**.
3. Delete `%LOCALAPPDATA%\Programs\Wade`.
4. To also remove the saved choice, delete the registry key `HKCU\Software\Wade`.

## Use

Hold `Alt` and press `Tab`. Wade opens on the current window; each `Tab` moves one card.

| Key | Action |
|---|---|
| `Alt+Tab` | open, then next card |
| `Tab` / `Shift+Tab`, `Right` / `Left`, mouse wheel | next / previous card |
| release `Alt`, `Enter`, click the middle card | switch |
| `Up`, `Delete`, drag the middle card up | close that window |
| `Esc` | cancel |

- The row loops: from the last card to the first and back.
- On open, the current window shrinks from where it is into the row; on switch, the chosen
  card grows into the real window.
- A quick `Alt+Tab` (press and release) goes straight to the previous window without showing anything.
- If Windows "Animation effects" is off, Wade opens and closes without animation.

## Settings

Right-click the Wade tray icon.

| Setting | Options |
|---|---|
| Animation speed | **Fast**, Normal, Slow, Off |
| Card size | **Large**, Medium, Small, Icons only (app icons and titles, no live previews) |
| Background | Black, 75% darker, **50% darker**, 25% darker (the desktop behind the cards) |
| Start with Windows | **on**, off |
| Run as administrator | **on**, off |

Defaults are in bold.

If Windows "Animation effects" is off, Wade has no animation whatever the speed.

**Run as administrator** lets Wade switch away from games and apps that run as administrator.
The first start asks for administrator rights once (UAC); Wade then starts from a Task Scheduler
task (named Wade) at sign-in, with no prompt. If you decline, Wade runs normally and does not ask
again; you can turn it on later from the menu. Settings are stored in `HKCU\Software\Wade`.

## Why it is light

- Windows itself (DWM thumbnails) draws the live previews; Wade copies no images.
- When the switcher is closed, no timer or loop runs.
- The keyboard hook runs on its own thread and only posts messages, so typing never waits on drawing.
- The drawing buffer exists only while the switcher is open.
- No network, no files written, no code injected into other apps. Admin rights only if you turn them on.

## Limits

- While a window started with "Run as administrator" has focus, Windows does not pass the
  keys to Wade, and the Windows Alt+Tab opens instead. Turn on **Run as administrator** to fix this.
- Minimized windows have no live preview; they show the app's large icon.
- Only windows on the current virtual desktop are shown.

## Develop

```powershell
cargo run          # debug build
.\run.bat          # stop the running copy, build, and start the new one
```

Only one Wade runs at a time. A second copy exits right away.

| File | Purpose |
|---|---|
| `src/main.rs` | startup, single instance |
| `src/hook.rs` | catches Alt+Tab and the switcher keys |
| `src/winlist.rs` | finds windows and brings the chosen one to the front |
| `src/switcher.rs` | card row, animation, drawing and mouse |
| `src/tray.rs` | tray icon, start with Windows, exit |
| `site/` | website, published to GitHub Pages |
| `assets/` | logo (`logo.svg`) and the app icon embedded in `wade.exe` |
