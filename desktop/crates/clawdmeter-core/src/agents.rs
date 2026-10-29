//! The device's Agentes page: Claude Code sessions and the subagents each one
//! runs, sent as the optional `ag` key (`docs/protocol.md`).
//!
//! The hook appends one [`AgentEvent`] per event to `<state>/agents.log`
//! (JSON lines, next to events.log, whose line format has no room for
//! more fields). [`Agents`] folds them into sessions and agents and
//! [`Agents::build`] renders the `ag` value the firmware's `agents_parse`
//! reads: `[[name, state, [[type, task, secs, running], ...]], ...]`.
//!
//! A subagent's short task comes from the `Agent` (formerly `Task`) tool's
//! `PreToolUse`: its `description` is queued per session and taken by the
//! `SubagentStart` that follows (same `agent_type` first, else the oldest),
//! since the subagent events carry no `tool_use_id` to match on.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Done agents stay listed this long after they stop (s).
pub const DONE_TTL_SECS: i64 = 600;
/// A session with no event for this long is dropped, unless an agent runs (s).
pub const SESSION_IDLE_SECS: i64 = 300;
/// A running agent with no stop for this long is dropped as stale (s).
pub const STALE_RUNNING_SECS: i64 = 7200;
/// Sessions the firmware keeps (`AGENTS_MAX_SESSIONS`).
pub const MAX_SESSIONS: usize = 4;
/// Agents the firmware keeps, across sessions (`AGENTS_MAX_AGENTS`).
pub const MAX_AGENTS: usize = 10;
/// Byte budget for the whole `{"ag":…}` write (the app caps writes at 500).
pub const MAX_BYTES: usize = 480;
/// Firmware string limits, in characters.
pub const NAME_CHARS: usize = 16;
/// See [`NAME_CHARS`].
pub const TYPE_CHARS: usize = 14;
/// See [`NAME_CHARS`].
pub const TASK_CHARS: usize = 24;
/// The firmware clamps seconds to 99h 59m 59s.
pub const MAX_SECS: i64 = 359_999;

/// One agents.log line. Only what the page needs: no prompts, no paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEvent {
    /// Unix seconds.
    #[serde(rename = "t")]
    pub ts: i64,
    /// `hook_event_name`.
    #[serde(rename = "e")]
    pub event: String,
    /// `session_id`.
    #[serde(rename = "s")]
    pub session: String,
    /// The project folder name (last component of `cwd`).
    #[serde(rename = "n", default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// `agent_id` (SubagentStart / SubagentStop).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `agent_type`, or the Agent tool's `subagent_type`.
    #[serde(rename = "ty", default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// The Agent tool's `description` (PreToolUse), clipped to [`TASK_CHARS`].
    #[serde(rename = "d", default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

impl AgentEvent {
    /// The agents.log line, newline included.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("serializes") + "\n"
    }
}

/// Parse one agents.log line; anything else is `None`.
pub fn parse_line(line: &str) -> Option<AgentEvent> {
    serde_json::from_str(line).ok()
}

/// The first `n` characters of `s` (never splits one).
pub fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[derive(Debug, Clone)]
struct Agent {
    id: String,
    ty: String,
    task: String,
    start: i64,
    end: Option<i64>,
}

#[derive(Debug, Clone)]
struct Session {
    name: String,
    first: i64,
    last: i64,
    prompt_open: bool,
    /// Agent tool calls not yet matched to a SubagentStart: (type, task, ts).
    pending: VecDeque<(String, String, i64)>,
    agents: Vec<Agent>,
}

impl Session {
    fn running(&self) -> bool {
        self.agents.iter().any(|a| a.end.is_none())
    }

    /// 0 paused, 1 working, 2 waiting on agents.
    fn state(&self) -> u8 {
        if self.running() {
            2
        } else {
            u8::from(self.prompt_open)
        }
    }
}

/// Sessions and their subagents, from agents.log.
#[derive(Debug, Clone, Default)]
pub struct Agents {
    sessions: HashMap<String, Session>,
}

