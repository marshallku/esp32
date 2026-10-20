//! Claude Code permission prompts on the panel.
//!
//! Claude Code runs `deskd hook` as a `PermissionRequest` hook; the hook hands
//! the request to the daemon over the control socket and waits. The daemon
//! queues requests — several sessions can ask at once, the panel has one
//! modal — shows them one at a time, and answers each hook with the button
//! pressed.
//!
//! The rules that make this safe enough to approve commands with:
//! - "Allow" is offered only when the panel can show the whole operation
//!   (`summarize`); otherwise the choice is Deny or the terminal.
//! - Every way of not answering — panel gone, hook killed, timeout, the
//!   "Terminal" button — falls through to Claude Code's own prompt. Silence
//!   is never approval.
//! - Answers are matched by a random id, once; a late or repeated answer
//!   finds nothing.
//! - A finger already on the glass when the modal appears cannot answer it
//!   (enforced on the panel, `desk_core::app::ARM_MS`).

use std::collections::VecDeque;

use desk_core::{
    protocol::HostMsg,
    text::{BODY_COLS, BODY_ROWS, fits_body},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

/// What the hook forwards: the fields of Claude Code's PermissionRequest
/// input that the panel shows.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Ask {
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Value,
    #[serde(default)]
    pub cwd: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
    /// Leave it to Claude Code's own prompt.
    Terminal,
}

/// The hook's stdout for a decision; `None` means print nothing, which Claude
/// Code reads as "no decision" and falls back to its normal prompt.
pub fn hook_output(decision: Decision) -> Option<Value> {
    let decision = match decision {
        Decision::Allow => serde_json::json!({ "behavior": "allow" }),
        Decision::Deny => serde_json::json!({ "behavior": "deny", "message": "Denied from the desk panel." }),
        Decision::Terminal => return None,
    };
    Some(serde_json::json!({
        "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision }
    }))
}

/// Tools whose security-relevant input is a single path; the body shows it
/// and the size of the change.
const FILE_TOOLS: &[&str] = &["Edit", "MultiEdit", "Write", "NotebookEdit"];

/// Title, body, and whether "Allow" may be offered.
pub fn summarize(ask: &Ask) -> (String, String, bool) {
    let project = ask.cwd.rsplit('/').find(|s| !s.is_empty()).unwrap_or("?");
    let title = format!("{} @ {}", ask.tool_name, project);
    let input = &ask.tool_input;

    let (body, allowable) = if ask.tool_name == "Bash" {
        // The command itself is the whole operation, so it must be shown
        // byte for byte; nothing else counts.
        let command = input["command"].as_str().unwrap_or("");
        (command.to_string(), !command.is_empty())
    } else if FILE_TOOLS.contains(&ask.tool_name.as_str()) {
        let path = input["file_path"].as_str().or(input["notebook_path"].as_str()).unwrap_or("");
        let lines = |key: &str| input[key].as_str().map_or(0, |s| s.lines().count());
        let detail = match ask.tool_name.as_str() {
            "Write" => format!("write {} lines", lines("content")),
            "Edit" => format!("replace {} lines with {}", lines("old_string"), lines("new_string")),
            "MultiEdit" => format!("{} edits", input["edits"].as_array().map_or(0, Vec::len)),
            _ => "notebook edit".to_string(),
        };
        (format!("{path}\n\n{detail}"), !path.is_empty())
    } else {
        // Anything else: show what there is, but only the terminal can approve.
        (serde_json::to_string(input).unwrap_or_default(), false)
    };

    let allowable = allowable && fits_body(&body);
    // Shown as-is when it fits — that is what makes Allow safe — and
    // otherwise cut to what the modal holds, with a note. The cut version is
    // only ever shown next to Deny / Terminal. Bounding it also keeps the
    // prompt under the protocol's line limit, which an arbitrary command
    // would otherwise blow through and take the link down with it.
    let body = if allowable { body } else { displayable(&body) };
    (displayable_line(&title), body, allowable)
}

const TRUNCATED: &str = "[cut short - approve in the terminal]";

