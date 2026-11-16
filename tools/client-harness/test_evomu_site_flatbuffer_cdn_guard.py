# -*- coding: utf-8 -*-
"""Unit tests for `_install_flatbuffer_cdn_http_guard` in `evomu_site.py` (`#1128`)."""

import builtins
import http.client
from http.server import BaseHTTPRequestHandler, HTTPServer
import os
import sys
import threading
import types
import unittest
import zlib as real_zlib

sys.modules.setdefault("__builtin__", builtins)
HARNESS_DIR = os.path.dirname(os.path.abspath(__file__))
if HARNESS_DIR not in sys.path:
    sys.path.insert(0, HARNESS_DIR)

import evomu_site  # noqa: E402


def _make_high_bit_crc_payload():
    """Return a binary payload whose IEEE CRC32 has MSB=1 (`>= 0x80000000`)."""
    for i in range(64):
        candidate = ("EVOMU_TITL_HIGH_CRC_%d" % i).encode("ascii")
        crc = real_zlib.crc32(candidate) & 0xFFFFFFFF
        if crc >= 0x80000000:
            return candidate, crc
    raise AssertionError("failed to find payload with MSB=1 CRC32")


class _Py2SignedZlib(object):
    """Emulate Python 2.7's signed 32-bit `zlib.crc32` return values."""

    def crc32(self, data, value=0):
        raw = real_zlib.crc32(data, value) & 0xFFFFFFFF
        if raw >= 0x80000000:
            return raw - 0x100000000
        return raw


def _build_fake_provider_module(https_calls):
    mod = types.ModuleType("content.client.flatbuffer.provider")
    mod.zlib = _Py2SignedZlib()

    def separate_host_and_request(url):
        split = url.split("/")
        host = split[2]
        req = ""
        for seg in split[3:]:
            req += "/" + seg
        return host, req

    mod.separate_host_and_request = separate_host_and_request

    class FlatbufferContentProvider(object):
        def retrieve_data_from_cdn(self, url, crc, timeout_seconds=10):
            host, request = mod.separate_host_and_request(url)
            data = self._send_http_request(host, request, timeout_seconds)
            if crc is not None and crc > 0:
                computed = mod.zlib.crc32(data)
                if computed != crc:
                    raise AssertionError(
                        "CRC mismatch: expected %s, got %s" % (crc, computed)
                    )
            return data

        def _send_http_request(self, host, request, timeout_seconds=10):
            https_calls.append((host, request, timeout_seconds))
            return b"FROM_HTTPS_DEFAULT_443"

    mod.FlatbufferContentProvider = FlatbufferContentProvider
    return mod


class FlatbufferCdnHttpGuardTest(unittest.TestCase):
    def setUp(self):
        self._orig_hooked = evomu_site._HOOKED_FLATBUFFER_CDN
        self._orig_mod = sys.modules.get("content.client.flatbuffer.provider")
        self._https_calls = []
        self._provider_mod = _build_fake_provider_module(self._https_calls)
        sys.modules["content.client.flatbuffer.provider"] = self._provider_mod
        evomu_site._HOOKED_FLATBUFFER_CDN = False

    def tearDown(self):
        evomu_site._HOOKED_FLATBUFFER_CDN = self._orig_hooked
        if self._orig_mod is None:
            sys.modules.pop("content.client.flatbuffer.provider", None)
        else:
            sys.modules["content.client.flatbuffer.provider"] = self._orig_mod

    def test_http_port_preserved_status_checked_and_py2_crc_unsigned(self):
        payload, expected_crc = _make_high_bit_crc_payload()
        self.assertLess(self._provider_mod.zlib.crc32(payload), 0)

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path == "/cdn/cosmetics/character/title/definitions/v24.1.0/en-us.fb":
                    self.send_response(200)
                    self.send_header("Content-Type", "application/octet-stream")
                    self.send_header("Content-Length", str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                else:
                    self.send_response(404)
                    self.end_headers()
                    self.wfile.write(b"not found")

            def log_message(self, fmt, *args):
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        port = server.server_address[1]
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            evomu_site._install_flatbuffer_cdn_http_guard()
            self.assertTrue(evomu_site._HOOKED_FLATBUFFER_CDN)
            wrapped_send = self._provider_mod.FlatbufferContentProvider._send_http_request
            wrapped_zlib = self._provider_mod.zlib

            # Idempotency: second call and _EvomuMetaHook keep single wrapper layer.
            evomu_site._install_flatbuffer_cdn_http_guard()
            self.assertIs(
                self._provider_mod.FlatbufferContentProvider._send_http_request,
                wrapped_send,
            )
            self.assertIs(self._provider_mod.zlib, wrapped_zlib)

            provider = self._provider_mod.FlatbufferContentProvider()
            url = (
                "http://127.0.0.1:%d/cdn/cosmetics/character/title/definitions/v24.1.0/en-us.fb"
                % port
            )
            got = provider.retrieve_data_from_cdn(url, expected_crc, timeout_seconds=5)
            self.assertEqual(got, payload)
            self.assertEqual(self._https_calls, [])

            # Non-200 HTTP response must raise HTTPException instead of returning error HTML/JSON.
            bad_url = "http://127.0.0.1:%d/cdn/missing.fb" % port
            with self.assertRaises(http.client.HTTPException):
                provider.retrieve_data_from_cdn(bad_url, None, timeout_seconds=5)

            # Default-port https:// continues delegating to the original TLS transport.
            https_data = provider.retrieve_data_from_cdn(
                "https://cdn.example.com/cdn/ok.fb", None, timeout_seconds=5
            )
            self.assertEqual(https_data, b"FROM_HTTPS_DEFAULT_443")
            self.assertEqual(self._https_calls, [("cdn.example.com", "/cdn/ok.fb", 5)])
        finally:
            server.shutdown()
            server.server_close()

    def test_explicit_https_port_does_not_force_port_443(self):
        evomu_site._install_flatbuffer_cdn_http_guard()
        provider = self._provider_mod.FlatbufferContentProvider()

        recorded = []

        class FakeHttpsConn(object):
            def __init__(self, netloc, timeout=None):
                recorded.append(("init", netloc, timeout))

            def putrequest(self, method, path):
                recorded.append(("req", method, path))

            def endheaders(self):
                pass

            def getresponse(self):
                resp = types.SimpleNamespace(status=200, reason="OK")
                resp.read = lambda: b"HTTPS_CUSTOM_PORT_OK"
                return resp

            def close(self):
                recorded.append(("close",))

        orig_https = http.client.HTTPSConnection
        http.client.HTTPSConnection = FakeHttpsConn
        try:
            got = provider.retrieve_data_from_cdn(
                "https://cdn.example.com:8443/cdn/universe.fb", None, timeout_seconds=7
            )
            self.assertEqual(got, b"HTTPS_CUSTOM_PORT_OK")
            self.assertEqual(self._https_calls, [])
            self.assertEqual(
                recorded,
                [
                    ("init", "cdn.example.com:8443", 7),
                    ("req", "GET", "/cdn/universe.fb"),
                    ("close",),
                ],
            )
        finally:
            http.client.HTTPSConnection = orig_https


if __name__ == "__main__":
    unittest.main()
