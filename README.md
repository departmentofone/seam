# Seam

Seam lets two or more Windows PCs share one mouse, keyboard and clipboard. Push the cursor off the edge of one screen and it carries on onto the next PC, the way a second monitor would. Copy text, images or files on one PC and paste them on the other, or drag a file from Explorer across the edge and drop it.

It's free and MIT licensed, and part of the [Department of One](https://www.departmentofone.net) shelf.

## What it does

- Moves the cursor between PCs at the screen edges you set up on the Displays page, with snapping, a size-match slider for mixed DPI screens, corner dead zones and an optional edge delay.
- Sends keystrokes to whichever PC has the cursor. Ctrl+Alt+Home brings the cursor home; Scroll Lock locks it to one PC.
- Syncs the clipboard: text, HTML, images, and copied files and folders. Pasting files in Explorer streams them straight from the other PC with Windows' own progress dialog.
- Lets you drag files across the edge, send them from the app window, or use the "Seam (other PC)" entry in Explorer's Send to menu.

## Moving an AI workspace

The AI workspace page moves Claude from another paired PC: Claude Code's settings, instructions, skills, plugins, hooks and memory, all session history, the Claude app's session list and scratch folders, MCP servers and per-folder permissions from `.claude.json`, and optionally the project folders the sessions ran in. Sign-in data, caches and live state never move.

- Paths inside the moved files are rewritten for the new PC (user folder, app data, each project's destination), in every spelling Claude writes them.
- Merging is conservative: new files are added, a transcript keeps whichever copy contains the other, JSON settings merge key by key with this PC winning ties, a differing memory is kept as a second file and linked from `MEMORY.md`, and other differences keep this PC's copy and save the incoming one in the backup's `conflicts` folder.
- It shows a dry-run summary before writing, refuses to run while Claude is open, journals every change under `%APPDATA%\Seam\ai-backups`, and can undo the whole move.
- Sessions started from a terminal get entries in the Claude app's list, so every session shows up there. The same button works on its own for sessions already on the PC.

Code: `core/src/aiws/` (`claude.rs` is the adapter; others plug in next to it).

## Security

- No account and no servers. Devices find each other with mDNS and connect directly over QUIC.
- Every connection is TLS 1.3. Devices pair once with a 6-digit code run through SPAKE2, and each one then pins the other's certificate. Unpaired devices are refused before any data is exchanged.
- The device's private key is encrypted with Windows DPAPI.
- File requests are checked against the exact files offered to that device, and incoming names are sanitised so nothing can be written outside the receive folder or overwrite an existing file.
- Clipboard content marked private by password managers is not shared (configurable).

## Known limits

- Windows blocks input into apps running as administrator unless Seam runs elevated too. Settings has a switch that sets up an elevated logon task.
- The sign-in screen and UAC prompts can't be controlled by any app.
- Some games' anti-cheat may object to input hooks. Pause sharing from the tray while you play.
- The installer is not code-signed yet, so SmartScreen shows a warning the first time.

## Layout

```
core/            seam-core: protocol, layout math, networking, input, clipboard, files
  src/win/       Windows hooks, injection, OLE clipboard and drag-and-drop
  tests/net.rs   two network stacks on loopback: pairing, pinning, streams
  examples/      headless engine, driven from stdin (handy for debugging)
app/             Tauri 2 shell (src-tauri) and the Svelte interface (src)
brand/           logo and icon SVGs
```

## Building

Needs Rust (MSVC toolchain), Node 20+ and the Visual Studio C++ build tools.

```
cd app
npm install
npx tauri build        # installer lands in target/release/bundle/nsis
```

`cargo test -p seam-core` runs the unit and network tests. `npm run dev` in `app/` previews the interface in a browser with a mock engine.

Set `SEAM_PROFILE=name` to run a second copy with its own settings folder, and `SEAM_BIND=127.0.0.1` to keep it off the network.
