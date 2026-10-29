//! Practice components: a code editor bound to string state, single-choice
//! lists that mark the right answer once picked, and quizzes that show one
//! question at a time with a revealable answer. Cursor and scroll are local
//! presentation state; only the edited text is state the panel sends.

use super::components::lang_ok;
use super::{Document, MAX_TEXT, Node, Session, binding_ok, item_count, line_ok, text_ok};
use anyhow::{Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

/// Spaces one indent level inserts.
const INDENT: usize = 4;

pub(super) const fn eight() -> u16 {
    8
}

/// One quiz question: a prompt, the controls that answer it, and an optional
/// model answer with an explanation, hidden until revealed.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    /// Prompt template.
    pub title: String,
    /// Child nodes that collect the answer.
    #[serde(default)]
    pub view: Vec<Node>,
    /// Revealable answer template.
    pub answer: Option<String>,
    /// Revealable explanation template.
    pub explain: Option<String>,
}

impl Question {
    /// Whether there is anything to reveal.
    #[must_use]
    pub fn has_answer(&self) -> bool {
        self.answer.is_some() || self.explain.is_some()
    }
    pub(super) fn rows(&self) -> u16 {
        super::components::view_rows(&self.view)
            .saturating_add(1)
            .saturating_add(if self.has_answer() { 3 } else { 0 })
    }
}

/// Cursor (a char index into the bound text), first visible line, and
/// whether keys edit the text rather than move between controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditorState {
    /// Char index of the caret.
    pub cursor: usize,
    /// First visible line.
    pub top: usize,
    /// Whether typing edits the text.
    pub editing: bool,
}

/// One editing keystroke, already decoded by the frontend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorKey {
    /// Literal text; tabs become spaces and line endings are normalized.
    Insert(String),
    /// A new line, keeping the current indent (one more after `:`/`{`/`(`/`[`).
    Newline,
    /// Spaces to the next indent stop.
    Indent,
    /// Remove one indent level from the current line.
    Outdent,
    /// Delete before the caret; inside leading spaces, back to the previous stop.
    Backspace,
    /// Delete after the caret.
    Delete,
    /// Caret one char left.
    Left,
    /// Caret one char right.
    Right,
    /// Caret one line up, keeping the column where it can.
    Up,
    /// Caret one line down, keeping the column where it can.
    Down,
    /// Caret to the first non-space char, or the line start from there.
    Home,
    /// Caret to the line end.
    End,
}

/// Editor text must fit the panel's text budget and hold no terminal controls.
pub(super) fn code_ok(value: &Value) -> Result<()> {
    let text = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("editor state must be a string"))?;
    text_ok(text, MAX_TEXT)
}

