import asyncio
from pathlib import Path
import tempfile
import unittest

from gateway import SECURITY_HEADERS, SESSION_COOKIE, app_for


class GatewayTest(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        web = root / "web"
        web.mkdir()
        (web / "index.html").write_text("<html></html>", encoding="utf-8")
        config = web / ".." / "config"
        config.mkdir()
        (config / "settings.json").write_text('{"theme":"Example"}', encoding="utf-8")
        self.app = app_for(
            "workspace.test", web, root, root / "remote-server.exe", root / "state",
            "test-token", "test-session", global_config=config,
        )

    async def request(self, host, path="/", headers=()):
        messages = []
        request_sent = False
        response_completed = asyncio.Event()

        async def receive():
            nonlocal request_sent
            if not request_sent:
                request_sent = True
                return {"type": "http.request", "body": b"", "more_body": False}
            await response_completed.wait()
            return {"type": "http.disconnect"}

        async def send(message):
            messages.append(message)
            if message["type"] == "http.response.body" and not message.get("more_body", False):
                response_completed.set()

        scope = {
            "type": "http",
            "asgi": {"version": "3.0", "spec_version": "2.4"},
            "http_version": "1.1",
            "method": "GET",
            "scheme": "http",
            "path": path,
            "root_path": "",
            "query_string": b"",
            "headers": [(b"host", host.encode("ascii")), *headers],
        }
        await asyncio.wait_for(self.app(scope, receive, send), timeout=5)
        return next(message for message in messages if message["type"] == "http.response.start")

    async def test_expected_host_keeps_security_headers(self):
        response = await self.request("workspace.test")
        self.assertEqual(response["status"], 303)
        headers = dict(response["headers"])
        self.assertEqual(headers[b"location"], b"/workspace")
        for name, value in SECURITY_HEADERS.items():
            self.assertEqual(headers[name.lower().encode("ascii")], value.encode("ascii"))

    async def test_unexpected_host_is_rejected(self):
        response = await self.request("other.test")
        self.assertEqual(response["status"], 400)

    async def test_startup_reads_configuration_through_a_noncanonical_path(self):
        cookie = (b"cookie", f"{SESSION_COOKIE}=test-session".encode("ascii"))
        async with self.app.router.lifespan_context(self.app):
            response = await self.request("workspace.test", "/global-config", (cookie,))
        self.assertEqual(response["status"], 200)
        self.assertEqual(dict(response["headers"])[b"content-type"], b"application/json")
