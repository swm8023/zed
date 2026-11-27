"""Build a cacheable snapshot of the current Web appearance."""

from base64 import b64decode, b64encode
from dataclasses import dataclass
import gzip
from hashlib import sha256
import json
from pathlib import PurePosixPath
import re
import struct
from urllib.parse import quote

import brotli


MAX_PROFILE_BYTES = 24 * 1024 * 1024
BINARY_PROFILE_MEDIA_TYPE = "application/vnd.zed.web-profile"
ASSET_FOLDERS = {
    "themes": {".json"},
    "icon_themes": {".json"},
    "icons": {".svg", ".png"},
    "fonts": {".ttf", ".otf", ".ttc"},
}
ICON_GROUPS = ("directory_icons", "named_directory_icons", "chevron_icons", "file_icons")

# Match quoted strings first so comment markers and commas inside them survive.
JSONC_TOKEN = re.compile(r'"(?:\\.|[^"\\])*"|//[^\r\n]*|/\*[\s\S]*?\*/')
JSON_TRAILING_COMMA = re.compile(r'"(?:\\.|[^"\\])*"|,(?=\s*[}\]])')


def parse_jsonc(text):
    without_comments = JSONC_TOKEN.sub(lambda match: match[0] if match[0][0] == '"' else " ", text)
    return json.loads(
        JSON_TRAILING_COMMA.sub(lambda match: match[0] if match[0][0] == '"' else "", without_comments)
    )


def selected_themes(preference):
    if isinstance(preference, str):
        return {preference}
    if isinstance(preference, dict):
        mode = preference.get("mode")
        appearances = (mode,) if mode in ("light", "dark") else ("light", "dark")
        return {preference[key] for key in appearances if isinstance(preference.get(key), str)}
    return set()


def icon_path(value):
    if isinstance(value, str):
        path = PurePosixPath(value)
        if (path.parts and path.parts[0] == "icons" and ".." not in path.parts
                and path.suffix.lower() in ASSET_FOLDERS["icons"]):
            return path.as_posix()
    return None


def icon_references(theme):
    for group in ICON_GROUPS:
        pending = [theme.get(group)]
        while pending:
            value = pending.pop()
            if isinstance(value, dict):
                pending.extend(value.values())
            elif (path := icon_path(value)) is not None:
                yield path


def read_global_config(root, *, builtin_themes=None):
    config = {"assets": {}}
    if root is None:
        return config

    remaining = MAX_PROFILE_BYTES

    def read(path, directory=root):
        nonlocal remaining
        resolved = path.resolve(strict=True)
        if not resolved.is_relative_to(directory):
            raise ValueError("Global configuration files must stay within the configured directory")
        with resolved.open("rb") as stream:
            data = stream.read(remaining + 1)
        remaining -= len(data)
        if remaining < 0:
            raise ValueError("Global configuration exceeds the size limit")
        return data

    def add_asset(path, data):
        key = path.relative_to(root).as_posix()
        config["assets"][key] = b64encode(data).decode("ascii")

    def select_catalog(path, names, directory=root):
        catalog = parse_jsonc(read(path, directory).decode("utf-8-sig"))
        if not isinstance(catalog, dict) or not isinstance(catalog.get("themes"), list):
            raise ValueError(f"{path.name} must contain a theme list")
        variants = [entry for entry in catalog["themes"]
                    if isinstance(entry, dict) and entry.get("name") in names]
        return {**catalog, "themes": variants}

    for key in ("settings", "keymap"):
        path = root / f"{key}.json"
        if path.is_file():
            config[key] = read(path).decode("utf-8-sig")

    settings = parse_jsonc(config.get("settings", "{}"))
    if not isinstance(settings, dict):
        raise ValueError("Global settings must be a JSON object")
    referenced_icons = set()
    installed_themes = set()
    for folder, setting in (("themes", "theme"), ("icon_themes", "icon_theme")):
        names = selected_themes(settings.get(setting))
        if not names:
            continue
        for path in sorted((root / folder).rglob("*.json")):
            if not path.is_file():
                continue
            catalog = select_catalog(path, names)
            if not catalog["themes"]:
                continue
            add_asset(path, json.dumps(catalog, separators=(",", ":")).encode())
            if folder == "themes":
                installed_themes.update(variant["name"] for variant in catalog["themes"])
            else:
                for variant in catalog["themes"]:
                    referenced_icons.update(icon_references(variant))

    missing_themes = selected_themes(settings.get("theme")) - installed_themes
    if missing_themes and builtin_themes is not None:
        directory = builtin_themes.resolve()
        for path in sorted(directory.glob("*/*.json")):
            catalog = select_catalog(path, missing_themes, directory)
            if catalog["themes"]:
                key = "builtin-themes/" + path.relative_to(directory).as_posix()
                data = json.dumps(catalog, separators=(",", ":")).encode()
                config["assets"][key] = b64encode(data).decode("ascii")

    for name in sorted(referenced_icons):
        path = root / name
        if path.is_file():
            add_asset(path, read(path))
    for path in sorted((root / "fonts").rglob("*")):
        if path.suffix.lower() in ASSET_FOLDERS["fonts"] and path.is_file():
            add_asset(path, read(path))
    return config


