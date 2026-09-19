#[derive(Default)]
pub(super) struct State {
    pub(super) path: String,
    pub(super) session: String,
    pub(super) window: String,
    pub(super) window_index: usize,
    pub(super) windows: usize,
    pub(super) windows_done: usize,
    pub(super) pane_index: usize,
    pub(super) panes: usize,
    pub(super) panes_done: usize,
    pub(super) total_panes: usize,
    pub(super) total_done: usize,
    pub(super) status: &'static str,
}

pub(super) struct Template(Vec<Part>);

enum Part {
    Literal(String),
    Field(&'static str),
}

const FIELDS: &[&str] = &[
    "workspace_path",
    "session",
    "window",
    "window_index",
    "window_total",
    "window_progress",
    "pane_index",
    "pane_total",
    "pane_progress",
    "progress",
    "windows_done",
    "windows_remaining",
    "window_progress_rel",
    "pane_done",
    "pane_remaining",
    "pane_progress_rel",
    "session_pane_total",
    "session_panes_done",
    "session_panes_remaining",
    "session_pane_progress",
    "overall_percent",
    "summary",
    "bar",
    "pane_bar",
    "window_bar",
    "status_icon",
];

impl Template {
    pub(super) fn new(format: &str) -> Self {
        let mut text = match format {
            "default" => "Loading workspace: {session} {bar} {progress} {window}",
            "minimal" => "Loading workspace: {session} [{window_progress}]",
            "window" => "Loading workspace: {session} {window_bar} {window_progress_rel}",
            "pane" => "Loading workspace: {session} {pane_bar} {session_pane_progress}",
            "verbose" => {
                "Loading workspace: {session} [window {window_index} of {window_total} · pane {pane_index} of {pane_total}] {window}"
            }
            other => other,
        };
        let mut parts = Vec::new();
        let mut literal = String::new();
        while let Some(character) = text.chars().next() {
            if text.starts_with("{{") || text.starts_with("}}") {
                literal.push(character);
                text = &text[2..];
            } else if character == '{' && text.contains('}') {
                let end = text.find('}').unwrap_or(0);
                if let Some(name) = FIELDS.iter().find(|name| **name == &text[1..end]) {
                    if !literal.is_empty() {
                        parts.push(Part::Literal(std::mem::take(&mut literal)));
                    }
                    parts.push(Part::Field(name));
                } else {
                    literal.push_str(&text[..=end]);
                }
                text = &text[end + 1..];
            } else {
                literal.push(character);
                text = &text[character.len_utf8()..];
            }
        }
        if !literal.is_empty() {
            parts.push(Part::Literal(literal));
        }
        Self(parts)
    }

    pub(super) fn render(&self, state: &State) -> String {
        let mut result = String::new();
        for part in &self.0 {
            match part {
                Part::Literal(text) => result.push_str(text),
                Part::Field(field) => result.push_str(&state.field(field)),
            }
        }
        result
    }
}

fn fraction(done: usize, total: usize) -> String {
    if total == 0 {
        String::new()
    } else {
        format!("{done}/{total}")
    }
}

fn bar(done: usize, total: usize) -> String {
    if total == 0 {
        return String::new();
    }
    let filled = done
        .saturating_mul(10)
        .checked_div(total)
        .unwrap_or(0)
        .min(10);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(10 - filled))
}

impl State {
    fn field(&self, name: &str) -> String {
        let count = match name {
            "window_index" => self.window_index,
            "window_total" => self.windows,
            "windows_done" => self.windows_done,
            "windows_remaining" => self.windows.saturating_sub(self.windows_done),
            "pane_index" => self.pane_index,
            "pane_total" => self.panes,
            "pane_done" => self.panes_done,
            "pane_remaining" => self.panes.saturating_sub(self.panes_done),
            "session_pane_total" => self.total_panes,
            "session_panes_done" => self.total_done,
            "session_panes_remaining" => self.total_panes.saturating_sub(self.total_done),
            "overall_percent" => self
                .total_done
                .saturating_mul(100)
                .checked_div(self.total_panes)
                .unwrap_or(0),
            _ => return self.text(name),
        };
        count.to_string()
    }

    fn text(&self, name: &str) -> String {
        match name {
            "workspace_path" => self.path.clone(),
            "session" => self.session.clone(),
            "window" => self.window.clone(),
            "window_progress" if self.window_index > 0 => fraction(self.window_index, self.windows),
            "pane_progress" if self.pane_index > 0 => fraction(self.pane_index, self.panes),
            "window_progress_rel" => fraction(self.windows_done, self.windows),
            "pane_progress_rel" => fraction(self.panes_done, self.panes),
            "session_pane_progress" => fraction(self.total_done, self.total_panes),
            "bar" | "pane_bar" => bar(self.total_done, self.total_panes),
            "window_bar" => bar(self.windows_done, self.windows),
            "progress" => [
                (self.window_index, self.windows, "win"),
                (self.pane_index, self.panes, "pane"),
            ]
            .into_iter()
            .filter(|(index, total, _)| *index > 0 && *total > 0)
            .map(|(index, total, label)| format!("{index}/{total} {label}"))
            .collect::<Vec<_>>()
            .join(" · "),
            "summary" => format!("[{} win, {} panes]", self.windows_done, self.total_done),
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_preserve_unknown_fields_and_never_expand_values() {
        let state = State {
            session: "{window}".into(),
            window: "actual".into(),
            ..State::default()
        };
        assert_eq!(
            Template::new("{{{session}}} {unknown} {session!r} {pane_index:03d} {broken")
                .render(&state),
            "{{window}} {unknown} {session!r} {pane_index:03d} {broken"
        );
        for preset in ["default", "minimal", "window", "pane", "verbose"] {
            assert!(
                Template::new(preset)
                    .render(&state)
                    .starts_with("Loading workspace: {window}")
            );
        }
    }

    #[test]
    fn completion_fields_use_delivered_counts_and_unknown_totals_stay_empty() {
        let template = Template::new(
            "{window_index}/{window_total} {pane_index}/{pane_total} {window_progress_rel} {pane_progress_rel} {session_pane_progress} {overall_percent}%",
        );
        let state = State {
            window_index: 2,
            windows: 3,
            windows_done: 1,
            pane_index: 2,
            panes: 4,
            panes_done: 1,
            total_done: 3,
            total_panes: 8,
            ..State::default()
        };
        assert_eq!(template.render(&state), "2/3 2/4 1/3 1/4 3/8 37%");
        assert_eq!(
            Template::new(
                "{bar}|{pane_bar}|{window_bar}|{session_pane_progress}|{overall_percent}"
            )
            .render(&State::default()),
            "||||0"
        );
    }
}
