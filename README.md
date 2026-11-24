# Auto Clicker & Key Presser

Click or press keys automatically at any speed, at one spot or a sequence of them. A small, fast Windows app with no ads, no accounts and no review prompts.

## What it does

- **Clicks** with the left, right or middle button, single or double, every 1 ms to 1 hour. Set the speed in milliseconds or in clicks per second. Input is sent with the Windows `SendInput` call.
- **Where:** at the cursor, or at a fixed point you choose with an on-screen picker.
- **Keys:** tap or hold a chosen key on the same timer, alone or together with the click.
- **Stop:** after a number of clicks, after a time limit, or with the hotkey. Optional start delay and random variation of timing and position.
- **Sequences:** an ordered list of click-at-a-point and key steps, each with its own wait, repeated in a loop.
- **Profiles:** save your settings under a name, with a hotkey of their own that loads and starts the profile.
- **Hotkey:** F6 starts and stops by default. Click the hotkey button in Options to rebind it. Hotkeys work while the window is minimised.
- **Tray icon:** sits next to the clock and turns green while a run is active. Click it to bring the window back; right-click for Start/Stop and Quit.
- **Always on top** option for the window.

Windows does not let a normal app send clicks or keys into windows that run as administrator, so the app cannot click inside them. It does not ask for, or use, administrator rights.

## Free and unlock

Everything above is free. A one-off unlock (an offline-checked licence key, no subscription) is planned to lift limits of three sequence steps and one saved profile. In this version no limits apply and no purchase or licence screens are shown.

## Install

- **Microsoft Store:** search for "Auto Clicker & Key Presser".
- **GitHub Releases:** download `Setup.exe` from the latest release of this repository. It installs per user, needs no admin rights, and updates itself from GitHub Releases (switch off with "Automatically update" at the bottom of the window).

The download is under 15 MB.

## Where your data lives

`%LOCALAPPDATA%\AutoClicker`

- `settings.json` - hotkey, window and click settings, update preference
- `profiles.json` - saved profiles

Delete the folder to reset everything.

## Privacy

No telemetry, no accounts, no ads. The only network use is the update check against GitHub Releases (about once a day, unless you turn automatic updates off). Licence keys, once supported, are verified offline.

## Building

```
cargo build --release
cargo test --release
```

Rust 2024, `eframe`/`egui`, Windows only.
