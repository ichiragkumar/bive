//! Regression test: the desktop dashboard must never render a blank window.
//!
//! The dashboard UI is modular source (`crates/herdr-desktop/ui/`, bundled by
//! `scripts/build_ui.sh` into `ui/dist/bundle.js`) shown either in the Tauri
//! shell or via the generated static preview (`scripts/make_preview.py` →
//! `target/preview/index.html`, built FROM the real files so the skeleton
//! cannot drift). A blank window happens when the skeleton loses mount
//! points, the JS wiring drifts from the Rust command/event surface, or the
//! generator drops a part — so this test parses and validates the skeleton,
//! the bundle wiring, the stylesheets, the preview-harness contract, and the
//! generated output itself. No browser needed.

use std::path::PathBuf;

/// Every DOM mount point the dashboard JS needs. If any is missing, lookups
/// return null and the surface renders blank.
const REQUIRED_IDS: &[&str] = &[
    "topbar",
    "btn-side",
    "conn-dot",
    "fleet-dot",
    "fleet-summary",
    "btn-kill-all",
    "layout",
    "sidebar",
    "host-list",
    "btn-add-remote",
    "profile-picker",
    "btn-spawn",
    "filter",
    "agent-list-pane",
    "agent-list",
    "detail-pane",
    "detail-header",
    "detail-title",
    "tab-chat",
    "tab-terminal",
    "tab-info",
    "chat-media",
    "chat-turns",
    "detail-log",
    "detail-info-list",
    "composer-bar",
    "composer",
    "composer-send",
    "detail-copy",
    "detail-kill",
    "fatal",
];

/// Void elements never need a closing tag.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

fn workspace_root() -> PathBuf {
    // This file lives in crates/herdr-desktop/tests/.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn assert_contains(hay: &str, needle: &str, what: &str) {
    assert!(
        hay.contains(needle),
        "{what}: expected to contain {needle:?}"
    );
}

/// Remove `<tag …>…</tag>` blocks so tag balancing only sees real markup
/// (bundled JS contains `<` in comparisons and template HTML).
fn strip_element_blocks(html: &str, tag: &str) -> String {
    let open_pat = format!("<{tag}");
    let close_pat = format!("</{tag}>");
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find(open_pat.as_str()) {
        out.push_str(&rest[..start]);
        let after_open = rest[start..]
            .find('>')
            .unwrap_or_else(|| panic!("unclosed <{tag}>"))
            + start
            + 1;
        let close = rest[after_open..]
            .find(close_pat.as_str())
            .unwrap_or_else(|| panic!("unclosed {tag} block"))
            + after_open;
        rest = &rest[close + close_pat.len()..];
    }
    out.push_str(rest);
    out
}

/// Minimal tag-balance parser: fails on stray closers, mismatched nesting, or
/// unclosed tags — the malformed-markup class of blank-window regressions.
/// Skips comments, `<!…>` declarations, and void/self-closing elements.
fn check_tag_balance(html: &str, what: &str) {
    let mut stack: Vec<String> = Vec::new();
    let mut i = 0;
    while i < html.len() {
        // Byte stepping can land inside a multi-byte char; resync first.
        if !html.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if !html[i..].starts_with('<') {
            i += 1;
            continue;
        }
        if html[i..].starts_with("<!--") {
            let end = html[i..]
                .find("-->")
                .unwrap_or_else(|| panic!("{what}: unclosed comment"));
            i += end + 3;
            continue;
        }
        if html[i..].starts_with("<!") {
            let end = html[i..]
                .find('>')
                .unwrap_or_else(|| panic!("{what}: unclosed <!…>"));
            i += end + 1;
            continue;
        }
        let rest = &html[i..];
        let end = rest
            .find('>')
            .unwrap_or_else(|| panic!("{what}: unclosed tag near {rest:.40}"));
        let inner = rest[1..end].trim();
        if let Some(name) = inner.strip_prefix('/') {
            let name = name.split_whitespace().next().unwrap_or("").to_lowercase();
            let open = stack
                .pop()
                .unwrap_or_else(|| panic!("{what}: stray </{name}>"));
            assert_eq!(open, name, "{what}: <{open}> closed by </{name}>");
        } else {
            let name = inner
                .split([' ', '\t', '\n', '\r', '/'])
                .next()
                .unwrap_or("")
                .to_lowercase();
            let self_close = inner.ends_with('/') || VOID_ELEMENTS.contains(&name.as_str());
            if !name.is_empty() && !self_close {
                stack.push(name);
            }
        }
        i += end + 1;
    }
    assert!(
        stack.is_empty(),
        "{what}: unclosed tags left on the stack: {stack:?}"
    );
}

