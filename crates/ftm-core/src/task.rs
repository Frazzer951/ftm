//! Items an agent posts to the sidebar's Tasks section for the user: decisions it is blocked on, and flags (FYI).

use crate::model::WorkspaceId;
use serde::{Deserialize, Serialize};

/// Numbered from 1 per daemon, short enough to type (`ftm task done 7`).
pub type TaskId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// The agent is blocked until the user answers.
    Decision,
    /// For the user's information; no answer expected, though the user may reply.
    Flag,
    /// A kind from a newer ftmd. Never sent.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Open,
    /// The user answered and the reply was typed into the agent's pane.
    Answered,
    /// The user put it away without answering.
    Dismissed,
    /// The agent resolved or withdrew it itself (`ftm task done`).
    Resolved,
    /// A state from a newer ftmd. Never sent.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    /// The workspace of the agent that posted it, which an answer is typed into.
    pub workspace: WorkspaceId,
    pub kind: TaskKind,
    /// One line.
    pub title: String,
    /// Markdown: what the agent found, what it recommends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Suggested answers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// Unix seconds.
    pub created_at: u64,
    pub state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Unix seconds it stopped being open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<u64>,
}

/// At most this many characters of a title are kept.
pub const TITLE_MAX: usize = 200;

/// Drops control characters other than newline and tab. Text reaches the agent through its terminal, where an escape
/// could end a bracketed paste early and have the rest read as keys.
pub fn clean_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// `title` on one line: control characters dropped, runs of whitespace (newlines included) made one space, at most
/// `TITLE_MAX` characters. `None` when nothing is left.
pub fn clean_title(title: &str) -> Option<String> {
    let line = clean_text(title).split_whitespace().collect::<Vec<_>>().join(" ");
    (!line.is_empty()).then(|| line.chars().take(TITLE_MAX).collect())
}

/// What is typed into the agent's pane when the user answers `task`. It stands on its own in the agent's transcript:
/// the task's id and title, then the answer, which starts on its own line when it has several.
pub fn reply_message(task: &Task, answer: &str) -> String {
    let (verb, label) = match task.kind {
        TaskKind::Flag => ("reply", "FYI"),
        _ => ("answered", "Q"),
    };
    let answer = clean_text(answer);
    let answer = answer.trim();
    let sep = if answer.contains('\n') { "\n" } else { " " };
    format!("[FTM task #{} {verb}] {label}: {} — A:{sep}{answer}", task.id, task.title)
}

/// What every agent is told about the task inbox when its session starts (`ftm hook session-start`). It sits in
/// every agent's context, so it stays short.
pub const AGENT_GUIDE: &str = r#"FTM task inbox: reach the user from this pane without them switching to it. Use "$FTM_BIN" when the ftm on PATH may be older.
- ftm ask "<question>" [--body TEXT | --body-file FILE|-] [--option ANSWER]...  a decision only the user can make: approvals, production or irreversible actions, permission changes, product calls. Prints the task number N.
- ftm flag "<title>" [--body TEXT | --body-file FILE|-]  an FYI the user should see; no answer expected.
- ftm tasks [--json] [--all] lists tasks; ftm task done N resolves your own task (run it if you settle the question before the user answers); ftm task rm N deletes it.
After posting, end your turn. The answer arrives as your next prompt, exactly:
  [FTM task #N answered] Q: <question> — A: <answer>
  [FTM task #N reply] FYI: <title> — A: <reply>
A multi-line answer starts on the line after "A:".
Routing: technical and design questions go to your leader over SendMessage if you have one, not to ftm ask; approvals relayed over SendMessage get refused, so anything only the user can approve goes through ftm ask. A leader does not re-ask what a worker already asked. Never run ftm task answer or ftm task dismiss yourself: those are the user's."#;

/// The `SessionStart` hook's output for a session starting from `source` (Claude Code's hook input): the guide as
/// `additionalContext` for a fresh conversation (`startup`, `clear`), nothing when tasks are off or for `resume` and
/// `compact`, whose conversation already holds it.
pub fn session_start_output(source: &str, enabled: bool) -> Option<String> {
    if !enabled || !matches!(source, "startup" | "clear") {
        return None;
    }
    Some(
        serde_json::json!({
            "hookSpecificOutput": { "hookEventName": "SessionStart", "additionalContext": AGENT_GUIDE }
        })
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(kind: TaskKind) -> Task {
        Task {
            id: 7,
            workspace: WorkspaceId::nil(),
            kind,
            title: "Use Postgres or SQLite?".into(),
            body: None,
            options: vec![],
            created_at: 0,
            state: TaskState::Open,
            answer: None,
            closed_at: None,
        }
    }

    #[test]
    fn titles_are_one_clean_line() {
        assert_eq!(clean_title("  a\n b\t\x1b[201~c  ").as_deref(), Some("a b [201~c"));
        assert_eq!(clean_title(" \n\x07 "), None);
        assert_eq!(clean_title(&"x".repeat(500)).unwrap().len(), TITLE_MAX);
    }

    #[test]
    fn replies_stand_on_their_own() {
        assert_eq!(
            reply_message(&task(TaskKind::Decision), " SQLite \n"),
            "[FTM task #7 answered] Q: Use Postgres or SQLite? — A: SQLite"
        );
        assert_eq!(
            reply_message(&task(TaskKind::Flag), "thanks\r\n\x1b[201~more"),
            "[FTM task #7 reply] FYI: Use Postgres or SQLite? — A:\nthanks\n[201~more"
        );
    }

    #[test]
    fn the_guide_goes_to_fresh_sessions_while_tasks_are_on() {
        let out: serde_json::Value = serde_json::from_str(&session_start_output("startup", true).unwrap()).unwrap();
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "SessionStart");
        let guide = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(guide.contains("[FTM task #N answered] Q: <question> — A: <answer>"));
        assert!(session_start_output("clear", true).is_some());
        assert_eq!(session_start_output("resume", true), None);
        assert_eq!(session_start_output("compact", true), None);
        assert_eq!(session_start_output("startup", false), None);
        // The guide quotes the reply format the code produces.
        let mut t = task(TaskKind::Decision);
        t.id = 0;
        t.title = "<question>".into();
        assert!(guide.contains(&reply_message(&t, "<answer>").replace("#0", "#N")));
    }

    #[test]
    fn values_from_a_newer_ftmd_still_parse() {
        assert_eq!(serde_json::from_str::<TaskKind>(r#""poll""#).unwrap(), TaskKind::Unknown);
        assert_eq!(serde_json::from_str::<TaskState>(r#""snoozed""#).unwrap(), TaskState::Unknown);
        let t: Task = serde_json::from_str(
            r#"{"id":1,"workspace":"00000000-0000-0000-0000-000000000000","kind":"flag","title":"t","created_at":0,"state":"open","future":1}"#,
        )
        .unwrap();
        assert!(t.body.is_none() && t.options.is_empty());
    }
}
