#!/usr/bin/env python3
"""Generate the standalone preview page for the herdr workspace.

Builds the preview FROM the real dashboard files (`crates/herdr-desktop/ui/`)
so the skeleton can never drift from what the Tauri shell serves:
stylesheet <link> tags are replaced with embedded <style> blocks and the
bundled JS (see scripts/build_ui.sh) is inlined after the `__TAURI__` shim,
together with a live fleet simulator.

Output: target/preview/index.html
"""

import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent
UI = ROOT / "crates/herdr-desktop/ui"
HARNESS_PATH = ROOT / "scripts/preview_harness.js"
BUNDLE_PATH = UI / "dist/bundle.js"
OUT = ROOT / "target/preview/index.html"

BANNER = """<div class="preview-banner">
      static preview of <code>crates/herdr-desktop/ui</code> — simulated fleet;
      the real app is the Tauri shell (<code>cargo run -p herdr-desktop --features tauri</code>)
      — rebuild the bundle first with <code>sh scripts/build_ui.sh</code>
    </div>"""


def main() -> None:
    html = (UI / "index.html").read_text()
    harness = HARNESS_PATH.read_text()
    bundle = BUNDLE_PATH.read_text()

    # Stylesheets: <link rel="stylesheet" href="styles/x.css" /> → <style>.
    def inline_css(match: re.Match) -> str:
        css_file = UI / match.group(1)
        css = css_file.read_text()
        return f"<style>\n/* inlined from {match.group(1)} */\n{css}\n    </style>"

    html, n_css = re.subn(
        r'<link rel="stylesheet" href="([^"]+)"\s*/>', inline_css, html
    )
    assert n_css >= 1, "index.html has no stylesheet links to inline"

    # Bundle: external module script → harness shim + inlined bundle module.
    bundle_tag = '<script type="module" src="dist/bundle.js"></script>'
    assert bundle_tag in html, "index.html must load dist/bundle.js as a module"
    html = html.replace(
        bundle_tag,
        "<script>\n" + harness + "\n    </script>\n"
        '    <script type="module">\n// ----- bundled dashboard (ui/dist/bundle.js) —\n'
        "// loaded as a module because it uses top-level await, exactly like in Tauri.\n"
        + bundle
        + "\n    </script>",
    )

    # Preview banner for context.
    assert "<body>" in html
    html = html.replace("<body>", "<body>\n    " + BANNER, 1)

    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(html)
    print(f"wrote {OUT} ({len(html)} bytes)")


if __name__ == "__main__":
    main()
