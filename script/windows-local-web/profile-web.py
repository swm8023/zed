"""Collect a Windows editor startup profile and measure the cold bundle transfer."""
import argparse
import asyncio
from base64 import b64decode, b64encode
from hashlib import sha256
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
from tempfile import TemporaryDirectory
from urllib.request import urlopen

import uvicorn
from starlette.responses import FileResponse, JSONResponse
from websockets.asyncio.client import connect

from gateway import SECURITY_HEADERS, SESSION_COOKIE, app_for


PROFILE = """globalThis.readStartupProfile = async () => {
    const path = performance.getEntriesByType('resource')
        .find(entry => entry.name.endsWith('/zed_web_workspace.js')).name;
    const wasm = await import(path).then(module => module.default());
    const size = wasm.__write_profile(0, 0);
    const ptr = wasm.__wbindgen_malloc_command_export(size, 1);
    wasm.__write_profile(ptr, size);
    const bytes = new Uint8Array(wasm.memory.buffer, ptr, size);
    const chunks = [];
    for (let offset = 0; offset < bytes.length; offset += 16384) {
        chunks.push(String.fromCharCode(...bytes.subarray(offset, offset + 16384)));
    }
    wasm.__wbindgen_free_command_export(ptr, size, 1);
    return btoa(chunks.join(''));
};"""


async def devtools_page(directory):
    async with asyncio.timeout(30):
        while True:
            try:
                port = (directory / "DevToolsActivePort").read_text().splitlines()[0]
                with urlopen(f"http://127.0.0.1:{port}/json/list", timeout=2) as response:
                    return next(tab["webSocketDebuggerUrl"] for tab in json.load(response) if tab["type"] == "page")
            except (OSError, IndexError, StopIteration, ValueError):
                await asyncio.sleep(0.2)


async def observe(page, origin, session, profile, gpu_timeout):
    async with connect(page, max_size=4 << 20) as ws:
        sequence = 0

        async def call(method, **params):
            nonlocal sequence
            sequence += 1
            await ws.send(json.dumps({"id": sequence, "method": method, "params": params}))
            while True:
                data = json.loads(await asyncio.wait_for(ws.recv(), 15))
                if data.get("id") == sequence:
                    if "error" in data:
                        raise RuntimeError(data["error"])
                    return data["result"]

        async def evaluate(expression, *, wait=False):
            result = await call("Runtime.evaluate", expression=expression, returnByValue=True, awaitPromise=wait)
            if "exceptionDetails" in result:
                raise RuntimeError(result["exceptionDetails"])
            return result["result"].get("value")

        cookie = await call("Network.setCookie", name=SESSION_COOKIE, value=session, url=origin, httpOnly=True)
        if not cookie["success"]:
            raise RuntimeError("Could not set the loopback profiling cookie")
        await call("Emulation.setDeviceMetricsOverride", width=1440, height=900, deviceScaleFactor=1, mobile=False)
        await call("Page.navigate", url=origin)
        async with asyncio.timeout(20):
            while not await evaluate("!!document.querySelector('#projects option[value=example]')"):
                await asyncio.sleep(0.1)
        await evaluate("""(() => {
            const select = document.querySelector('#projects');
            select.value = 'example';
            select.dispatchEvent(new Event('change'));
        })()""")
        state = {}
        try:
            async with asyncio.timeout(gpu_timeout):
                while True:
                    state = await evaluate("""(() => {
                        const frame = document.querySelector('iframe:not([data-pending])');
                        const editor = frame?.contentDocument;
                        const status = document.querySelector('#status');
                        return {done: !!editor?.querySelector('canvas'), status: status?.innerText,
                                failed: !!status?.innerText && !document.querySelector('iframe')
                                    && !document.querySelector('#projects')?.disabled};
                    })()""")
                    if state["done"]:
                        break
                    if state.get("failed"):
                        raise RuntimeError(state.get("status"))
                    await asyncio.sleep(0.2)
        except TimeoutError:
            raise RuntimeError(f"The WebGPU workspace did not start ({state.get('status')})") from None

        transferred = await evaluate("""(() => {
            const frame = document.querySelector('iframe:not([data-pending])').contentWindow;
            return [performance, frame.performance].flatMap(timing => [
                ...timing.getEntriesByType('navigation'), ...timing.getEntriesByType('resource'),
            ]).reduce((total, entry) => total + entry.transferSize, 0);
        })()""")
        print(f"Cold browser HTTP transfer to working editor: {transferred} bytes", flush=True)
        if profile:
            encoded = await evaluate("document.querySelector('iframe').contentWindow.readStartupProfile()", wait=True)
            profile.write_bytes(b64decode(encoded, validate=True))
        return transferred


def loopback_certificate(scratch):
    openssl = shutil.which("openssl")
    if not openssl:
        git_core = subprocess.check_output(["git", "--exec-path"], text=True,
                                           creationflags=subprocess.CREATE_NO_WINDOW).strip()
        openssl = Path(git_core).parents[2] / "usr/bin/openssl.exe"
    certificate = scratch / "test.crt"
    key = scratch / "test.key"
    flags = subprocess.CREATE_NO_WINDOW
    subprocess.run([openssl, "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                    "-subj", "/CN=127.0.0.1", "-addext", "subjectAltName=IP:127.0.0.1",
                    "-keyout", key, "-out", certificate], check=True, stderr=subprocess.DEVNULL,
                   creationflags=flags)
    public_key = subprocess.check_output([openssl, "x509", "-in", certificate, "-pubkey", "-noout"],
                                         creationflags=flags)
    der = subprocess.run([openssl, "pkey", "-pubin", "-outform", "DER"], input=public_key,
                         check=True, stdout=subprocess.PIPE, creationflags=flags).stdout
    return certificate, key, b64encode(sha256(der).digest()).decode("ascii")