/// One agent as the device gets it.
#[derive(Debug, Clone)]
struct Row {
    ty: String,
    task: String,
    secs: i64,
    running: bool,
    /// Start, or end for a done agent: drop order.
    at: i64,
}

#[derive(Debug, Clone)]
struct SessionRow {
    name: String,
    state: u8,
    last: i64,
    agents: Vec<Row>,
}

fn render(rows: &[SessionRow], zero_running: bool) -> Value {
    Value::Array(
        rows.iter()
            .map(|s| {
                let agents: Vec<Value> = s
                    .agents
                    .iter()
                    .map(|a| {
                        let secs = if zero_running && a.running { 0 } else { a.secs };
                        json!([a.ty, a.task, secs, u8::from(a.running)])
                    })
                    .collect();
                json!([s.name, s.state, agents])
            })
            .collect(),
    )
}

fn write_len(ag: &Value) -> usize {
    json!({ "ag": ag }).to_string().len()
}

impl Agents {
    /// Fold in one event.
    pub fn ingest(&mut self, ev: &AgentEvent) {
        if ev.session.is_empty() {
            return;
        }
        if ev.event == "SessionEnd" {
            self.sessions.remove(&ev.session);
            return;
        }
        let s = self.sessions.entry(ev.session.clone()).or_insert_with(|| Session {
            name: String::new(),
            first: ev.ts,
            last: ev.ts,
            prompt_open: false,
            pending: VecDeque::new(),
            agents: Vec::new(),
        });
        if let Some(n) = ev.project.as_deref().filter(|n| !n.is_empty()) {
            s.name = clip(n, NAME_CHARS);
        }
        s.last = s.last.max(ev.ts);
        match ev.event.as_str() {
            "UserPromptSubmit" => s.prompt_open = true,
            "Stop" => s.prompt_open = false,
            // Only the Agent/Task tool's PreToolUse carries a type.
            "PreToolUse" => {
                if let Some(ty) = &ev.agent_type {
                    s.pending.push_back((ty.clone(), ev.task.clone().unwrap_or_default(), ev.ts));
                    if s.pending.len() > MAX_AGENTS {
                        s.pending.pop_front();
                    }
                }
            }
            "SubagentStart" => {
                let id = ev.id.clone().unwrap_or_default();
                if !id.is_empty() && s.agents.iter().any(|a| a.id == id) {
                    return;
                }
                let ty = ev.agent_type.clone().unwrap_or_default();
                // shortcut: no shared key between the tool call and the
                // subagent, so FIFO by type (else oldest); parallel agents
                // whose types differ in name between the two can swap tasks.
                let i = s.pending.iter().position(|p| p.0 == ty).or((!s.pending.is_empty()).then_some(0));
                let task = i.and_then(|i| s.pending.remove(i)).map(|p| p.1).unwrap_or_default();
                s.agents.push(Agent { id, ty, task, start: ev.ts, end: None });
            }
            "SubagentStop" => {
                let id = ev.id.as_deref().unwrap_or_default();
                if let Some(a) = s.agents.iter_mut().find(|a| a.end.is_none() && a.id == id) {
                    a.end = Some(ev.ts);
                }
            }
            _ => {}
        }
    }

    /// Drop what expired by `now`.
    pub fn prune(&mut self, now: i64) {
        for s in self.sessions.values_mut() {
            s.agents.retain(|a| match a.end {
                Some(end) => now - end < DONE_TTL_SECS,
                None => now - a.start < STALE_RUNNING_SECS,
            });
            s.pending.retain(|p| now - p.2 < DONE_TTL_SECS);
        }
        self.sessions.retain(|_, s| s.running() || now - s.last < SESSION_IDLE_SECS);
    }

