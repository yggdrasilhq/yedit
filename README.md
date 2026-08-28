# yedit

A tabbed notepad / markdown reader for the Yggdrasil ecosystem — the second
libyggterm consumer (ychrome is the pilot).

`yedit [file...]` inside a yggterm terminal takes over the viewport with a
rendered-markdown / plain-editor page and contributes a **Notes** pane
(vertical note tabs, open-or-create path box, recent files) to the right rail.

- **Markdown mode** (default): read-optimized rendering, wide tables scroll in
  place, light/dark theme-aware. Toggle to a **plain editor** (Ctrl+E).
- **Explicit save** (floppy / Ctrl+S) with a **revision guard**: if the file
  changed on disk since it was opened, yedit prompts (Reload / Overwrite) —
  never a silent clobber.
- **Session persistence**: open tabs, the active tab, and the mode restore on
  the next run (`~/.yggterm/yedit/session.json`, host-resident).
- Outside yggterm it serves its page on a loopback URL for a regular browser.

## Install

**ynpm** — ships with yggterm. One manager keeps every yggdrasilhq binary current across
the whole fleet: generations with rollback, drift-watching, one command.

```sh
ynpm install @ygghq/yedit
```

**No npm, no yggterm?** One curl, straight from the registry:

```sh
curl -fsSL https://raw.githubusercontent.com/yggdrasilhq/yedit/main/install.sh | sh
```

Prebuilt for linux (x64, arm64), macOS (x64, arm64), windows (x64, arm64).

## License

- source code: **GPL-3.0-or-later**, full text in `LICENSE`
- repository documentation (`*.md`): **CC BY-SA 4.0**, see `LICENSE-CC-BY-SA-4.0`
- names and logos: neither licence covers them — see `TRADEMARKS.md`

Copyright 2026 Avikalpa Kundu <avi@gour.top>.

Contributions need a signed CLA — see `CONTRIBUTING.md` and `CLA.md`.