def managed_site(app, web):
    workspace_id = secrets.token_urlsafe(32)
    prefix = f"/_zed/workspace/{workspace_id}/"
    version = (web / "bundle.version").read_text(encoding="ascii").strip()
    assets = f"/web/{version}/"
    sites = {
        "/_zed/site": {"login": "/"},
        "/_zed/auth/status": {"authenticated": True, "csrfToken": "local"},
        "/_zed/projects": {"projects": [
            {"projectId": "example", "name": "Example", "hubId": "local", "online": True},
        ]},
        "/_zed/workspace/": {"sessionId": workspace_id, "assets": assets, "sessionPath": prefix},
    }

    async def route(scope, receive, send):
        path = scope.get("path", "")
        if path == "/":
            return await FileResponse(web / "wheelmaker.html", headers=SECURITY_HEADERS)(scope, receive, send)
        if path in sites:
            return await JSONResponse(sites[path], headers=SECURITY_HEADERS)(scope, receive, send)
        if path.startswith(prefix):
            mapped = dict(scope)
            mapped["path"] = "/" + path[len(prefix):]
            mapped["raw_path"] = mapped["path"].encode()
            return await app(mapped, receive, send)
        return await app(scope, receive, send)

    return route


async def measure(build, web, scratch, browser, profile, max_bytes, software):
    workspace = scratch / "example"
    workspace.mkdir()
    (workspace / "main.go").write_text('package main\n\nfunc main() {}\n', encoding="utf-8")
    (workspace / "go.mod").write_text('module example.com/zed-profile\n\ngo 1.22\n', encoding="utf-8")
    config = scratch / "config"
    config.mkdir()
    (config / "settings.json").write_text(json.dumps({
        "session": {"trust_all_worktrees": True, "restore_unsaved_buffers": False},
    }), encoding="utf-8")
    session = secrets.token_urlsafe(32)
    binary = build / "cargo/release-fast/remote-server.exe"
    certificate, key, spki = loopback_certificate(scratch)
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        host = f"127.0.0.1:{listener.getsockname()[1]}"
        origin = f"https://{host}"
        app = app_for(host, web, workspace, binary, scratch / "state", secrets.token_urlsafe(32), session,
                      public_origin=origin, global_config=config, scoped=True, direct_tls=True)
        server = uvicorn.Server(uvicorn.Config(managed_site(app, web), log_level="warning", timeout_graceful_shutdown=4,
                                               ssl_certfile=str(certificate), ssl_keyfile=str(key)))
        serving = asyncio.create_task(server.serve(sockets=[listener]))
        chrome_profile = scratch / "edge"
        flags = ["--use-webgpu-adapter=swiftshader", "--enable-unsafe-swiftshader"] if software else []
        process = None
        try:
            async with asyncio.timeout(20):
                while not server.started:
                    await asyncio.sleep(0.1)
            process = subprocess.Popen([
                str(browser), "--headless=new", "--enable-unsafe-webgpu", "--disable-extensions",
                "--no-first-run", "--disable-sync", "--remote-debugging-port=0",
                f"--user-data-dir={chrome_profile}", f"--ignore-certificate-errors-spki-list={spki}",
                *flags, "about:blank",
            ], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)
            page = await devtools_page(chrome_profile)
            transferred = await observe(page, origin, session, profile, 70)
            if max_bytes is not None and transferred > max_bytes:
                raise RuntimeError(f"Cold editor transfer of {transferred} bytes exceeds {max_bytes}")
        finally:
            if process and process.poll() is None:
                process.terminate()
                process.wait(timeout=10)
            server.should_exit = True
            await serving


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("build", type=Path)
    parser.add_argument("--splitter", type=Path)
    parser.add_argument("--profile", type=Path)
    parser.add_argument("--max-bytes", type=int)
    parser.add_argument("--software", action="store_true")
    parser.add_argument("--browser", type=Path, default=Path(os.environ["ProgramFiles(x86)"]) / "Microsoft/Edge/Application/msedge.exe")
    args = parser.parse_args()
    if bool(args.profile) != bool(args.splitter):
        parser.error("Pass --profile and --splitter together to capture the startup profile")
    build = args.build.resolve(strict=True)
    with TemporaryDirectory(prefix="zed-web-profile-") as directory:
        scratch = Path(directory)
        web = build / "web"
        if args.profile:
            instrumented = scratch / "web"
            shutil.copytree(web, instrumented, ignore=shutil.ignore_patterns("*.br"))
            with (instrumented / "index.html").open("a", encoding="utf-8") as entry:
                entry.write(f"\n<script type=module>{PROFILE}</script>\n")
            name = "zed_web_workspace_bg.wasm"
            subprocess.run([args.splitter.resolve(strict=True), web / name,
                            "--instrument", "-o", instrumented / name], check=True,
                           creationflags=subprocess.CREATE_NO_WINDOW)
            web = instrumented
        asyncio.run(measure(build, web, scratch, args.browser, args.profile, args.max_bytes, args.software))


if __name__ == "__main__":
    main()
