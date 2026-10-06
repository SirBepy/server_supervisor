//! Substitutes `CommandParam` placeholders into a command template at spawn
//! time, the same convention `{PORT}` already uses (see `port_inject.rs`).
//!
//! `proc/spawn.rs` runs this ahead of `resolve_port` and
//! `inject_machine_flag`, so both of those scan the fully resolved command
//! line rather than a template with `{NAME}` holes in it.

use crate::types::{CommandParam, ParamValue};

/// Single left-to-right pass over `cmd`: each `{IDENT}` token whose IDENT
/// matches a param's `name` uppercased is replaced by that param's chosen
/// flag; replaced text is never re-scanned, so a flag containing another
/// param's `{NAME}` token (or `{PORT}`) is emitted verbatim. An empty
/// `params` slice returns `cmd` byte-for-byte unchanged - not even
/// `normalize_cmd`'d - so this is a pure no-op until a command has params.
pub fn substitute_params(cmd: &str, params: &[CommandParam]) -> String {
    if params.is_empty() {
        return cmd.to_string();
    }
    let out = render(cmd, params, |p| {
        resolve_value(p, None).map(|v| v.flag.clone()).unwrap_or_default()
    });
    normalize_cmd(&out)
}

/// Like `substitute_params`, but picks an explicit `(param name, value id)`
/// override per param instead of reading `last_value`, for render-and-compare
/// matching. Falls back to `last_value` then `values.first()` for any param
/// `chosen` doesn't name.
#[allow(dead_code)]
pub(crate) fn render_with(cmd: &str, params: &[CommandParam], chosen: &[(String, String)]) -> String {
    if params.is_empty() {
        return cmd.to_string();
    }
    let out = render(cmd, params, |p| {
        let override_id = chosen
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&p.name))
            .map(|(_, value_id)| value_id.as_str());
        resolve_value(p, override_id).map(|v| v.flag.clone()).unwrap_or_default()
    });
    normalize_cmd(&out)
}

/// Chosen value for one param: an explicit `override_id` wins if it names a
/// real value, else `last_value`, else the first value, else `None` (empty
/// `values` - the placeholder vanishes to `""`, decision 4 in the design spec).
fn resolve_value<'a>(p: &'a CommandParam, override_id: Option<&str>) -> Option<&'a ParamValue> {
    override_id
        .and_then(|id| p.values.iter().find(|v| v.value == id))
        .or_else(|| p.last_value.as_deref().and_then(|lv| p.values.iter().find(|v| v.value == lv)))
        .or_else(|| p.values.first())
}

/// The shared scanner: finds each `{IDENT}` token in `cmd`, looks IDENT up
/// among `params` (case-insensitive on `name`), and calls `pick` for a match
/// to get the replacement text. An unmatched token (notably `{PORT}`, which
/// is never a param) is emitted verbatim. Continues scanning strictly after
/// each token, so a `pick` result is never itself re-scanned for `{...}`.
fn render(cmd: &str, params: &[CommandParam], pick: impl Fn(&CommandParam) -> String) -> String {
    let mut out = String::with_capacity(cmd.len());
    let mut rest = cmd;
    loop {
        let Some(start) = rest.find('{') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let after_brace = &rest[start + 1..];
        let Some(end) = after_brace.find('}') else {
            // Unterminated `{`: emit the remainder verbatim, nothing to match.
            out.push_str(&rest[start..]);
            break;
        };
        let token = &after_brace[..end];
        match params.iter().find(|p| p.name.to_uppercase() == token.to_uppercase()) {
            Some(p) => out.push_str(&pick(p)),
            None => {
                out.push('{');
                out.push_str(token);
                out.push('}');
            }
        }
        rest = &after_brace[end + 1..];
    }
    out
}