/// `(line, column)` of a char index, both in chars.
#[must_use]
pub fn line_col(text: &str, cursor: usize) -> (usize, usize) {
    let mut line = 0;
    let mut column = 0;
    for ch in text.chars().take(cursor) {
        if ch == '\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    (line, column)
}

/// Char index of `(line, column)`, with the column clamped to the line.
fn offset(text: &str, line: usize, column: usize) -> usize {
    let mut index = 0;
    for (number, content) in text.split('\n').enumerate() {
        let length = content.chars().count();
        if number == line {
            return index + column.min(length);
        }
        index += length + 1;
    }
    text.chars().count()
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', &" ".repeat(INDENT))
        .chars()
        .filter(|ch| *ch == '\n' || !ch.is_control())
        .collect()
}

/// The edit applied to `text`: the new text and caret.
fn apply(text: &str, cursor: usize, key: &EditorKey) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let (line, column) = line_col(text, cursor);
    let line_text: String = text.split('\n').nth(line).unwrap_or_default().to_string();
    let leading = line_text.chars().take_while(|ch| *ch == ' ').count();
    let splice = |at: usize, remove: usize, insert: &str| {
        let mut out: String = chars[..at].iter().collect();
        out.push_str(insert);
        out.extend(&chars[(at + remove).min(chars.len())..]);
        out
    };
    match key {
        EditorKey::Insert(insert) => {
            let insert = normalize(insert);
            (splice(cursor, 0, &insert), cursor + insert.chars().count())
        }
        EditorKey::Newline => {
            let opens = cursor > 0 && matches!(chars[cursor - 1], ':' | '{' | '(' | '[');
            let indent = leading.min(column) + if opens { INDENT } else { 0 };
            let insert = format!("\n{}", " ".repeat(indent));
            (splice(cursor, 0, &insert), cursor + 1 + indent)
        }
        EditorKey::Indent => {
            let spaces = INDENT - column % INDENT;
            (splice(cursor, 0, &" ".repeat(spaces)), cursor + spaces)
        }
        EditorKey::Outdent => {
            let remove = leading.min(match leading % INDENT {
                0 => INDENT,
                partial => partial,
            });
            let start = cursor - column;
            (splice(start, remove, ""), cursor - remove.min(column))
        }
        EditorKey::Backspace if cursor == 0 => (text.to_string(), 0),
        EditorKey::Backspace => {
            let remove = if column > 0 && column <= leading {
                match column % INDENT {
                    0 => INDENT,
                    partial => partial,
                }
            } else {
                1
            };
            (splice(cursor - remove, remove, ""), cursor - remove)
        }
        EditorKey::Delete => (splice(cursor, 1, ""), cursor),
        EditorKey::Left => (text.to_string(), cursor.saturating_sub(1)),
        EditorKey::Right => (text.to_string(), (cursor + 1).min(chars.len())),
        EditorKey::Up if line == 0 => (text.to_string(), 0),
        EditorKey::Up => (text.to_string(), offset(text, line - 1, column)),
        EditorKey::Down => {
            let last = text.split('\n').count() - 1;
            if line == last {
                (text.to_string(), chars.len())
            } else {
                (text.to_string(), offset(text, line + 1, column))
            }
        }
        EditorKey::Home => {
            let target = if column == leading { 0 } else { leading };
            (text.to_string(), cursor - column + target)
        }
        EditorKey::End => (
            text.to_string(),
            cursor - column + line_text.chars().count(),
        ),
    }
}

impl Document {
    pub(super) fn validate_practice(&self, node: &Node) -> Result<usize> {
        Ok(match node {
            Node::Editor {
                key,
                label,
                lang,
                rows,
            } => {
                line_ok(label, 120)?;
                Self::template_ok(label)?;
                if let Some(lang) = lang {
                    lang_ok(lang)?;
                }
                ensure!((3..=16).contains(rows), "editor rows must be 3–16");
                binding_ok(&self.state, key, |value| code_ok(value).is_ok())?;
                1
            }
            Node::Choice {
                key,
                label,
                options,
                answer,
            } => {
                Self::template_ok(label)?;
                item_count(options.len(), 2, 8)?;
                for option in options {
                    line_ok(option, 120)?;
                    Self::template_ok(option)?;
                }
                if let Some(answer) = answer {
                    ensure!(
                        options.contains(answer),
                        "choice answer must be one of its options"
                    );
                }
                binding_ok(&self.state, key, |value| {
                    value
                        .as_str()
                        .is_some_and(|text| text.is_empty() || options.iter().any(|o| o == text))
                })?;
                options.len()
            }
            Node::Quiz { items, key } => {
                item_count(items.len(), 1, 12)?;
                for item in items {
                    line_ok(&item.title, 1024)?;
                    Self::template_ok(&item.title)?;
                    for text in item.answer.iter().chain(&item.explain) {
                        Self::template_ok(text)?;
                    }
                }
                if let Some(key) = key {
                    binding_ok(&self.state, key, Value::is_number)?;
                }
                2
            }
            _ => bail!("not a practice component"),
        })
    }
}

