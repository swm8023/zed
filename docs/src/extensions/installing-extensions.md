---
title: Installing Extensions
description: Browse official extensions and manage local extensions.
---

# Installing Extensions {#installing-extensions}

Open {#action zed::Extensions} and select **Browse Official Extensions** to
search the Zed catalog. Select **Install** on the extension you want to download.

Use **Installed** to search or remove local extensions, including while offline.
Use {#action zed::InstallDevExtension} to load an extension from a local source
directory. To install a prebuilt extension, extract its files into a directory
named for its extension ID under `extensions/installed`. Run
{#action zed::ReloadExtensions} to pick up changes.

## Installation Location

- On macOS, extensions are stored in `~/Library/Application Support/Zed/extensions`.
- On Linux, they are stored in `$XDG_DATA_HOME/zed/extensions` or `~/.local/share/zed/extensions`.
- On Windows, they are stored in `%LOCALAPPDATA%\Zed\extensions`.

The `installed` subdirectory contains extension files. The `work` subdirectory contains files created by extensions, such as downloaded language servers.