fn check_mount_points(html: &str, what: &str) {
    for id in REQUIRED_IDS {
        assert_contains(html, &format!("id=\"{id}\""), what);
    }
}

#[test]
fn tag_balance_checker_rejects_broken_markup() {
    // The validator itself must not be vacuous: well-formed markup passes,
    // each broken shape panics.
    check_tag_balance("<div><span></span></div>", "ok-fixture");
    for bad in [
        "<div><span></div>", // mismatched nesting
        "<div>",             // unclosed tag
        "</span>",           // stray closer
        "<div><!-- oops",    // unclosed comment
    ] {
        assert!(
            std::panic::catch_unwind(|| check_tag_balance(bad, "bad-fixture")).is_err(),
            "balance checker accepted broken markup: {bad:?}"
        );
    }
}

#[test]
fn dashboard_skeleton_has_all_mount_points() {
    let html = read("crates/herdr-desktop/ui/index.html");
    assert!(
        html.starts_with("<!doctype html>"),
        "skeleton: missing doctype"
    );
    check_mount_points(&html, "ui/index.html");

    // Wiring: modular stylesheets + the esbuild bundle entry.
    assert_contains(&html, "styles/tokens.css", "ui/index.html");
    assert_contains(
        &html,
        "<script type=\"module\" src=\"dist/bundle.js\">",
        "ui/index.html",
    );

    // Spawn profile picker must offer every daemon profile.
    for opt in [
        "value=\"generic\"",
        "value=\"claude-code\"",
        "value=\"codex\"",
        "value=\"bash\"",
    ] {
        assert_contains(&html, opt, "ui/index.html profile picker");
    }
    // State filter options the list compares against.
    for opt in [
        "value=\"all\"",
        "value=\"Working\"",
        "value=\"Blocked\"",
        "value=\"Idle\"",
        "value=\"Errored\"",
    ] {
        assert_contains(&html, opt, "ui/index.html filter");
    }
    // Detail tabs.
    for tab in [
        "data-tab=\"chat\"",
        "data-tab=\"terminal\"",
        "data-tab=\"info\"",
    ] {
        assert_contains(&html, tab, "ui/index.html tabs");
    }

    // Only external references here, so balance the whole document directly.
    check_tag_balance(&html, "ui/index.html");
}

#[test]
fn dashboard_bundle_matches_tauri_surface() {
    let js = read("crates/herdr-desktop/ui/dist/bundle.js");
    assert!(js.len() > 10_000, "bundle looks truncated");

    // Entry guard: outside Tauri/preview this renders a notice, never blank.
    assert_contains(&js, "missing window.__TAURI__", "bundle.js");

    // Event subscriptions: the only update path (no polling).
    for channel in ["herdr://event", "herdr://conn"] {
        assert_contains(&js, channel, "bundle.js");
    }
    // Commands the Rust shell exposes (see `app.rs` invoke_handler),
    // including the sidebar's remote commands and host-aware spawn.
    for cmd in [
        "snapshot_cmd",
        "spawn_agent_cmd",
        "kill_all_cmd",
        "kill_agent_cmd",
        "send_input_cmd",
        "remote_list_cmd",
        "remote_add_cmd",
        "remote_remove_cmd",
    ] {
        assert_contains(&js, cmd, "bundle.js");
    }
    // Render surfaces: one module each, all present in the bundle.
    for symbol in [
        "__HERDR_APP__",
        "agent-list",
        "host-list",
        "profile-picker",
        "chat-turns",
        "detail-info-list",
        "noteSentLocal",
    ] {
        assert_contains(&js, symbol, "bundle.js");
    }
    // Every daemon event kind must be handled or fleet state goes stale.
    for kind in [
        "AgentSpawned",
        "AgentOutput",
        "AgentMedia",
        "StateChange",
        "AgentExited",
        "AgentRemoved",
    ] {
        assert_contains(&js, kind, "bundle.js");
    }
    // Inlining the preview must not break out of its <script> block.
    assert!(
        !js.contains("</script>"),
        "bundle.js must not contain </script>"
    );
}