/// `text` reduced to what the panel's font can draw, at most a modal's worth.
/// Characters outside printable ASCII become '?': they are exactly what must
/// not be passed off as something they are not.
fn displayable(text: &str) -> String {
    let clean: String = text.chars().map(|c| if c == '\n' || (' '..='~').contains(&c) { c } else { '?' }).collect();
    if fits_body(&clean) {
        return clean;
    }
    // Leave the last row for the note.
    let mut lines = desk_core::text::wrap(&clean, BODY_COLS);
    lines.truncate(BODY_ROWS - 1);
    let mut out = lines.join("\n");
    out.push('\n');
    out.push_str(TRUNCATED);
    out
}

/// One line of at most a modal row, for the title.
fn displayable_line(text: &str) -> String {
    text.chars().map(|c| if (' '..='~').contains(&c) { c } else { '?' }).take(BODY_COLS).collect()
}

struct Pending {
    id: u64,
    ask: Ask,
    reply: oneshot::Sender<Decision>,
}

/// The queue of prompts waiting for the panel; the front one is on screen.
#[derive(Default)]
pub struct Approvals {
    queue: VecDeque<Pending>,
}

impl Approvals {
    /// Adds a request. Returns the prompt to show if it went straight to the
    /// front, or an updated title for the current one ("+N waiting").
    pub fn push(&mut self, id: u64, ask: Ask, reply: oneshot::Sender<Decision>) -> Option<HostMsg> {
        self.queue.push_back(Pending { id, ask, reply });
        self.current_prompt()
    }

    /// The panel answered prompt `id` with button `button`. Returns the next
    /// prompt to show, if any. Unknown or stale ids change nothing.
    pub fn answer(&mut self, id: u64, button: u8) -> Option<HostMsg> {
        if self.queue.front().is_none_or(|p| p.id != id) {
            return None;
        }
        let pending = self.queue.pop_front()?;
        let (_, _, allowable) = summarize(&pending.ask);
        let decision = match (buttons(allowable).get(button as usize).copied(), allowable) {
            (Some("Allow"), true) => Decision::Allow,
            (Some("Deny"), _) => Decision::Deny,
            _ => Decision::Terminal,
        };
        let _ = pending.reply.send(decision);
        self.current_prompt()
    }

    /// Drops requests whose hook has gone away (killed, timed out). Returns
    /// what to do on the panel: dismiss the one that was showing, then show
    /// the new front.
    pub fn reap(&mut self) -> Vec<HostMsg> {
        let showing = self.queue.front().map(|p| p.id);
        let before = self.queue.len();
        self.queue.retain(|p| !p.reply.is_closed());
        if self.queue.len() == before {
            return Vec::new();
        }
        let mut msgs = Vec::new();
        if let Some(id) = showing
            && self.queue.front().map(|p| p.id) != Some(id)
        {
            msgs.push(HostMsg::Dismiss { id });
        }
        msgs.extend(self.current_prompt());
        msgs
    }

    /// The front request as a prompt, with the queue length in its title.
    pub fn current_prompt(&self) -> Option<HostMsg> {
        let front = self.queue.front()?;
        let (mut title, body, allowable) = summarize(&front.ask);
        if self.queue.len() > 1 {
            title.push_str(&format!(" (+{})", self.queue.len() - 1));
        }
        Some(HostMsg::Prompt {
            id: front.id,
            title,
            body,
            buttons: buttons(allowable).iter().map(|s| s.to_string()).collect(),
        })
    }
}

