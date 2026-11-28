"""Serve the Zed Web build and its authenticated RPC transport."""

import argparse
import asyncio
from collections import deque
from contextlib import asynccontextmanager, suppress
from html import escape
import json
import logging
import os
from pathlib import Path
import platform
import secrets
import sqlite3
import struct
import subprocess
from urllib.parse import parse_qs, urlencode, urlsplit

import brotli
from starlette.applications import Starlette
from starlette.datastructures import Headers
from starlette.exceptions import HTTPException
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.responses import HTMLResponse, JSONResponse, PlainTextResponse, RedirectResponse, Response
from starlette.routing import Mount, Route, WebSocketRoute
from starlette.staticfiles import StaticFiles
from starlette.websockets import WebSocketDisconnect
import uvicorn
from watchfiles import awatch

from editor_db import EditorDatabase
from web_profile import BINARY_PROFILE_MEDIA_TYPE, ProfileSnapshot, is_profile_path, read_global_config

MAX_FRAME = 16 * 1024 * 1024
SESSION_COOKIE = "zed_local_web_session"
SECURITY_HEADERS = {
    "Cross-Origin-Opener-Policy": "same-origin",
    "Cross-Origin-Embedder-Policy": "require-corp",
    "Cross-Origin-Resource-Policy": "same-origin",
    "Referrer-Policy": "same-origin",
    "X-Content-Type-Options": "nosniff",
    "Cache-Control": "no-store",
}


def return_path(raw):
    if not raw or not raw.startswith("/workspace"):
        return "/workspace"
    if any(ord(character) <= 32 or ord(character) == 127 for character in raw):
        return "/workspace"
    target = urlsplit(raw)
    return raw if target.path == "/workspace" and not target.fragment else "/workspace"


def authorized(connection, session):
    supplied = connection.cookies.get(SESSION_COOKIE, "").encode("utf-8")
    return secrets.compare_digest(supplied, session.encode("ascii"))


def login_form(target, invalid=False):
    error = '<p role="alert">The token is incorrect.</p>' if invalid else ""
    return HTMLResponse(f"""<!doctype html>
<html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Sign in to Zed</title>
<style>body{{font:16px system-ui,sans-serif;margin:10vh auto;padding:1rem;max-width:25rem}}
input,button{{font:inherit;margin:.5rem 0;padding:.5rem}}input{{width:100%;box-sizing:border-box}}</style>
<h1>Sign in to Zed</h1>
<form method="post" action="/login">
<input type="hidden" name="next" value="{escape(target, quote=True)}">
<label for="token">Access token</label>
<input id="token" name="token" type="password" autocomplete="current-password" required autofocus>
{error}<button type="submit">Open workspace</button>
</form></html>""", status_code=401 if invalid else 200)


async def relay_from_browser(socket, stdin):
    while True:
        message = await socket.receive()
        if message["type"] == "websocket.disconnect":
            return
        frame = message.get("bytes")
        if frame is None or len(frame) < 4:
            await socket.close(1003, "Expected a binary Zed RPC frame")
            return
        size, = struct.unpack_from("<I", frame)
        if size > MAX_FRAME or size != len(frame) - 4:
            await socket.close(1002, "Invalid Zed RPC frame size")
            return
        stdin.write(frame)
        await stdin.drain()


async def relay_from_server(socket, stdout):
    while True:
        try:
            header = await stdout.readexactly(4)
            size, = struct.unpack("<I", header)
            if size > MAX_FRAME:
                await socket.close(1009, "Remote server frame is too large")
                return
            payload = await stdout.readexactly(size)
        except asyncio.IncompleteReadError:
            await socket.close(1011, "Remote server closed the connection")
            return
        await socket.send_bytes(header + payload)


