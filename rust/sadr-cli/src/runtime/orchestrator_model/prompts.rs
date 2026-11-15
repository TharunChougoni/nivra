use super::*;

/// User-turn triggers for the persistent orchestrator session. They are
/// distinct from the model-role `Trigger` selectors and share its supervisor
/// identity and usage authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionTrigger {
    SessionOpen,
    Resume,
    Change,
    PreClose,
    PostFill,
    PostDecide,
    LunaNoDecideStreak,
    FailureStreak,
    Idle,
    Decision,
    Research,
    Compaction,
    Handoff,
}

impl SessionTrigger {
    pub(crate) const ALL: [Self; 13] = [
        Self::SessionOpen,
        Self::Resume,
        Self::Change,
        Self::PreClose,
        Self::PostFill,
        Self::PostDecide,
        Self::LunaNoDecideStreak,
        Self::FailureStreak,
        Self::Idle,
        Self::Decision,
        Self::Research,
        Self::Compaction,
        Self::Handoff,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SessionOpen => "session_open",
            Self::Resume => "resume",
            Self::Change => "change",
            Self::PreClose => "pre_close",
            Self::PostFill => "post_fill",
            Self::PostDecide => "post_decide",
            Self::LunaNoDecideStreak => "luna_no_decide_streak",
            Self::FailureStreak => "failure_streak",
            Self::Idle => "idle",
            Self::Decision => "decision",
            Self::Research => "research",
            Self::Compaction => "compaction",
            Self::Handoff => "handoff",
        }
    }

    /// The default short user-turn template. It is used only when the
    /// configuration does not provide one and never replaces the standing role.
    pub(crate) const fn default_prompt(self) -> &'static str {
        match self {
            Self::SessionOpen => {
                "Session {{session}} opens at minute {{session_minute}}. Notes: {{notes}}. Recent turns: {{recent_turns}}. Summary: {{summary}}. Review operations and reply with the strict JSON object."
            }
            Self::Resume => {
                "Session {{session}} resumes at minute {{session_minute}} after a restart or deploy. Notes: {{notes}}. Recent turns: {{recent_turns}}. Resume the interrupted work."
            }
            Self::Change => {
                "Applied inputs changed for session {{session}}: {{change}}. Notes: {{notes}}. Review the change."
            }
            Self::PreClose => {
                "Pre-close review at minute {{session_minute}} for session {{session}}. Fills: {{fills}}. Notes: {{notes}}."
            }
            Self::PostFill => {
                "A fill was recorded in session {{session}} ({{fills}} today). Notes: {{notes}}. Review the position."
            }
            Self::PostDecide => {
                "A digest decision was recorded in session {{session}}. Notes: {{notes}}. Review it."
            }
            Self::LunaNoDecideStreak => {
                "No digest decision has been recorded for {{luna_no_decide_streak}} turns in session {{session}}. Notes: {{notes}}."
            }
            Self::FailureStreak => {
                "Supervisor failure streak {{supervisor_failures}} in session {{session}}. Notes: {{notes}}."
            }
            Self::Idle => {
                "Idle review for session {{session}} at minute {{session_minute}}. Notes: {{notes}}. Active directives: {{active_directives}}."
            }
            Self::Decision => {
                "Paper decision turn for session {{session}} at minute {{session_minute}}. Notes: {{notes}}. Factual digest: {{digest}}. Recent turns: {{recent_turns}}. Summary: {{summary}}. Prior seed: {{seed}}. Return the strict paper TradePlan JSON."
            }
            Self::Research => {
                "Research turn for session {{session}} at minute {{session_minute}}. Notes: {{notes}}. Recent turns: {{recent_turns}}. Summary: {{summary}}. Evidence: {{evidence}}. Pinned workspace refs: {{workspace_refs}}. Active directives: {{active_directives}}. Return the strict bounded research JSON; no orders or financial fields."
            }
            Self::Compaction => {
                "Summarise the session {{session}} turns so far. Keep every note and fact needed for future turns. Summary start: {{summary}}."
            }
            Self::Handoff => {
                "Write the end-of-day handoff for session {{session}}. Positions: {{positions}}. Notes: {{notes}}. Summary: {{summary}}."
            }
        }
    }

    /// Placeholders a template for this trigger may use. `{{summary}}`,
    /// `{{notes}}` and friends are plain facts supplied by the host; an unknown
    /// or misspelled placeholder fails validation instead of rendering empty.
    pub(crate) fn facts(self) -> &'static [&'static str] {
        match self {
            Self::SessionOpen | Self::Resume => &[
                "session",
                "session_minute",
                "notes",
                "recent_turns",
                "summary",
                "seed",
                "applied_hashes",
                "current_hashes",
                "active_directives",
            ],
            Self::Change => &[
                "session",
                "session_minute",
                "notes",
                "recent_turns",
                "summary",
                "change",
            ],
            Self::PreClose => &[
                "session",
                "session_minute",
                "notes",
                "recent_turns",
                "summary",
                "fills",
            ],
            Self::PostFill => &["session", "notes", "fills", "positions"],
            Self::PostDecide => &["session", "notes", "latest_luna_review"],
            Self::LunaNoDecideStreak => {
                &["session", "notes", "luna_no_decide_streak", "recent_turns"]
            }
            Self::FailureStreak => &["session", "notes", "supervisor_failures"],
            Self::Idle => &[
                "session",
                "session_minute",
                "notes",
                "active_directives",
                "recent_turns",
                "summary",
            ],
            Self::Decision => &[
                "session",
                "session_minute",
                "notes",
                "summary",
                "recent_turns",
                "seed",
                "digest",
                "decision_evidence",
            ],
            Self::Research => &[
                "session",
                "session_minute",
                "notes",
                "summary",
                "recent_turns",
                "evidence",
                "workspace_refs",
                "active_directives",
            ],
            Self::Compaction => &["session", "summary", "recent_turns", "notes"],
            Self::Handoff => &[
                "session",
                "positions",
                "notes",
                "summary",
                "recent_turns",
                "active_directives",
            ],
        }
    }
}