impl Session {
    /// Caret, scroll and mode of an editor.
    #[must_use]
    pub fn editor(&self, node: usize) -> EditorState {
        self.components
            .editors
            .get(&node)
            .copied()
            .unwrap_or_default()
    }
    fn editor_binding(&self, node: usize) -> Result<(String, usize)> {
        let Some(Node::Editor { key, rows, .. }) = self.document.node(node) else {
            bail!("not an editor");
        };
        Ok((key.clone(), usize::from(*rows)))
    }
    /// Start or stop editing; starting puts the caret at the end the first time.
    /// # Errors
    /// Rejects non-editors.
    pub fn set_editing(&mut self, node: usize, editing: bool) -> Result<()> {
        let (key, rows) = self.editor_binding(node)?;
        let text = self.value(&key);
        let state = self
            .components
            .editors
            .entry(node)
            .or_insert_with(|| EditorState {
                cursor: text.chars().count(),
                ..EditorState::default()
            });
        state.editing = editing;
        Self::follow(state, &text, rows);
        Ok(())
    }
    /// Put the caret at a visible `(row, column)` and start editing.
    /// # Errors
    /// Rejects non-editors.
    pub fn place_cursor(&mut self, node: usize, row: usize, column: usize) -> Result<()> {
        let (key, rows) = self.editor_binding(node)?;
        let text = self.value(&key);
        let state = self.components.editors.entry(node).or_default();
        state.cursor = offset(&text, state.top + row, column);
        state.editing = true;
        Self::follow(state, &text, rows);
        Ok(())
    }
    /// Apply one editing key transactionally; the text stays valid state.
    /// # Errors
    /// Rejects non-editors and text past the 4 KiB budget.
    pub fn edit_code(&mut self, node: usize, key: &EditorKey) -> Result<()> {
        let (field, rows) = self.editor_binding(node)?;
        let text = self.value(&field);
        let state = self.editor(node);
        let (next, cursor) = apply(&text, state.cursor, key);
        if next != text {
            ensure!(next.len() <= MAX_TEXT, "code is limited to 4 KiB");
            let mut state = self.state.clone();
            state.insert(field.clone(), Value::String(next.clone()));
            self.bindings_ok(&mut state)?;
            self.commit(state)?;
        }
        let state = self.components.editors.entry(node).or_default();
        state.cursor = cursor;
        state.editing = true;
        Self::follow(state, &next, rows);
        Ok(())
    }
    fn follow(state: &mut EditorState, text: &str, rows: usize) {
        state.cursor = state.cursor.min(text.chars().count());
        let (line, _) = line_col(text, state.cursor);
        if line < state.top {
            state.top = line;
        } else if line >= state.top + rows {
            state.top = line + 1 - rows;
        }
    }
    /// Pick one option of a choice list.
    /// # Errors
    /// Rejects non-choices and unknown options.
    pub fn choose(&mut self, node: usize, option: usize) -> Result<()> {
        let Some(Node::Choice { key, options, .. }) = self.document.node(node) else {
            bail!("not a choice");
        };
        let value = options
            .get(option)
            .ok_or_else(|| anyhow::anyhow!("unknown option"))?;
        let mut next = self.state.clone();
        next.insert(key.clone(), json!(value));
        self.bindings_ok(&mut next)?;
        self.commit(next)
    }
    /// Current question index, normalized against the question count.
    #[must_use]
    pub fn question(&self, node: usize) -> usize {
        let Some(Node::Quiz { items, key }) = self.document.node(node) else {
            return 0;
        };
        key.as_ref()
            .map_or_else(
                || self.components.quiz.get(&node).copied().unwrap_or(0),
                |key| self.selected_row(key),
            )
            .min(items.len().saturating_sub(1))
    }
    /// Show another question, updating its optional binding transactionally.
    /// # Errors
    /// Rejects non-quizzes and invalid resulting bindings.
    pub fn select_question(&mut self, node: usize, question: usize) -> Result<()> {
        let Some(Node::Quiz { items, key }) = self.document.node(node) else {
            bail!("not a quiz");
        };
        let question = question.min(items.len().saturating_sub(1));
        if let Some(key) = key {
            let mut next = self.state.clone();
            next.insert(key.clone(), json!(question));
            self.bindings_ok(&mut next)?;
            self.commit(next)?;
        } else {
            self.components.quiz.insert(node, question);
            self.revision += 1;
        }
        Ok(())
    }
    /// Whether the current question's answer is showing.
    #[must_use]
    pub fn revealed(&self, node: usize) -> bool {
        self.components
            .revealed
            .contains(&(node, self.question(node)))
    }
    /// Show or hide the current question's answer.
    /// # Errors
    /// Rejects questions without an answer or explanation.
    pub fn reveal(&mut self, node: usize) -> Result<()> {
        let question = self.question(node);
        ensure!(
            matches!(self.document.node(node), Some(Node::Quiz { items, .. }) if items.get(question).is_some_and(Question::has_answer)),
            "this question has no answer to show"
        );
        if !self.components.revealed.remove(&(node, question)) {
            self.components.revealed.insert((node, question));
        }
        self.revision += 1;
        Ok(())
    }
    /// Right picks out of answered choice lists that declare an answer, and
    /// how many such lists the quiz holds; `None` when it holds none.
    #[must_use]
    pub fn quiz_score(&self, node: usize) -> Option<(usize, usize)> {
        let Some(Node::Quiz { items, .. }) = self.document.node(node) else {
            return None;
        };
        let mut nodes = Vec::new();
        for item in items {
            super::components::walk(&item.view, &mut nodes);
        }
        let graded: Vec<_> = nodes
            .into_iter()
            .filter_map(|node| match node {
                Node::Choice {
                    key,
                    answer: Some(answer),
                    ..
                } => Some(self.value(key) == *answer),
                _ => None,
            })
            .collect();
        (!graded.is_empty()).then(|| (graded.iter().filter(|right| **right).count(), graded.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, cursor: usize, keys: &[EditorKey]) -> (String, usize) {
        keys.iter()
            .fold((text.to_string(), cursor), |(text, cursor), key| {
                apply(&text, cursor, key)
            })
    }

    #[test]
    fn typing_indents_after_openers_and_backspace_removes_whole_indents() {
        let (text, cursor) = run(
            "",
            0,
            &[
                EditorKey::Insert("def f():".into()),
                EditorKey::Newline,
                EditorKey::Insert("return 1".into()),
            ],
        );
        assert_eq!(text, "def f():\n    return 1");
        assert_eq!(cursor, text.chars().count());
        let (text, cursor) = run(&text, 13, &[EditorKey::Backspace]);
        assert_eq!((text.as_str(), cursor), ("def f():\nreturn 1", 9));
        let (text, _) = run("a\tb\r\nc", 0, &[EditorKey::Insert("x\ty\r\n".into())]);
        assert_eq!(text, "x    y\na\tb\r\nc");
    }

    #[test]
    fn tab_and_shift_tab_move_to_indent_stops() {
        assert_eq!(run("ab", 2, &[EditorKey::Indent]), ("ab  ".into(), 4));
        assert_eq!(
            run("      x", 7, &[EditorKey::Outdent]),
            ("    x".into(), 5)
        );
        assert_eq!(
            run("    x", 5, &[EditorKey::Outdent, EditorKey::Outdent]),
            ("x".into(), 1)
        );
    }

    #[test]
    fn caret_moves_keep_columns_and_home_toggles_indent() {
        let text = "first line\nab\n    third";
        let end_of_first = 10;
        assert_eq!(apply(text, end_of_first, &EditorKey::Down).1, 13);
        assert_eq!(apply(text, 13, &EditorKey::Down).1, 16);
        assert_eq!(apply(text, 16, &EditorKey::Up).1, 13);
        assert_eq!(apply(text, 0, &EditorKey::Up).1, 0);
        let third = 14;
        assert_eq!(apply(text, third + 7, &EditorKey::Home).1, third + 4);
        assert_eq!(apply(text, third + 4, &EditorKey::Home).1, third);
        assert_eq!(apply(text, third, &EditorKey::End).1, third + 9);
        assert_eq!(
            apply(text, third + 9, &EditorKey::Down).1,
            text.chars().count()
        );
        assert_eq!(line_col(text, third + 2), (2, 2));
    }

    fn session(source: &str) -> Session {
        Session::new(Document::parse(source).unwrap()).unwrap()
    }

    #[test]
    fn editors_edit_bound_state_and_scroll_to_the_caret() {
        let mut ui = session(
            "root = Panel(\"Practice\", [code])\n$code = \"fn main() {\"\ncode = Editor($code, \"Solution\", \"rust\", 3)",
        );
        let node = 0;
        assert!(!ui.editor(node).editing);
        ui.set_editing(node, true).unwrap();
        assert_eq!(ui.editor(node).cursor, 11);
        for key in [
            EditorKey::Newline,
            EditorKey::Insert("let a = 1;\nlet b = 2;\nlet c = 3;".into()),
            EditorKey::Newline,
            EditorKey::Insert("}".into()),
        ] {
            ui.edit_code(node, &key).unwrap();
        }
        assert_eq!(
            ui.value("code"),
            "fn main() {\n    let a = 1;\nlet b = 2;\nlet c = 3;\n}"
        );
        assert_eq!(ui.editor(node).top, 2);
        ui.place_cursor(node, 0, 99).unwrap();
        assert_eq!(line_col(&ui.value("code"), ui.editor(node).cursor), (2, 10));
        let huge = EditorKey::Insert("x".repeat(MAX_TEXT));
        assert!(ui.edit_code(node, &huge).is_err());
        assert!(ui.value("code").len() < MAX_TEXT);
        assert!(ui.set_editing(5, true).is_err());
    }

    #[test]
    fn quizzes_page_reveal_and_score_their_choices() {
        let mut ui = session(
            r#"root = Panel("Interview", [quiz])
$q1 = ""
$q2 = ""
$code = ""
quiz = Quiz([
  Question("Which is O(1)?", [Choice($q1, "Pick one", ["Hash lookup", "Sort"], "Hash lookup")], "Hash lookup", "Average case."),
  Question("Reverse a list", [Editor($code, "Python", "python")]),
  Question("Stack or queue?", [Choice($q2, "Pick", ["Stack", "Queue"], "Stack")])
])"#,
        );
        let quiz = 0;
        assert_eq!(ui.quiz_score(quiz), Some((0, 2)));
        assert_eq!(ui.document().control_count(), 2 + 2 + 1 + 2);
        let visible: Vec<_> = ui.visible_nodes().into_iter().map(|(i, _)| i).collect();
        assert_eq!(visible, [0, 1]);
        ui.choose(1, 0).unwrap();
        assert_eq!(ui.quiz_score(quiz), Some((1, 2)));
        assert!(ui.choose(1, 5).is_err());
        assert!(!ui.revealed(quiz));
        ui.reveal(quiz).unwrap();
        assert!(ui.revealed(quiz));
        ui.select_question(quiz, 1).unwrap();
        assert!(!ui.revealed(quiz));
        assert!(ui.reveal(quiz).is_err(), "nothing to reveal");
        let visible: Vec<_> = ui.visible_nodes().into_iter().map(|(i, _)| i).collect();
        assert_eq!(visible, [0, 2]);
        ui.select_question(quiz, 99).unwrap();
        assert_eq!(ui.question(quiz), 2);
        ui.choose(3, 1).unwrap();
        assert_eq!(ui.quiz_score(quiz), Some((1, 2)));
        assert!(
            ui.document()
                .plain_text()
                .lines()
                .any(|line| line == "Reverse a list")
        );
    }

    #[test]
    fn practice_components_report_bad_answers_bindings_and_sizes() {
        for (component, state) in [
            ("Choice($v, \"Pick\", [\"A\", \"B\"], \"C\")", "$v = \"\""),
            ("Choice($v, \"Pick\", [\"A\", \"B\"])", "$v = \"Z\""),
            ("Choice($v, \"Pick\", [\"A\"])", "$v = \"\""),
            ("Editor($v, \"Code\", \"rust\", 40)", "$v = \"\""),
            ("Editor($v, \"Code\", \"no spaces\")", "$v = \"\""),
            ("Editor($v, \"Code\")", "$v = 3"),
            ("Quiz([])", "$v = 0"),
        ] {
            let source = format!("root = Panel(\"Bad\", [{component}])\n{state}");
            assert!(Document::check(&source).is_err(), "{component}");
        }
    }
}