    /// The `ag` value as of `now`, within the firmware's limits and
    /// [`MAX_BYTES`], plus a key that changes only when what the page shows
    /// changes (running agents' seconds left out). Over the limits, done
    /// agents go first (oldest first), then paused sessions (least recently
    /// active first), then the newest running agents, then the last sessions.
    pub fn build(&self, now: i64) -> (Value, String) {
        let mut sessions: Vec<(&String, &Session)> = self.sessions.iter().collect();
        sessions.sort_by_key(|(id, s)| (s.first, *id));
        let mut rows: Vec<SessionRow> = sessions
            .into_iter()
            .map(|(_, s)| SessionRow {
                name: if s.name.is_empty() { "Claude".into() } else { s.name.clone() },
                state: s.state(),
                last: s.last,
                agents: s
                    .agents
                    .iter()
                    .map(|a| Row {
                        ty: clip(&a.ty, TYPE_CHARS),
                        task: clip(&a.task, TASK_CHARS),
                        secs: (a.end.unwrap_or(now) - a.start).clamp(0, MAX_SECS),
                        running: a.end.is_none(),
                        at: a.end.unwrap_or(a.start),
                    })
                    .collect(),
            })
            .collect();
        let paused = |rows: &[SessionRow]| {
            rows.iter().enumerate().filter(|(_, s)| s.state == 0).min_by_key(|(_, s)| s.last).map(|(i, _)| i)
        };
        while rows.len() > MAX_SESSIONS {
            let i = paused(&rows).unwrap_or(rows.len() - 1);
            rows.remove(i);
        }
        loop {
            let agents: usize = rows.iter().map(|s| s.agents.len()).sum();
            let too_big = write_len(&render(&rows, false)) > MAX_BYTES;
            if agents <= MAX_AGENTS && !too_big {
                break;
            }
            let oldest = |want_running: bool, newest: bool| {
                rows.iter()
                    .enumerate()
                    .flat_map(|(si, s)| s.agents.iter().enumerate().map(move |(ai, a)| (si, ai, a)))
                    .filter(|(_, _, a)| a.running == want_running)
                    .min_by_key(|(_, _, a)| if newest { -a.at } else { a.at })
                    .map(|(si, ai, _)| (si, ai))
            };
            if let Some((si, ai)) = oldest(false, false) {
                rows[si].agents.remove(ai);
            } else if let Some(i) = paused(&rows).filter(|_| too_big) {
                rows.remove(i);
            } else if let Some((si, ai)) = oldest(true, true) {
                rows[si].agents.remove(ai);
            } else {
                rows.pop();
            }
        }
        (render(&rows, false), render(&rows, true).to_string())
    }

    /// Nothing to list.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_760_000_000;

    fn ev(ts: i64, event: &str, sid: &str) -> AgentEvent {
        AgentEvent {
            ts,
            event: event.into(),
            session: sid.into(),
            project: Some("Clawdmeter".into()),
            id: None,
            agent_type: None,
            task: None,
        }
    }

    fn call(ts: i64, sid: &str, ty: &str, task: &str) -> AgentEvent {
        AgentEvent { agent_type: Some(ty.into()), task: Some(task.into()), ..ev(ts, "PreToolUse", sid) }
    }

    fn sub(ts: i64, event: &str, sid: &str, id: &str, ty: &str) -> AgentEvent {
        AgentEvent { id: Some(id.into()), agent_type: Some(ty.into()), ..ev(ts, event, sid) }
    }

    fn ag(a: &Agents, now: i64) -> String {
        a.build(now).0.to_string()
    }