pub(crate) fn digest_text(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn placeholders(template: &str) -> Result<Vec<&str>> {
    let mut found = Vec::new();
    let mut rest = template;
    loop {
        let Some(start) = rest.find("{{") else {
            if rest.contains("}}") {
                return Err("orchestrator_prompt_placeholder_invalid".into());
            }
            return Ok(found);
        };
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or("orchestrator_prompt_placeholder_invalid")?;
        let key = after[..end].trim();
        if key.is_empty()
            || key.len() > 64
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err("orchestrator_prompt_placeholder_invalid".into());
        }
        found.push(key);
        rest = &after[end + 2..];
    }
}

/// Render a validated trigger template with the host-owned facts. A fact the
/// trigger does not supply (but validation allowed) renders as JSON null, never
/// as an empty string that could read as "nothing happened".
// Wired into the supervisor turn builder as the persistent session lands.
#[allow(dead_code)]
pub(crate) fn render_trigger(template: &str, facts: &Value) -> String {
    let mut out = String::with_capacity(template.len() + 256);
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let key = after[..end].trim();
        let value = &facts[key];
        match value {
            Value::String(text) => out.push_str(text),
            other => out.push_str(&other.to_string()),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

impl OrchestratorModelSettings {
    /// Resolve and validate the standing session prompt and every trigger
    /// template. An explicit template must use only that trigger's known
    /// placeholders; an invalid template never falls back silently.
    pub(crate) fn session_prompts(
        &self,
    ) -> Result<(LoadedPrompt, BTreeMap<SessionTrigger, String>)> {
        let system = match &self.system_prompt {
            Some(path) => load_prompt_file(path, self.max_prompt_bytes)?,
            None => self.load_prompt(Trigger::Supervisor)?,
        };
        let mut templates = BTreeMap::new();
        for trigger in SessionTrigger::ALL {
            let template = self
                .trigger_prompts
                .get(&trigger)
                .cloned()
                .unwrap_or_else(|| trigger.default_prompt().to_owned());
            if template.len() > 8192 {
                return Err("orchestrator_trigger_prompt_oversize".into());
            }
            for key in placeholders(&template)? {
                if !trigger.facts().contains(&key) {
                    return Err("orchestrator_trigger_prompt_unknown_placeholder".into());
                }
            }
            templates.insert(trigger, template);
        }
        Ok((system, templates))
    }
}

pub(crate) fn load_prompt_file(path: &Path, max_bytes: u64) -> Result<LoadedPrompt> {
    let file = File::open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "orchestrator_prompt_missing"
        } else {
            "orchestrator_prompt_unreadable"
        }
    })?;
    if !file
        .metadata()
        .map_err(|_| "orchestrator_prompt_unreadable")?
        .is_file()
    {
        return Err("orchestrator_prompt_unreadable".into());
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| "orchestrator_prompt_unreadable")?;
    if bytes.len() as u64 > max_bytes {
        return Err("orchestrator_prompt_oversize".into());
    }
    let sha256 = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let text = String::from_utf8(bytes).map_err(|_| "orchestrator_prompt_invalid_utf8")?;
    Ok(LoadedPrompt { text, sha256 })
}
