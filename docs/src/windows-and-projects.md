---
title: Windows & Projects
description: "How Zed handles multiple projects in windows, including project switching and options for opening in new windows."
---

# Windows & Projects

Zed lets you work on multiple projects in a single window. Use the project name in the title bar or Open Recent to switch projects in the current window.

## How Projects Open

By default, when you open a folder in Zed, it opens as a new project in the current window and preserves each project's tabs and layout.

| Action             | Result                  |
| ------------------ | ----------------------- |
| File > Open        | Opens in current window |
| File > Open Recent | Opens in current window |
| Drag folder to Zed | Opens in current window |
| `zed ~/project`    | Opens in current window |

## Working with Multiple Projects

When you have multiple projects open:

- Choose a project from **This Window** in the title bar's project picker or Open Recent.
- Each project has its own file tree, git state, and search scope
- Your workspace layout (splits, tabs) is preserved per project

## Opening in a New Window

Sometimes you want a completely separate window. Here's how:

### From Open Recent

When using File > Open Recent ({#kb projects::OpenRecent}):

- **Enter** or **click** opens in the current window
- **Cmd+Enter** or **Cmd+click** (macOS) / **Ctrl+Enter** or **Ctrl+click** (Linux/Windows) opens in a new window

### From the CLI

Use the `-n` flag to force a new window:

```sh
zed -n ~/projects/other-project
```

Other CLI options for controlling window behavior:

| Flag            | Behavior                                           |
| --------------- | -------------------------------------------------- |
| `-n`, `--new`   | Always open in a new window                        |
| `-a`, `--add`   | Add to the current window                          |
| `-r`, `--reuse` | Replace the current project in the existing window |

See [CLI Reference](./reference/cli.md) for full details.

### Via Settings

You can change the default behavior by setting `cli_default_open_behavior`, which controls CLI opens, and `default_open_behavior`, which controls UI project opens and external open requests, such as macOS Finder's `Open With > Zed`.

To use separate windows, open the Settings Editor and search for `CLI Default Open Behavior` or `Default Open Behavior`, setting either or both to `new_window`. Or add this to your settings.json:

```json [settings]
{
  "default_open_behavior": "new_window",
  "cli_default_open_behavior": "new_window"
}
```

Both settings accept the same values:

- `existing_window` (default): Open folders in the current window. Files outside any open project can also open in an existing window.
- `new_window`: Prefer a new window. For external or CLI file opens, a non-ignored file inside an already-open project opens in that project's window while an unrelated file opens in a new window.

It's worth noting that explicit flags provided to the CLI will override the CLI setting. In particular, `-n` forces a new window even when a path matches an open project. See [CLI Reference](./reference/cli.md) for the available flags.

## Adding Folders to a Project

If you want to add a folder to your current project, you have several options:

- **File menu**: File > Add Folder to Project
- **[Project panel](./project-panel.md)**: Right-click in the project panel and choose "Add Folders to Project"
- **Open Recent**: Select a recent project and click the "Add Folder to this Project" button

This adds the folder as an additional root in your current project's file tree, similar to VS Code's multi-root workspaces.

## See Also

- [Getting Started](./getting-started.md): Essential commands and setup
- [VS Code Migration](./migrate/vs-code.md): How Zed's project model differs from VS Code
