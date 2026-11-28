#!/usr/bin/env python3
"""Local login pages for the browser-login benchmark. Standard library only.

    python3 -I bench/web-login/server.py [--port 8765]

Serves three variants of one login form. Each takes `redirect_uri` and `state`
in the query, like an OAuth authorize endpoint, and after a correct login
answers 302 to `<redirect_uri>?email=...&state=...&variant=...`.

    /plain/login       email and password on one page
    /two-step/login    email first, password on the next page
    /controlled/login  inputs mirrored into JavaScript state on `input` events,
                       submit disabled until the state is valid

The sample app opens these pages in a Chrome Custom Tab (Android, through
`adb reverse tcp:<port> tcp:<port>`) or an ASWebAuthenticationSession (iOS
Simulator, which shares the host's loopback). The server binds 127.0.0.1 only
and redirects only to the `--redirect-scheme` scheme.
"""
import argparse
import html
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlencode, urlsplit

# The one account. The flows under examples/sample_app/.maestro type these.
EMAIL = "qa.pilot@tapwing.dev"
PASSWORD = "Tw!ng-Pass2026"

PAGES = Path(__file__).resolve().parent / "pages"
VARIANTS = ("plain", "two-step", "controlled")
EVENT_NAMES = ("keydown", "input", "change")


def render(name, **values):
    """A page with every `{{key}}` replaced by the HTML-escaped value."""
    text = (PAGES / name).read_text(encoding="utf-8")
    for key, value in values.items():
        text = text.replace("{{" + key + "}}", html.escape(value, quote=True))
    return text


def error_block(message):
    return f'<p class="error" role="alert">{html.escape(message)}</p>' if message else ""


class Handler(BaseHTTPRequestHandler):
    server_version = "TapwingWebLogin/1"
    redirect_scheme = "tapwingsample"

    def log_message(self, fmt, *args):
        print(f"{self.log_date_time_string()} {fmt % args}", flush=True)

    def send_page(self, body, status=200, content_type="text/html; charset=utf-8"):
        data = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def send_redirect(self, location):
        self.send_response(302)
        self.send_header("Location", location)
        self.send_header("Content-Length", "0")
        self.send_header("Cache-Control", "no-store")
        self.end_headers()

    def session(self, params):
        """`redirect_uri` and `state` of a request, or None after answering 400."""
        redirect_uri = params.get("redirect_uri", [""])[0]
        state = params.get("state", [""])[0]
        if urlsplit(redirect_uri).scheme != self.redirect_scheme:
            self.send_page(
                f"redirect_uri must use the {self.redirect_scheme}:// scheme\n", 400, "text/plain; charset=utf-8"
            )
            return None
        return {"redirect_uri": redirect_uri, "state": state}

    def do_GET(self):
        url = urlsplit(self.path)
        params = parse_qs(url.query)
        if url.path == "/healthz":
            return self.send_page("ok\n", content_type="text/plain; charset=utf-8")
        if url.path == "/style.css":
            return self.send_page((PAGES / "style.css").read_text(encoding="utf-8"), content_type="text/css")
        if url.path == "/":
            return self.send_page(render("index.html", scheme=self.redirect_scheme))
        if url.path == "/favicon.ico":
            self.send_response(204)
            return self.end_headers()
        variant, _, page = url.path.strip("/").partition("/")
        if variant in VARIANTS and page == "login":
            session = self.session(params)
            if session is None:
                return None
            name = "two-step-identifier.html" if variant == "two-step" else f"{variant}.html"
            return self.send_page(render(name, **session).replace("{{error_block}}", ""))
        if variant == "two-step" and page == "password":
            session = self.session(params)
            if session is None:
                return None
            email = params.get("email", [""])[0]
            return self.send_page(
                render("two-step-password.html", email=email, **session).replace("{{error_block}}", "")
            )
        return self.send_page("not found\n", 404, "text/plain; charset=utf-8")

    def do_POST(self):
        url = urlsplit(self.path)
        length = int(self.headers.get("Content-Length") or 0)
        form = parse_qs(self.rfile.read(length).decode("utf-8"), keep_blank_values=True)
        variant, _, page = url.path.strip("/").partition("/")
        if variant not in VARIANTS or page not in ("login", "password"):
            return self.send_page("not found\n", 404, "text/plain; charset=utf-8")
        session = self.session(form)
        if session is None:
            return None
        email = form.get("email", [""])[0]
        if variant == "two-step" and page == "login":
            # Like an identity provider, the first step accepts any identifier.
            query = urlencode({"email": email, **session})
            return self.send_redirect(f"/two-step/password?{query}")
        if page == "password" and variant != "two-step":
            return self.send_page("not found\n", 404, "text/plain; charset=utf-8")
        password = form.get("password", [""])[0]
        if variant == "controlled":
            counts = " ".join(f"{name}={form.get(name, ['?'])[0]}" for name in EVENT_NAMES)
            print(f"controlled-events {counts}", flush=True)
        if email != EMAIL or password != PASSWORD:
            # Fixture credentials, logged so a dropped or doubled character is visible.
            print(f"login-rejected variant={variant} email={email!r} password={password!r}", flush=True)
            name = "two-step-password.html" if variant == "two-step" else f"{variant}.html"
            body = render(name, email=email, **session)
            return self.send_page(body.replace("{{error_block}}", error_block("Wrong email or password.")))
        print(f"login-ok variant={variant}", flush=True)
        separator = "&" if "?" in session["redirect_uri"] else "?"
        query = urlencode({"email": email, "state": session["state"], "variant": variant})
        return self.send_redirect(f"{session['redirect_uri']}{separator}{query}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--redirect-scheme", default=Handler.redirect_scheme)
    args = parser.parse_args()
    Handler.redirect_scheme = args.redirect_scheme
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"web-login server on http://127.0.0.1:{args.port} (variants: {', '.join(VARIANTS)})", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    sys.exit(main())