/// The affirmative button goes last, where the panel colours it as such.
fn buttons(allowable: bool) -> &'static [&'static str] {
    if allowable { &["Deny", "Terminal", "Allow"] } else { &["Deny", "Terminal"] }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn bash(command: &str) -> Ask {
        Ask { tool_name: "Bash".into(), tool_input: json!({ "command": command }), cwd: "/home/u/dev/esp32".into() }
    }

    fn prompt_buttons(msg: Option<HostMsg>) -> Vec<String> {
        match msg {
            Some(HostMsg::Prompt { buttons, .. }) => buttons,
            other => panic!("expected a prompt, got {other:?}"),
        }
    }

    #[test]
    fn bash_shows_the_full_command_and_may_be_allowed() {
        let (title, body, allowable) = summarize(&bash("cargo test -p deskd"));
        assert_eq!((title.as_str(), body.as_str(), allowable), ("Bash @ esp32", "cargo test -p deskd", true));
    }

    #[test]
    fn allow_is_withheld_when_the_command_cannot_be_shown_whole() {
        assert!(!summarize(&bash(&"x".repeat(2000))).2, "too long for the modal");
        assert!(!summarize(&bash("echo \u{202e}gnp.exe")).2, "a glyph the panel cannot draw");
        let other = Ask { tool_name: "WebFetch".into(), tool_input: json!({ "url": "https://x" }), cwd: "/".into() };
        assert!(!summarize(&other).2, "tools outside v1's list go to the terminal");
    }

    #[test]
    fn any_request_fits_one_protocol_line() {
        // Quotes and backslashes double when escaped, non-ASCII far more;
        // whatever arrives, the prompt must stay a single deliverable line.
        let (tx, _rx) = oneshot::channel();
        let mut a = Approvals::default();
        let nasty = "\"\\é\u{202e}".repeat(3000);
        let msg = a.push(1, bash(&nasty), tx).unwrap();
        let line = desk_core::protocol::encode_host(&msg);
        assert!(line.len() <= desk_core::protocol::MAX_LINE, "{} bytes", line.len());
        let HostMsg::Prompt { body, buttons, .. } = msg else { panic!("expected a prompt") };
        assert!(body.ends_with(TRUNCATED));
        assert!(fits_body(&body), "the cut body itself shows completely");
        assert_eq!(buttons, ["Deny", "Terminal"]);
    }

    #[test]
    fn file_tools_show_the_path_and_change_size() {
        let ask = Ask {
            tool_name: "Write".into(),
            tool_input: json!({ "file_path": "/home/u/a.rs", "content": "a\nb\nc" }),
            cwd: "/home/u".into(),
        };
        let (_, body, allowable) = summarize(&ask);
        assert_eq!(body, "/home/u/a.rs\n\nwrite 3 lines");
        assert!(allowable);
    }

    #[test]
    fn answers_map_buttons_to_decisions_and_advance_the_queue() {
        let mut a = Approvals::default();
        let (tx1, mut rx1) = oneshot::channel();
        let (tx2, mut rx2) = oneshot::channel();
        assert_eq!(prompt_buttons(a.push(1, bash("ls"), tx1)), ["Deny", "Terminal", "Allow"]);
        match a.push(2, bash(&"y".repeat(2000)), tx2) {
            Some(HostMsg::Prompt { id: 1, title, .. }) => assert!(title.ends_with("(+1)")),
            other => panic!("front prompt should stay, with a count: {other:?}"),
        }

        assert_eq!(a.answer(2, 0), None, "only the prompt on screen can be answered");
        let next = a.answer(1, 2);
        assert_eq!(rx1.try_recv().unwrap(), Decision::Allow);
        assert_eq!(prompt_buttons(next), ["Deny", "Terminal"], "second request is not allowable");

        // Button 2 does not exist on a two-button prompt: never an Allow.
        a.answer(2, 2);
        assert_eq!(rx2.try_recv().unwrap(), Decision::Terminal);
        assert_eq!(a.answer(1, 2), None, "late duplicate");
    }

    #[test]
    fn gone_hooks_are_reaped_and_their_prompt_dismissed() {
        let mut a = Approvals::default();
        let (tx1, rx1) = oneshot::channel();
        let (tx2, _rx2) = oneshot::channel();
        a.push(1, bash("a"), tx1);
        a.push(2, bash("b"), tx2);
        drop(rx1);
        let msgs = a.reap();
        assert!(matches!(msgs[0], HostMsg::Dismiss { id: 1 }));
        assert!(matches!(msgs[1], HostMsg::Prompt { id: 2, .. }));
        assert!(a.reap().is_empty());
    }

    #[test]
    fn hook_output_shapes() {
        assert_eq!(hook_output(Decision::Allow).unwrap()["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert_eq!(hook_output(Decision::Deny).unwrap()["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
        assert_eq!(hook_output(Decision::Terminal), None);
    }
}
