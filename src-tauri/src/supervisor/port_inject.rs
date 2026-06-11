//! Best-effort port forcing for non-flutter commands.
//!
//! The supervisor already substitutes `{PORT}` and sets the `PORT` env var, but
//! a dev server that hardcodes its port in a config file (e.g. a vite/angular
//! config pinned to 8080) ignores both. The reliable override is the framework's
//! own CLI port flag - a flag beats a config file - so when a command opts into a
//! dynamic port but does not itself express one, we append the right flag for the
//! framework we can recognize. Anything we can't recognize is left untouched: the
//! `PORT` env var is still set, and `ports_detect` reports whatever it actually
//! bound, so visibility never depends on this succeeding.

use crate::types::ProcKind;

/// Produce the command string to spawn for a dynamic port `p`.
///
/// Substitutes any `{PORT}` placeholder. If the author already expressed the
/// port (placeholder or an explicit flag), we respect it verbatim. Otherwise,
/// for a recognized framework, we append its native port flag. Flutter is driven
/// by its own machinery (proxy + `{PORT}` convention) and is never rewritten here.
pub fn resolve_port(cmd: &str, kind: &ProcKind, p: u16) -> String {
    let substituted = cmd.replace("{PORT}", &p.to_string());
    if cmd.contains("{PORT}") || matches!(kind, ProcKind::Flutter) || has_port_flag(cmd) {
        return substituted;
    }
    match framework_flag(cmd) {
        Some(flag) => format!("{substituted} {flag} {p}"),
        None => substituted,
    }
}

/// True when the command already carries an explicit port flag we'd otherwise
/// add (so we don't double-specify and fight the author's intent).
fn has_port_flag(cmd: &str) -> bool {
    let lower = cmd.to_ascii_lowercase();
    lower.contains("--port")
        || lower.contains("--web-port")
        || lower.contains(" -p ")
        || lower.ends_with(" -p")
        || lower.contains("port=")
}

/// The CLI port flag to append for a recognized framework, or `None` if we don't
/// recognize the tool (leave it to the `PORT` env var + OS detection).
///
/// Heuristic and deliberately conservative: we only return a flag for shapes
/// where that flag is known-valid for the underlying tool, because a flag the
/// tool rejects would break startup - strictly worse than the unforced default.
fn framework_flag(cmd: &str) -> Option<&'static str> {
    let lower = cmd.to_ascii_lowercase();

    // Script-runner indirection (`npm run dev`, `pnpm dev`, `yarn dev`, ...). The
    // underlying tool receives args after `--`; the JS dev servers Joe runs
    // (vite/angular/next) all accept `--port`. This mirrors the `npm run dev --
    // --port {PORT}` form the supervised-run skill already recommends.
    if is_script_runner(&lower) {
        return Some("-- --port");
    }

    // Directly-invoked tools, each with its own native flag.
    if lower.contains("next ") {
        return Some("-p"); // next dev -p <port>
    }
    if lower.contains("vite")
        || lower.contains("ng serve")
        || lower.contains("ng ")
        || lower.contains("vue-cli-service")
        || lower.contains("nuxt")
        || lower.contains("astro")
        || lower.contains("webpack serve")
        || lower.contains("webpack-dev-server")
        || lower.contains("http-server")
    {
        return Some("--port");
    }
    None
}

/// JS package-manager script runners whose real command lives in package.json,
/// so we can only reach it by forwarding args past `--`.
fn is_script_runner(lower: &str) -> bool {
    lower.starts_with("npm run")
        || lower.starts_with("npm start")
        || lower.starts_with("pnpm ")
        || lower.starts_with("yarn ")
        || lower.starts_with("bun run")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generic(cmd: &str, p: u16) -> String {
        resolve_port(cmd, &ProcKind::Generic, p)
    }

    #[test]
    fn placeholder_is_substituted_and_respected() {
        // Author expressed the port via {PORT}: substitute, never inject.
        assert_eq!(generic("vite --port {PORT}", 42013), "vite --port 42013");
        assert_eq!(
            generic("node server.js --port {PORT}", 5),
            "node server.js --port 5"
        );
    }

    #[test]
    fn explicit_port_flag_is_left_alone() {
        // A hardcoded flag (no placeholder) must not get a second one appended.
        assert_eq!(generic("vite --port 3000", 42013), "vite --port 3000");
        assert_eq!(generic("next dev -p 3000", 42013), "next dev -p 3000");
    }

    #[test]
    fn known_tools_get_their_native_flag() {
        assert_eq!(generic("vite", 42013), "vite --port 42013");
        assert_eq!(generic("ng serve", 42013), "ng serve --port 42013");
        assert_eq!(generic("next dev", 42013), "next dev -p 42013");
        assert_eq!(generic("astro dev", 42013), "astro dev --port 42013");
    }

    #[test]
    fn script_runners_forward_past_double_dash() {
        assert_eq!(
            generic("npm run dev", 42013),
            "npm run dev -- --port 42013"
        );
        assert_eq!(generic("pnpm dev", 42013), "pnpm dev -- --port 42013");
        assert_eq!(generic("yarn start", 42013), "yarn start -- --port 42013");
    }

    #[test]
    fn unrecognized_command_is_untouched() {
        // No recognized tool: rely on the PORT env var + OS detection. We must
        // not append a flag a mystery binary might reject and fail to start on.
        assert_eq!(
            generic("./my-custom-server --foo", 42013),
            "./my-custom-server --foo"
        );
    }

    #[test]
    fn flutter_is_never_rewritten_here() {
        // Flutter's port is handled by its own proxy/{PORT} path; this fn leaves
        // a flutter command alone apart from substituting an existing placeholder.
        assert_eq!(
            resolve_port("flutter run -d web-server", &ProcKind::Flutter, 42013),
            "flutter run -d web-server"
        );
        assert_eq!(
            resolve_port("flutter run -d web-server --web-port {PORT}", &ProcKind::Flutter, 42013),
            "flutter run -d web-server --web-port 42013"
        );
    }
}