async def relay(socket, binary, workspace, state, *, scoped=False):
    identifier = f"web-{secrets.token_hex(6)}"
    env = {**os.environ, "ZED_REMOTE_SERVER_DATA_DIR": str(state)}
    command = ("web", "--workspace-root", str(workspace)) if scoped else ("proxy", "--identifier", identifier)
    process = await asyncio.create_subprocess_exec(
        binary, *command, cwd=workspace, env=env,
        stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
        creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
    )
    tasks = (
        asyncio.create_task(relay_from_browser(socket, process.stdin)),
        asyncio.create_task(relay_from_server(socket, process.stdout)),
    )
    try:
        completed, _ = await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
        for task in completed:
            task.result()
    finally:
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        process.stdin.close()
        try:
            await asyncio.wait_for(process.wait(), 4)
        except TimeoutError:
            if process.returncode is None:
                process.kill()
            await process.wait()


def encoding_quality(headers, encoding):
    qualities = {}
    for item in headers.get("accept-encoding", "").split(","):
        name, *parameters = item.split(";")
        quality = 1.0
        for parameter in parameters:
            key, _, value = parameter.strip().partition("=")
            if key.lower() == "q":
                try:
                    quality = float(value)
                except ValueError:
                    quality = 0.0
        qualities[name.strip().lower()] = quality
    return qualities.get(encoding, qualities.get("*", 0.0))


class WebAssets(StaticFiles):
    async def get_response(self, path, scope):
        content_types = {".wasm": "application/wasm", ".js": "text/javascript", ".bin": "application/octet-stream"}
        asset_type = content_types.get(Path(path).suffix)
        if asset_type and encoding_quality(Headers(scope=scope), "br") > 0:
            try:
                response = await super().get_response(path + ".br", scope)
            except HTTPException as error:
                if error.status_code != 404:
                    raise
            else:
                response.headers["Content-Encoding"] = "br"
                response.headers["Content-Type"] = asset_type
                response.headers["Vary"] = "Accept-Encoding"
                response.headers["Cache-Control"] = "public, max-age=31536000, immutable"
                return response
        response = await super().get_response(path, scope)
        if asset_type:
            response.headers["Vary"] = "Accept-Encoding"
        response.headers["Cache-Control"] = "public, max-age=31536000, immutable"
        return response