    #[test]
    fn lines_round_trip_and_tolerate_missing_fields() {
        let e = call(T, "s1", "Explore", "rutas API");
        let line = e.to_line();
        assert_eq!(line, "{\"t\":1760000000,\"e\":\"PreToolUse\",\"s\":\"s1\",\"n\":\"Clawdmeter\",\"ty\":\"Explore\",\"d\":\"rutas API\"}\n");
        assert_eq!(parse_line(line.trim_end()), Some(e));
        assert_eq!(parse_line(r#"{"t":5,"e":"Stop","s":"x","future":1}"#).unwrap().project, None);
        assert_eq!(parse_line("1760000000 Stop abc"), None);
        assert_eq!(parse_line(r#"{"e":"Stop","s":"x"}"#), None);
    }

    #[test]
    fn tasks_follow_their_tool_call() {
        let mut a = Agents::default();
        a.ingest(&ev(T, "UserPromptSubmit", "s1"));
        assert_eq!(ag(&a, T), r#"[["Clawdmeter",1,[]]]"#);
        a.ingest(&call(T + 1, "s1", "Explore", "rutas API"));
        a.ingest(&call(T + 1, "s1", "general-purpose", "tests"));
        a.ingest(&call(T + 1, "s1", "Explore", "docs"));
        // Started out of order: matched by type, then FIFO within the type.
        a.ingest(&sub(T + 2, "SubagentStart", "s1", "g", "general-purpose"));
        a.ingest(&sub(T + 2, "SubagentStart", "s1", "e1", "Explore"));
        a.ingest(&sub(T + 3, "SubagentStart", "s1", "e2", "Explore"));
        // A type the tool call named differently takes the oldest left (none here).
        a.ingest(&sub(T + 3, "SubagentStart", "s1", "x", "plugin:rev"));
        assert_eq!(
            ag(&a, T + 10),
            r#"[["Clawdmeter",2,[["general-purpos","tests",8,1],["Explore","rutas API",8,1],["Explore","docs",7,1],["plugin:rev","",7,1]]]]"#
        );
        a.ingest(&sub(T + 20, "SubagentStop", "s1", "e1", "Explore"));
        a.ingest(&sub(T + 20, "SubagentStart", "s1", "e1", "Explore")); // duplicate id: ignored
        let (v, _) = a.build(T + 30);
        assert_eq!(v[0][2][1], json!(["Explore", "rutas API", 18, 0]), "done: total duration, frozen");
    }

    #[test]
    fn session_states() {
        let mut a = Agents::default();
        a.ingest(&ev(T, "UserPromptSubmit", "s1"));
        assert_eq!(a.build(T).0[0][1], 1);
        a.ingest(&sub(T + 1, "SubagentStart", "s1", "a", "Explore"));
        assert_eq!(a.build(T + 1).0[0][1], 2);
        a.ingest(&sub(T + 2, "SubagentStop", "s1", "a", "Explore"));
        assert_eq!(a.build(T + 2).0[0][1], 1);
        a.ingest(&ev(T + 3, "Stop", "s1"));
        assert_eq!(a.build(T + 3).0[0][1], 0);
        // A background agent still running after the turn ended.
        a.ingest(&sub(T + 4, "SubagentStart", "s1", "b", "Explore"));
        assert_eq!(a.build(T + 4).0[0][1], 2);
        a.ingest(&ev(T + 5, "SessionEnd", "s1"));
        assert!(a.is_empty());
        // No session id: ignored.
        a.ingest(&ev(T, "UserPromptSubmit", ""));
        assert!(a.is_empty());
    }

    #[test]
    fn expiry() {
        let mut a = Agents::default();
        a.ingest(&ev(T, "UserPromptSubmit", "idle"));
        a.ingest(&ev(T, "UserPromptSubmit", "busy"));
        a.ingest(&sub(T, "SubagentStart", "busy", "long", "Explore"));
        a.ingest(&ev(T, "UserPromptSubmit", "done"));
        a.ingest(&sub(T, "SubagentStart", "done", "d", "Plan"));
        a.ingest(&sub(T + 10, "SubagentStop", "done", "d", "Plan"));
        a.ingest(&ev(T + 290, "PostToolUse", "done"));
        a.prune(T + SESSION_IDLE_SECS);
        assert_eq!(a.sessions.len(), 2, "idle session gone, the one with a running agent stays");
        a.prune(T + 10 + DONE_TTL_SECS);
        assert!(!a.sessions.contains_key("done"), "its done agent expired and it went quiet");
        assert!(a.sessions["busy"].running());
        a.prune(T + STALE_RUNNING_SECS);
        assert!(a.is_empty(), "a running agent with no stop for 2 h is stale");
    }

    #[test]
    fn limits_and_drop_order() {
        let mut a = Agents::default();
        // Six sessions; two paused ones go first, the oldest-active first.
        for (i, sid) in ["a", "b", "c", "d", "e", "f"].iter().enumerate() {
            let t = T + i as i64;
            a.ingest(&AgentEvent { project: Some(format!("p{sid}")), ..ev(t, "UserPromptSubmit", sid) });
        }
        a.ingest(&ev(T + 10, "Stop", "b"));
        a.ingest(&ev(T + 9, "Stop", "d"));
        let names: Vec<String> = a.build(T + 20).0.as_array().unwrap().iter().map(|s| s[0].to_string()).collect();
        assert_eq!(names, ["\"pa\"", "\"pc\"", "\"pe\"", "\"pf\""], "paused b, d dropped");

        // 12 agents in one session: the 2 oldest done go.
        let mut a = Agents::default();
        a.ingest(&ev(T, "UserPromptSubmit", "s"));
        for i in 0..12 {
            a.ingest(&sub(T + i, "SubagentStart", "s", &format!("a{i}"), "E"));
        }
        for i in 0..4 {
            a.ingest(&sub(T + 20 + i, "SubagentStop", "s", &format!("a{i}"), "E"));
        }
        let (v, _) = a.build(T + 30);
        let list = v[0][2].as_array().unwrap();
        assert_eq!(list.len(), MAX_AGENTS);
        assert_eq!(list[0], json!(["E", "", 20, 0]), "a0 and a1 (oldest done) dropped");
    }

    #[test]
    fn byte_budget_is_utf8_safe() {
        let mut a = Agents::default();
        let long = "ñ".repeat(40);
        for s in 0..4 {
            let sid = format!("s{s}");
            a.ingest(&AgentEvent { project: Some(long.clone()), ..ev(T, "UserPromptSubmit", &sid) });
            for i in 0..3 {
                a.ingest(&call(T, &sid, &long, &long));
                a.ingest(&sub(T + i, "SubagentStart", &sid, &format!("{s}{i}"), &long));
            }
        }
        a.ingest(&sub(T + 5, "SubagentStop", "s0", "00", &long));
        for s in 0..4 {
            a.ingest(&AgentEvent { project: Some(long.clone()), ..ev(T + 6, "PostToolUse", &format!("s{s}")) });
        }
        let (v, key) = a.build(T + 10);
        let write = json!({ "ag": v }).to_string();
        assert!(write.len() <= MAX_BYTES, "{} bytes", write.len());
        let first = &v[0];
        assert_eq!(first[0].as_str().unwrap().chars().count(), NAME_CHARS);
        let agent = v.as_array().unwrap().iter().find_map(|s| s[2].get(0)).unwrap();
        assert_eq!(agent[0].as_str().unwrap().chars().count(), TYPE_CHARS);
        assert_eq!(agent[1].as_str().unwrap().chars().count(), TASK_CHARS);
        assert_eq!(agent[3], 1, "the done agent went first");
        assert!(v.to_string().contains("ñ"), "multi-byte text survives whole");
        // The key ignores running agents' seconds.
        assert_eq!(a.build(T + 99).1, key);
    }

    #[test]
    fn matches_the_simulator_scenario_shape() {
        let mut a = Agents::default();
        let p = |ts, ev_: &str, sid: &str, n: &str| AgentEvent { project: Some(n.into()), ..ev(ts, ev_, sid) };
        a.ingest(&p(T, "UserPromptSubmit", "1", "Clawdmeter"));
        a.ingest(&call(T, "1", "programador", "sesiones"));
        a.ingest(&sub(T, "SubagentStart", "1", "x", "programador"));
        a.ingest(&p(T + 1, "UserPromptSubmit", "2", "Docs"));
        a.ingest(&p(T + 2, "Stop", "2", "Docs"));
        assert_eq!(
            json!({ "ag": a.build(T + 240).0 }).to_string(),
            r#"{"ag":[["Clawdmeter",2,[["programador","sesiones",240,1]]],["Docs",0,[]]]}"#
        );
    }
}
