//! Claude Code settings passed to every agent with `--settings`, so its hooks report status through `ftm status`
//! without touching the user's own settings. The hooks are additive to the user's, never block, and always exit 0.

use eyre::{Result, WrapErr};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// (hook event, matcher, status). Waiting on the user means a permission decision (`PermissionRequest` fires at once;
/// the `permission_prompt` notification only after ~6s), an `AskUserQuestion`, or an MCP elicitation; the 60s
/// `idle_prompt` notification is left out so a finished agent never turns red. `PostToolUse` rather than `PreToolUse`
/// marks work, so it cannot race the `AskUserQuestion` hook and it clears needs_input once a prompt is answered.
/// `SessionEnd` is best effort: a killed session may never send it. `Stop` passes its input on (`--hook`) for the
/// background subagents still running.
const HOOKS: &[(&str, Option<&str>, &str)] = &[
    ("UserPromptSubmit", None, "working"),
    ("PostToolUse", None, "working"),
    ("PermissionRequest", None, "needs-input"),
    ("PreToolUse", Some("AskUserQuestion"), "needs-input"),
    (
        "Notification",
        Some("permission_prompt|elicitation_dialog|elicitation_url_dialog"),
        "needs-input",
    ),
    ("Stop", None, "idle --hook"),
    ("StopFailure", None, "idle"),
    ("SessionEnd", None, "done"),
];

/// supports-hyperlinks, which Claude Code uses, only emits OSC 8 links for terminals it recognizes unless
/// `FORCE_HYPERLINK` is set, and then emits them even into pipes and files. So the agent gets it through `env`
/// (which overrides any inherited value), and every SessionStart (startup, resume, clear, compact) unsets it again
/// for the Bash tool's commands through `$CLAUDE_ENV_FILE`, keeping escape codes out of their output.
const HYPERLINK_ENV: &str =
    r#"{ [ -z "$CLAUDE_ENV_FILE" ] || echo 'unset FORCE_HYPERLINK' >> "$CLAUDE_ENV_FILE"; } 2>/dev/null || true"#;

pub fn settings(ftm: &Path) -> Value {
    let ftm = shell_quote(&ftm.to_string_lossy());
    let mut hooks = serde_json::Map::new();
    for (event, matcher, status) in HOOKS {
        let mut group = json!({
            "hooks": [{
                "type": "command",
                "command": format!("{ftm} status {status} >/dev/null 2>&1 || true"),
                "timeout": 5,
            }],
        });
        if let Some(matcher) = matcher {
            group["matcher"] = json!(matcher);
        }
        hooks
            .entry(*event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("array")
            .push(group);
    }
    hooks.insert(
        "SessionStart".into(),
        json!([{ "hooks": [{ "type": "command", "command": HYPERLINK_ENV, "timeout": 5 }] }]),
    );
    json!({ "env": { "FORCE_HYPERLINK": "1" }, "hooks": hooks })
}

/// The `ftm` next to this ftmd, else whatever `ftm` is on the panes' PATH.
pub fn ftm_binary() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("ftm")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("ftm"))
}

/// Writes the settings next to `socket`, named after it so daemons sharing a dir (installed and dev) keep their own.
pub fn write(socket: &Path) -> Result<PathBuf> {
    let path = socket.with_extension("claude-settings.json");
    let body = serde_json::to_vec_pretty(&settings(&ftm_binary()))?;
    std::fs::write(&path, body).wrap_err_with(|| format!("writing {}", path.display()))?;
    Ok(path)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_call_ftm_status_and_never_fail() {
        let s = settings(Path::new("/opt/it's/ftm"));
        let cmd = |event: &str| s["hooks"][event][0]["hooks"][0]["command"].as_str().unwrap().to_string();
        assert_eq!(cmd("Stop"), r"'/opt/it'\''s/ftm' status idle --hook >/dev/null 2>&1 || true");
        assert!(cmd("Notification").contains("status needs-input"));
        assert!(
            !s["hooks"]["Notification"][0]["matcher"]
                .as_str()
                .unwrap()
                .contains("idle_prompt")
        );
        assert_eq!(s["hooks"]["PreToolUse"][0]["matcher"], "AskUserQuestion");
        assert!(s["hooks"]["Stop"][0].get("matcher").is_none());
    }

    #[test]
    fn hyperlinks_are_forced_for_the_agent_but_not_its_bash_commands() {
        let s = settings(Path::new("/opt/ftm/ftm"));
        assert_eq!(s["env"]["FORCE_HYPERLINK"], "1");
        assert!(s["hooks"]["SessionStart"][0].get("matcher").is_none());
        let hook = s["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str().unwrap();
        let run = |env_file: Option<&Path>| {
            let mut sh = std::process::Command::new("/bin/sh");
            sh.args(["-c", hook]).env_remove("CLAUDE_ENV_FILE");
            if let Some(f) = env_file {
                sh.env("CLAUDE_ENV_FILE", f);
            }
            sh.status().unwrap().success()
        };
        let dir = std::env::temp_dir().join(format!("ftmd-hooks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("env.sh");
        assert!(run(Some(&file)));
        assert!(run(Some(&file)));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "unset FORCE_HYPERLINK\nunset FORCE_HYPERLINK\n"
        );
        assert!(run(None));
        assert!(run(Some(Path::new(""))));
        assert!(run(Some(&dir.join("missing/env.sh"))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