/// Collapse a command string to its canonical dedup form: trim ends and
/// reduce every run of internal whitespace to a single space. Keeps case
/// (flags are case-sensitive). Shared by `add_command`'s dedupe and
/// `substitute_params`, which relies on it to close the gap an empty flag
/// leaves behind.
pub(crate) fn normalize_cmd(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(name: &str, label: &str, values: Vec<ParamValue>, last_value: Option<&str>) -> CommandParam {
        CommandParam {
            name: name.to_string(),
            label: label.to_string(),
            values,
            last_value: last_value.map(|s| s.to_string()),
        }
    }

    fn value(id: &str, label: &str, flag: &str) -> ParamValue {
        ParamValue {
            value: id.to_string(),
            label: label.to_string(),
            flag: flag.to_string(),
        }
    }

    #[test]
    fn normalize_cmd_collapses_and_trims_whitespace() {
        assert_eq!(normalize_cmd("flutter  run"), "flutter run");
        assert_eq!(normalize_cmd("  flutter run  "), "flutter run");
        assert_eq!(normalize_cmd("npm\trun   dev"), "npm run dev");
        assert_eq!(normalize_cmd("flutter run"), "flutter run");
    }

    #[test]
    fn one_param_resolved() {
        let params = vec![param(
            "flavor",
            "Flavor",
            vec![value("dev", "Dev", "--flavor dev")],
            Some("dev"),
        )];
        assert_eq!(
            substitute_params("flutter run {FLAVOR} -t lib/main.dart", &params),
            "flutter run --flavor dev -t lib/main.dart"
        );
    }

    #[test]
    fn two_params_resolved() {
        let params = vec![
            param("device", "Device", vec![value("chrome", "Chrome", "-d chrome")], Some("chrome")),
            param(
                "flavor",
                "Flavor",
                vec![value("dev", "Dev", "--flavor dev")],
                Some("dev"),
            ),
        ];
        assert_eq!(
            substitute_params("flutter run {DEVICE} {FLAVOR}", &params),
            "flutter run -d chrome --flavor dev"
        );
    }

    #[test]
    fn empty_flag_value_drops_placeholder_and_collapses_whitespace() {
        let params = vec![param(
            "device",
            "Device",
            vec![value("default", "Default", "")],
            Some("default"),
        )];
        assert_eq!(
            substitute_params("flutter run {DEVICE} -t lib/main.dart", &params),
            "flutter run -t lib/main.dart"
        );
    }

    #[test]
    fn stale_last_value_falls_back_to_first_value() {
        let params = vec![param(
            "flavor",
            "Flavor",
            vec![value("dev", "Dev", "--flavor dev"), value("prod", "Prod", "--flavor prod")],
            Some("staging"), // no longer present among values
        )];
        assert_eq!(
            substitute_params("flutter run {FLAVOR}", &params),
            "flutter run --flavor dev"
        );
    }

    #[test]
    fn empty_values_vanishes_to_nothing() {
        let params = vec![param("flavor", "Flavor", Vec::new(), None)];
        assert_eq!(substitute_params("flutter run {FLAVOR} -t lib/main.dart", &params), "flutter run -t lib/main.dart");
    }

    #[test]
    fn empty_params_slice_leaves_cmd_byte_for_byte_unchanged() {
        // Double space proves normalize_cmd never ran: a populated params
        // slice would collapse this, so if it survives, the no-op path held.
        let cmd = "flutter  run -d chrome";
        assert_eq!(substitute_params(cmd, &[]), cmd);
    }

    #[test]
    fn single_pass_does_not_rescan_a_flag_containing_another_params_token() {
        // The "device" flag text literally contains "{FLAVOR}"; it must be
        // emitted verbatim, never resolved against the flavor param.
        let params = vec![
            param("device", "Device", vec![value("weird", "Weird", "-d {FLAVOR}")], Some("weird")),
            param("flavor", "Flavor", vec![value("dev", "Dev", "--flavor dev")], Some("dev")),
        ];
        assert_eq!(
            substitute_params("flutter run {DEVICE}", &params),
            "flutter run -d {FLAVOR}"
        );
    }

    #[test]
    fn flag_containing_port_token_survives_substitution() {
        let params = vec![param(
            "dart-define-from-file",
            "Dart define file",
            vec![value("dev", "Dev", "--dart-define=URL=http://localhost:{PORT}")],
            Some("dev"),
        )];
        assert_eq!(
            substitute_params("flutter run {DART-DEFINE-FROM-FILE} --web-port {PORT}", &params),
            "flutter run --dart-define=URL=http://localhost:{PORT} --web-port {PORT}"
        );
    }

    #[test]
    fn port_placeholder_resolves_after_substitute_params_then_resolve_port() {
        use crate::supervisor::port_inject;
        use crate::types::ProcKind;

        let params = vec![param(
            "device",
            "Device",
            vec![value("chrome", "Chrome", "-d chrome")],
            Some("chrome"),
        )];
        let template = "flutter run {DEVICE} --web-port {PORT}";
        let after_params = substitute_params(template, &params);
        assert_eq!(after_params, "flutter run -d chrome --web-port {PORT}");
        let after_port = port_inject::resolve_port(&after_params, &ProcKind::Flutter, 42013);
        assert_eq!(after_port, "flutter run -d chrome --web-port 42013");
    }

    #[test]
    fn machine_flag_lands_after_run_not_after_param_text() {
        use crate::supervisor::flutter;
        use crate::types::ProcKind;

        let params = vec![param(
            "device",
            "Device",
            vec![value("chrome", "Chrome", "-d chrome")],
            Some("chrome"),
        )];
        let template = "flutter run {DEVICE} --flavor dev";
        let after_params = substitute_params(template, &params);
        assert_eq!(after_params, "flutter run -d chrome --flavor dev");
        let after_machine = flutter::inject_machine_flag(&after_params, &ProcKind::Flutter);
        assert_eq!(after_machine, "flutter run --machine -d chrome --flavor dev");
    }
}