#[test]
fn dashboard_css_covers_rendered_surfaces() {
    let root = workspace_root();
    let dir = root.join("crates/herdr-desktop/ui/styles");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert!(
        names.contains(&"tokens.css".to_string()),
        "styles/: tokens.css missing, got {names:?}"
    );
    let css: String = names
        .iter()
        .filter(|n| n.ends_with(".css"))
        .map(|n| read(&format!("crates/herdr-desktop/ui/styles/{n}")))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(css.len() > 1024, "styles/ look truncated");
    // Every selector the skeleton and rendered surfaces depend on.
    for sel in [
        ":root",
        "#topbar",
        "#layout",
        "#sidebar",
        "#host-list",
        "#agent-list",
        ".agent-row",
        ".badge",
        ".dot",
        "#detail-pane",
        ".tab",
        ".turn",
        ".log",
        "#detail-info-list",
        "#composer-bar",
        ".fatal",
        ".muted",
    ] {
        assert_contains(&css, sel, "styles/");
    }
}

#[test]
fn preview_harness_matches_dashboard_contract() {
    let harness = read("scripts/preview_harness.js");

    // The shim must provide the exact Tauri surface main.js imports…
    for symbol in ["window.__TAURI__", "invoke", "listen"] {
        assert_contains(&harness, symbol, "preview_harness.js");
    }
    // …handle every command the dashboard invokes…
    for cmd in [
        "snapshot_cmd",
        "spawn_agent_cmd",
        "kill_all_cmd",
        "kill_agent_cmd",
        "send_input_cmd",
        "remote_list_cmd",
        "remote_add_cmd",
        "remote_remove_cmd",
    ] {
        assert_contains(&harness, cmd, "preview_harness.js");
    }
    // …serve snapshot chat turns and remote hosts…
    for symbol in ["snapChat", "registryHosts", "chat"] {
        assert_contains(&harness, symbol, "preview_harness.js");
    }
    // …and emit the same channels/events the Rust shell forwards.
    for symbol in [
        "herdr://event",
        "herdr://conn",
        "AgentSpawned",
        "AgentOutput",
        "StateChange",
        "AgentMedia",
        "AgentExited",
        "AgentRemoved",
    ] {
        assert_contains(&harness, symbol, "preview_harness.js");
    }
    assert!(
        !harness.contains("</script>"),
        "preview_harness.js must not contain </script>"
    );

    // The generator builds FROM the real files so the skeleton cannot drift.
    let gen = read("scripts/make_preview.py");
    for symbol in [
        "crates/herdr-desktop/ui",
        "index.html",
        "dist/bundle.js",
        "preview_harness.js",
        "target/preview/index.html",
        "preview-banner",
        "stylesheet",
    ] {
        assert_contains(&gen, symbol, "make_preview.py");
    }
}

#[test]
fn generated_preview_renders_fleet_ui() {
    let root = workspace_root();
    let out = root.join("target/preview/index.html");

    // Regenerate with the real generator so this validates what ships.
    let status = std::process::Command::new("python3")
        .arg(root.join("scripts/make_preview.py"))
        .status()
        .expect("python3 must be installed to build the preview");
    assert!(status.success(), "make_preview.py failed");

    let html =
        std::fs::read_to_string(&out).unwrap_or_else(|e| panic!("reading {}: {e}", out.display()));
    assert!(
        html.len() > 30_000,
        "generated preview looks truncated ({} bytes)",
        html.len()
    );

    // External references must all be inlined (single snapshot file).
    assert!(
        !html.contains("src=\"dist/bundle.js\""),
        "generated preview still references the external bundle"
    );
    assert!(
        !html.contains("rel=\"stylesheet\""),
        "generated preview still references external stylesheets"
    );

    // The full dashboard skeleton survives generation…
    check_mount_points(&html, "generated preview");
    assert_contains(&html, "preview-banner", "generated preview");

    // …with all three parts embedded: stylesheets, harness shim, bundle.
    assert_contains(&html, "--bg:", "generated preview (embedded CSS)");
    assert_contains(
        &html,
        "window.__TAURI__",
        "generated preview (harness shim)",
    );
    assert_contains(&html, "__HERDR_APP__", "generated preview (dashboard code)");

    // Structural soundness outside script/style blocks (script bodies contain
    // `<` in JS comparisons and template HTML, so strip them first).
    let markup = strip_element_blocks(&html, "script");
    let markup = strip_element_blocks(&markup, "style");
    check_tag_balance(&markup, "generated preview");
}