def defer_profile_icons(config):
    assets = config["assets"]
    files = {path: b64decode(assets.pop(path)) for path in list(assets) if path.startswith("icons/")}
    if not files:
        return None, files

    digest = sha256()
    for path, data in sorted(files.items()):
        digest.update(path.encode() + b"\0")
        digest.update(sha256(data).digest())
    revision = digest.hexdigest()

    def replace_paths(value):
        if isinstance(value, dict):
            return {key: replace_paths(child) for key, child in value.items()}
        path = icon_path(value)
        if path in files:
            return f"/global-icons/{revision}/{quote(path, safe='/')}"
        return value

    for path, payload in assets.items():
        if path.startswith("icon_themes/"):
            catalog = json.loads(b64decode(payload))
            for theme in catalog["themes"]:
                for group in ICON_GROUPS:
                    if group in theme:
                        theme[group] = replace_paths(theme[group])
            assets[path] = b64encode(json.dumps(catalog, separators=(",", ":")).encode()).decode("ascii")

    return revision, files


@dataclass(frozen=True)
class ProfilePayload:
    body: bytes
    brotli_body: bytes
    gzip_body: bytes
    etag: str
    media_type: str

    @classmethod
    def compress(cls, body, etag, media_type):
        return cls(body, brotli.compress(body, quality=11),
                   gzip.compress(body, compresslevel=6, mtime=0), etag, media_type)


def encode_binary_profile(config):
    assets = {name: b64decode(payload) for name, payload in config["assets"].items()}
    manifest = json.dumps({**config, "assets": {name: len(asset) for name, asset in assets.items()}},
                          separators=(",", ":")).encode()
    return b"".join((b"ZCFG\x01", struct.pack("<I", len(manifest)), manifest, *assets.values()))


@dataclass(frozen=True)
class ProfileSnapshot:
    json_payload: ProfilePayload
    binary_payload: ProfilePayload
    icon_revision: str | None
    icon_files: dict[str, bytes]
    brotli_icon_files: dict[str, bytes]

    @classmethod
    def load(cls, root, previous=None, *, deferred_icons=False, builtin_themes=None):
        config = read_global_config(root, builtin_themes=builtin_themes)
        icon_revision, icon_files = defer_profile_icons(config) if deferred_icons else (None, {})
        body = json.dumps(config, separators=(",", ":")).encode()
        digest = sha256(body).hexdigest()
        etag = f'W/"{digest}"'
        if previous is not None and previous.json_payload.etag == etag:
            return previous
        return cls(
            json_payload=ProfilePayload.compress(body, etag, "application/json"),
            binary_payload=ProfilePayload.compress(encode_binary_profile(config),
                                                   f'W/"{digest}-binary-1"', BINARY_PROFILE_MEDIA_TYPE),
            icon_revision=icon_revision,
            icon_files=icon_files,
            brotli_icon_files={path: brotli.compress(icon, quality=5)
                               for path, icon in icon_files.items() if path.endswith(".svg")},
        )


def is_profile_path(root, path):
    relative = path.relative_to(root)
    if len(relative.parts) == 1:
        return relative.name in ("settings.json", "keymap.json", *ASSET_FOLDERS)
    suffixes = ASSET_FOLDERS.get(relative.parts[0])
    return suffixes is not None and (not relative.suffix or relative.suffix.lower() in suffixes)
