import asyncio
import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
from tempfile import TemporaryDirectory

from starlette.responses import HTMLResponse
import uvicorn
from websockets.asyncio.client import connect

loader = importlib.util.spec_from_file_location("release_profile", Path(__file__).with_name("profile-web.py"))
profiler = importlib.util.module_from_spec(loader)
loader.loader.exec_module(profiler)

async def request_device(page, origin):
    async with connect(page) as ws:
        message_id = 0

        async def call(method, **params):
            nonlocal message_id
            message_id += 1
            await ws.send(json.dumps({"id": message_id, "method": method, "params": params}))
            while True:
                data = json.loads(await ws.recv())
                if data.get("id") == message_id:
                    return data["result"]

        version = await call("Browser.getVersion")
        await call("Page.navigate", url=origin)
        while True:
            frame = await call("Runtime.evaluate", expression="!!document.querySelector('iframe')?.contentDocument?.querySelector('canvas')")
            if frame["result"].get("value"):
                break
            await asyncio.sleep(0.1)
        result = await call("Runtime.evaluate", awaitPromise=True, returnByValue=True, expression="""(async () => {
          const w = document.querySelector('iframe').contentWindow;
          const info = {secure: w.isSecureContext, isolated: w.crossOriginIsolated, gpu: !!w.navigator.gpu};
          try {
            const gpu = w.navigator.gpu;
            const adapter = await gpu?.requestAdapter({powerPreference: 'high-performance'});
            if (!adapter) {
              info.error = 'No WebGPU adapter';
              return info;
            }
            info.adapter = {description: adapter.info.description, architecture: adapter.info.architecture, fallback: adapter.info.isFallbackAdapter};
            const context = w.document.querySelector('canvas').getContext('webgpu');
            w.document.querySelector('canvas').width = 1440;
            w.document.querySelector('canvas').height = 900;
            const optional = 'dual-source-blending';
            const device = await adapter.requestDevice({
              requiredFeatures: adapter.features.has(optional) ? [optional] : [],
              requiredLimits: {
                maxTextureDimension2D: adapter.limits.maxTextureDimension2D,
                maxBindGroups: 4, maxSamplersPerShaderStage: 16, maxUniformBuffersPerShaderStage: 12,
                maxStorageBuffersPerShaderStage: 4, maxStorageTexturesPerShaderStage: 4,
                maxBufferSize: 268435456, maxStorageBufferBindingSize: 134217728
              }
            });
            context.configure({device, format: gpu.getPreferredCanvasFormat()});
            const view = context.getCurrentTexture().createView();
            const command = device.createCommandEncoder();
            command.beginRenderPass({colorAttachments:[{
              view, clearValue:{r:0,g:0,b:0,a:1}, loadOp:'clear', storeOp:'store'
            }]}).end();
            device.queue.submit([command.finish()]);
            await device.queue.onSubmittedWorkDone();
            info.rendered = true;
          } catch (error) {
            info.error = String(error);
          }
          return info;
        })()""")
        return {"browser": version["product"], **result["result"]["value"]}


async def probe(origin, scratch, fingerprint, browser, flags):
    flags = list(flags)
    profile = scratch / "edge"
    output = scratch / "edge.log"
    with output.open("w", encoding="utf-8") as log, subprocess.Popen([
        browser, "--headless=new", "--enable-unsafe-webgpu", "--disable-extensions",
        "--no-first-run", "--disable-sync", "--remote-debugging-port=0",
        "--enable-logging=stderr",
        f"--user-data-dir={profile}", f"--ignore-certificate-errors-spki-list={fingerprint}",
        *flags, "about:blank",
    ], stdout=subprocess.DEVNULL, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW) as process:
        try:
            async with asyncio.timeout(20):
                page = await profiler.devtools_page(profile)
                result = await request_device(page, origin)
        finally:
            process.terminate()
            process.wait(timeout=8)
    if not result.get("rendered"):
        result["diagnostics"] = [line for line in output.read_text(encoding="utf-8", errors="replace").splitlines()
                                 if any(word in line.lower() for word in ("vulkan", "swiftshader", "dawn", "gpu"))][-10:]
    return result


async def app(scope, receive, send):
    markup = '<iframe src="/frame" style="opacity:0"></iframe>' if scope["path"] == "/" else "<canvas></canvas>"
    await HTMLResponse(markup, headers=profiler.SECURITY_HEADERS)(scope, receive, send)


async def main():
    temp_root = Path(os.environ.get("RUNNER_TEMP") or os.environ["TEMP"]).resolve()
    with TemporaryDirectory(dir=temp_root, prefix="zed-gpu-") as directory:
        scratch = Path(directory).resolve()
        assert scratch.parent == temp_root
        cert, key, fingerprint = profiler.loopback_certificate(scratch)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen()
            origin = f"https://127.0.0.1:{listener.getsockname()[1]}"
            server = uvicorn.Server(uvicorn.Config(app, lifespan="off", log_level="error",
                                                   ssl_certfile=str(cert), ssl_keyfile=str(key)))
            serving = asyncio.create_task(server.serve(sockets=[listener]))
            try:
                async with asyncio.timeout(8):
                    while not server.started:
                        await asyncio.sleep(0.1)
                edge = Path(os.environ["ProgramFiles(x86)"]) / "Microsoft/Edge/Application/msedge.exe"
                chrome = Path(os.environ["ProgramFiles"]) / "Google/Chrome/Application/chrome.exe"
                software = ["--use-webgpu-adapter=swiftshader", "--enable-unsafe-swiftshader"]
                variants = [
                    ("edge-swiftshader", edge, software),
                    ("edge-default", edge, []),
                    ("chrome-vulkan", chrome, [*software, "--use-vulkan=swiftshader", "--disable-vulkan-surface"]),
                ]
                if browser := os.environ.get("ZED_PROBE_EDGE"):
                    variants.append(("edge-pinned-swiftshader", Path(browser), software))
                for label, browser, flags in variants:
                    location = scratch / label
                    location.mkdir()
                    try:
                        result = await probe(origin, location, fingerprint, browser, flags)
                    except (OSError, TimeoutError, RuntimeError, KeyError) as error:
                        result = {"error": f"{type(error).__name__}: {error}"}
                    print(json.dumps({"variant": label, **result}), flush=True)
            finally:
                server.should_exit = True
                await serving


if __name__ == "__main__":
    asyncio.run(main())
