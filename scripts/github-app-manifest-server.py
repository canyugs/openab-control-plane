#!/usr/bin/env python3
"""Serve a GitHub App manifest registration page and capture the manifest callback code.

Used by scripts/register-github-app.sh. Requires Python 3 only for this helper —
the manual UI path in docs/install-github-app.md needs no Python.

Environment:
  REDIRECT_BASE   Public URL prefix (required), e.g. https://abc.trycloudflare.com
  PLANE_URL       Control plane base URL for webhook + manifest url field
  APP_NAME        GitHub App display name
  GITHUB_ORG      If set, register under organizations/<org>/settings/apps/new
  PORT            Listen port (default 8795)
  CODE_FILE       Where to write the temporary manifest code (default /tmp/github-app-manifest-code.txt)
"""
from __future__ import annotations

import json
import os
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(os.environ.get("PORT", "8795"))
ORG = os.environ.get("GITHUB_ORG", "").strip()
APP_NAME = os.environ.get("APP_NAME", "OpenAB Council")
PLANE_URL = os.environ.get("PLANE_URL", "").rstrip("/")
CODE_FILE = os.environ.get("CODE_FILE", "/tmp/github-app-manifest-code.txt")
REDIRECT_BASE = os.environ.get("REDIRECT_BASE", "").rstrip("/")

if not REDIRECT_BASE:
    raise SystemExit("REDIRECT_BASE env required")
if not PLANE_URL:
    raise SystemExit("PLANE_URL env required")

WEBHOOK_URL = f"{PLANE_URL}/api/v1/github_webhooks"
GITHUB_POST = (
    f"https://github.com/organizations/{ORG}/settings/apps/new"
    if ORG
    else "https://github.com/settings/apps/new"
)

MANIFEST = {
    "name": APP_NAME,
    "url": PLANE_URL,
    "description": "Self-hosted PR review council powered by OpenAB Control Plane",
    "public": False,
    "hook_attributes": {
        "url": WEBHOOK_URL,
        "active": True,
    },
    "redirect_url": f"{REDIRECT_BASE}/callback",
    "default_permissions": {
        "pull_requests": "write",
        "contents": "read",
        "statuses": "write",
        "issues": "write",
    },
    "default_events": ["pull_request", "issue_comment"],
}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        print(f"[manifest] {fmt % args}")

    def do_GET(self):
        if self.path.startswith("/callback"):
            qs = urllib.parse.urlparse(self.path).query
            params = urllib.parse.parse_qs(qs)
            code = (params.get("code") or [""])[0]
            if not code:
                self.send_response(400)
                self.end_headers()
                self.wfile.write(b"missing code")
                return
            with open(CODE_FILE, "w", encoding="utf-8") as f:
                f.write(code)
            print(f"[manifest] captured code: {code[:16]}...")
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(
                (
                    f"<html><body><h1>{APP_NAME} registered</h1>"
                    "<p>Return to your terminal. The register script will continue automatically.</p>"
                    "</body></html>"
                ).encode()
            )
            return

        manifest_json = json.dumps(MANIFEST)
        owner = f"organization <strong>{ORG}</strong>" if ORG else "your personal account"
        html = f"""<!doctype html>
<html><body>
<h1>Register {APP_NAME}</h1>
<p>Owner: {owner}</p>
<p>Webhook: {WEBHOOK_URL}</p>
<form action="{GITHUB_POST}" method="post">
  <input type="hidden" name="manifest" value='{manifest_json}' />
  <button type="submit">Create {APP_NAME} GitHub App</button>
</form>
</body></html>"""
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.end_headers()
        self.wfile.write(html.encode())

    def do_POST(self):
        self.do_GET()


if __name__ == "__main__":
    target = f"organization {ORG}" if ORG else "personal account"
    print(f"target={target} listen=127.0.0.1:{PORT} redirect={REDIRECT_BASE}/callback")
    print(f"webhook={WEBHOOK_URL}")
    HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()