def app_for(host, web, workspace, binary, state, token, session, *,
            public_origin=None, global_config=None, direct_tls=False, scoped=False):
    origin = public_origin or f"http://{host}"
    request_host = urlsplit(origin).netloc
    if global_config is not None:
        global_config = global_config.resolve(strict=True)
    database = None
    profile = None
    previous_profiles = deque(maxlen=2)
    profile_lock = asyncio.Lock()
    entry_point = (web / "index.html").read_text(encoding="utf-8")
    profile_version_file = web / "profile.version"
    deferred_icons = profile_version_file.is_file()
    if deferred_icons and profile_version_file.read_text(encoding="ascii").strip() != "1":
        raise ValueError("Invalid appearance profile version in Web bundle")
    version_file = web / "bundle.version"
    versioned_assets = []
    if version_file.is_file():
        version = version_file.read_text(encoding="ascii").strip()
        if len(version) != 64 or any(character not in "0123456789abcdef" for character in version):
            raise ValueError("Invalid version in Web bundle")
        bundle_url = f"/web/{version}"
        entry_point = entry_point.replace("/web/zed_web_workspace.js", f"{bundle_url}/zed_web_workspace.js")
        versioned_assets.append(Mount(bundle_url, WebAssets(directory=web)))
    compressed_entry_point = brotli.compress(entry_point.encode(), quality=11)

    async def refresh_profile():
        nonlocal profile
        async with profile_lock:
            try:
                updated = await asyncio.to_thread(
                    ProfileSnapshot.load, global_config, profile, deferred_icons=deferred_icons,
                    builtin_themes=web / "builtin-themes",
                )
                if (profile is not None and profile.icon_revision is not None
                        and profile.icon_revision != updated.icon_revision):
                    previous_profiles.append(profile)
                profile = updated
            except (OSError, ValueError) as error:
                logging.getLogger(__name__).error("Could not load the global Zed configuration: %s", error)

    async def watch_profile():
        async for changes in awatch(global_config, debounce=250, step=50):
            if any(is_profile_path(global_config, Path(path)) for _, path in changes):
                await refresh_profile()

    @asynccontextmanager
    async def lifespan(_app):
        nonlocal database
        database = EditorDatabase()
        watcher = asyncio.create_task(watch_profile()) if global_config is not None else None
        try:
            await refresh_profile()
            yield
        finally:
            if watcher is not None:
                watcher.cancel()
                with suppress(asyncio.CancelledError):
                    await watcher
            database.connection.close()
    instance_id = secrets.token_hex(16)

    def database_request(request):
        try:
            result = database.call(request["method"], request.get("params", {}))
            return {"result": result}
        except (sqlite3.Error, KeyError, TypeError, ValueError, OverflowError) as error:
            return {"error": str(error)}

    async def sql_http(request):
        if request.headers.get("origin") != origin or not authorized(request, session):
            return JSONResponse({"error": "Invalid editor session"}, status_code=403)
        data = bytearray()
        async for chunk in request.stream():
            if len(data) + len(chunk) > MAX_FRAME:
                return JSONResponse({"error": "Request too large"}, status_code=413)
            data.extend(chunk)
        try:
            payload = json.loads(data)
        except (UnicodeError, ValueError):
            return JSONResponse({"error": "Invalid JSON request"}, status_code=400)
        return JSONResponse(database_request(payload))

    def accepts_socket(socket):
        if direct_tls:
            if socket.url.scheme != "wss":
                return False
        elif socket.client is None or socket.client.host != "127.0.0.1":
            return False
        return (
            socket.headers.get("host") == request_host and socket.headers.get("origin") == origin
            and authorized(socket, session)
        )

    async def sql_socket(socket):
        if not accepts_socket(socket):
            await socket.close(1008)
            return
        await socket.accept()
        await socket.send_json({"method": "Server::hello", "params": {"instance_id": instance_id}})
        try:
            while True:
                try:
                    request = await socket.receive_json()
                except (KeyError, ValueError):
                    await socket.close(1003, "Expected a JSON database request")
                    return
                if not isinstance(request, dict) or not isinstance(request.get("id"), int):
                    await socket.close(1003, "Expected a database request ID")
                    return
                await socket.send_json({"id": request["id"], **database_request(request)})
        except WebSocketDisconnect:
            pass

    async def index(_request):
        return RedirectResponse("/workspace", status_code=303)

    async def editor(request):
        if authorized(request, session):
            headers = {"Vary": "Accept-Encoding, Cookie"}
            if encoding_quality(request.headers, "br") > 0:
                headers["Content-Encoding"] = "br"
                return HTMLResponse(compressed_entry_point, headers=headers)
            return HTMLResponse(entry_point, headers=headers)
        query = request.url.query
        target = return_path(f"/workspace?{query}" if query else "/workspace")
        return RedirectResponse("/login?" + urlencode({"next": target}), status_code=303)

    async def bootstrap(request):
        if not authorized(request, session):
            return PlainTextResponse("Sign in first.", status_code=401)
        architecture = "aarch64" if platform.machine().lower() in {"arm64", "aarch64"} else "x86_64"
        return JSONResponse({"workspace": str(workspace), "architecture": architecture})

    async def global_profile(request):
        if not authorized(request, session):
            return PlainTextResponse("Sign in first.", status_code=401)
        if profile is None:
            return PlainTextResponse("Could not load the global Zed configuration.", status_code=500)
        accepted_types = request.headers.get("accept", "").split(",")
        payload = (profile.binary_payload if any(value.strip() == BINARY_PROFILE_MEDIA_TYPE
                                                  for value in accepted_types)
                   else profile.json_payload)
        headers = {"Cache-Control": "private, no-cache", "ETag": payload.etag,
                   "Vary": "Accept, Accept-Encoding, Cookie"}
        tags = request.headers.get("if-none-match", "").split(",")
        if any(tag.strip().removeprefix("W/") in ("*", payload.etag.removeprefix("W/")) for tag in tags):
            return Response(status_code=304, headers=headers)
        brotli_quality = encoding_quality(request.headers, "br")
        gzip_quality = encoding_quality(request.headers, "gzip")
        if brotli_quality > 0 and brotli_quality >= gzip_quality:
            headers["Content-Encoding"] = "br"
            return Response(payload.brotli_body, media_type=payload.media_type, headers=headers)
        if gzip_quality > 0:
            headers["Content-Encoding"] = "gzip"
            return Response(payload.gzip_body, media_type=payload.media_type, headers=headers)
        return Response(payload.body, media_type=payload.media_type, headers=headers)

    async def global_icon(request):
        if not authorized(request, session):
            return PlainTextResponse("Sign in first.", status_code=401)
        revision = request.path_params["revision"]
        path = request.path_params["path"]
        for snapshot in (profile, *previous_profiles):
            if snapshot is not None and snapshot.icon_revision == revision and path in snapshot.icon_files:
                body = snapshot.icon_files[path]
                headers = {
                    "Cache-Control": "private, max-age=31536000, immutable",
                    "Content-Disposition": "attachment",
                    "Vary": "Accept-Encoding, Cookie",
                }
                compressed = snapshot.brotli_icon_files.get(path)
                if compressed is not None and len(compressed) < len(body) and encoding_quality(request.headers, "br") > 0:
                    body = compressed
                    headers["Content-Encoding"] = "br"
                return Response(body, media_type="application/octet-stream", headers=headers)
        return PlainTextResponse("Icon not found.", status_code=404)

    async def sign_in(request):
        if request.method == "GET":
            target = return_path(request.query_params.get("next"))
            if authorized(request, session):
                return RedirectResponse(target, status_code=303)
            return login_form(target)
        if request.headers.get("origin") != origin:
            return PlainTextResponse("Sign in from the editor page.", status_code=403)
        if request.headers.get("content-type", "").split(";", 1)[0] != "application/x-www-form-urlencoded":
            return PlainTextResponse("Expected a sign-in form.", status_code=415)
        data = bytearray()
        async for chunk in request.stream():
            if len(data) + len(chunk) > 4096:
                return PlainTextResponse("Sign-in form is too large.", status_code=413)
            data.extend(chunk)
        try:
            fields = parse_qs(data.decode("utf-8"), max_num_fields=2)
        except ValueError:
            return PlainTextResponse("Invalid sign-in form.", status_code=400)
        target = return_path(fields.get("next", [None])[0])
        provided = fields.get("token", [""])[0].encode("utf-8")
        if not secrets.compare_digest(provided, token.encode("ascii")):
            return login_form(target, invalid=True)
        response = RedirectResponse(target, status_code=303)
        response.set_cookie(SESSION_COOKIE, session, httponly=True, samesite="strict",
                            secure=public_origin is not None)
        return response

    async def proxy(socket):
        if not accepts_socket(socket):
            await socket.close(1008)
            return
        await socket.accept()
        try:
            await relay(socket, binary, workspace, state, scoped=scoped)
        except WebSocketDisconnect:
            pass
        except (OSError, RuntimeError) as error:
            logging.getLogger(__name__).warning("Zed RPC relay ended: %s", error)

    app = Starlette(routes=[
        Route("/", index),
        Route("/workspace", editor),
        Route("/bootstrap", bootstrap),
        Route("/global-config", global_profile),
        Route("/global-icons/{revision}/{path:path}", global_icon),
        Route("/sql", sql_http, methods=["POST"]),
        WebSocketRoute("/sql-rpc", sql_socket),
        Route("/login", sign_in, methods=["GET", "POST"]),
        WebSocketRoute("/zed-proxy", proxy),
        *versioned_assets,
        Mount("/web", StaticFiles(directory=web)),
    ], lifespan=lifespan)

    async def check_host(request, call_next):
        if request.headers.get("host") != request_host:
            return PlainTextResponse("Use the workspace URL printed by the gateway.", status_code=400)
        response = await call_next(request)
        for name, value in SECURITY_HEADERS.items():
            response.headers.setdefault(name, value)
        return response

    app.add_middleware(BaseHTTPMiddleware, dispatch=check_host)
    return app


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--web", required=True, type=Path)
    parser.add_argument("--workspace", required=True, type=Path)
    parser.add_argument("--state", required=True, type=Path)
    parser.add_argument("--remote-server", required=True, type=Path)
    parser.add_argument("--port", type=int, default=48680)
    parser.add_argument("--listen", choices=("127.0.0.1", "0.0.0.0"), default="127.0.0.1")
    parser.add_argument("--public-origin", help="HTTPS origin used by browsers")
    parser.add_argument("--tls-certfile", type=Path)
    parser.add_argument("--tls-keyfile", type=Path)
    parser.add_argument("--global-config", type=Path, help="Shared Zed appearance and keymap directory")
    args = parser.parse_args()
    web = args.web.resolve(strict=True)
    workspace = args.workspace.resolve(strict=True)
    binary = args.remote_server.resolve(strict=True)
    state = args.state.resolve()
    config = args.global_config.resolve(strict=True) if args.global_config else None
    if config is not None and not config.is_dir():
        parser.error("--global-config must be a directory")
    public_origin = None
    if args.public_origin:
        try:
            public = urlsplit(args.public_origin)
            if (public.scheme != "https" or not public.hostname or public.username is not None
                    or public.path not in ("", "/") or public.query or public.fragment
                    or any(character.isspace() for character in args.public_origin)):
                raise ValueError
            authority = public.netloc.lower()
            if public.port == 443:
                authority = authority.removesuffix(":443")
            public_origin = f"https://{authority}"
        except ValueError:
            parser.error("--public-origin must be an HTTPS origin without a path or credentials")
    direct_tls = args.tls_certfile is not None and args.tls_keyfile is not None
    if bool(args.tls_certfile) != bool(args.tls_keyfile):
        parser.error("--tls-certfile and --tls-keyfile must be supplied together")
    if args.listen == "0.0.0.0" and not direct_tls:
        parser.error("--listen 0.0.0.0 requires TLS")
    if direct_tls and (not public_origin or (urlsplit(public_origin).port or 443) != args.port):
        parser.error("Direct TLS requires an HTTPS --public-origin matching --port")
    if not web.is_dir() or not (web / "index.html").is_file():
        parser.error("--web must point to a packaged browser build")
    if not workspace.is_dir() or not binary.is_file():
        parser.error("--workspace must be a directory and --remote-server must be an executable")
    if not 1024 <= args.port <= 65535:
        parser.error("--port must be between 1024 and 65535")
    socket_path = state / "server_state" / "web-000000000000" / "stdout.sock"
    if os.name == "nt" and len(os.fsencode(socket_path)) >= 108:
        parser.error("--state is too long for a Windows socket; use a shorter directory")
    state.mkdir(parents=True, exist_ok=True)
    token = secrets.token_urlsafe(32)
    session = secrets.token_urlsafe(32)
    host = f"127.0.0.1:{args.port}"
    origin = public_origin or f"http://{host}"
    print(f"Workspace: {origin}/workspace", flush=True)
    print(f"Access token: {token}", flush=True)
    uvicorn.run(
        app_for(host, web, workspace, binary, state, token, session,
                public_origin=public_origin, global_config=config, direct_tls=direct_tls),
        host=args.listen, port=args.port, ws="websockets",
        ssl_certfile=str(args.tls_certfile) if direct_tls else None,
        ssl_keyfile=str(args.tls_keyfile) if direct_tls else None,
        ws_max_size=MAX_FRAME + 4, proxy_headers=False, access_log=False,
        timeout_graceful_shutdown=5,
    )


if __name__ == "__main__":
    main()
