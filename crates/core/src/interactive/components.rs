//! Additional bounded components and their local interaction state.

use super::{
    Document, MAX_TEXT, MAX_VALUE, Node, Session, Step, Tone, binding_ok, item_count, line_ok,
    spend, text_ok,
};
use anyhow::{Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// One labelled tab and its child view.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tab {
    /// Label template.
    pub label: String,
    /// Child nodes.
    pub view: Vec<Node>,
}

/// One expandable section.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    /// Heading template.
    pub title: String,
    /// Child nodes.
    pub view: Vec<Node>,
    /// Initial expansion state.
    #[serde(default)]
    pub open: bool,
}

/// Supported terminal plot geometries.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChartKind {
    /// Connected samples.
    Line,
    /// Filled samples.
    Area,
    /// Individual samples.
    Scatter,
    /// Vertical bars.
    Bar,
    /// A ring showing series totals.
    Pie,
}

/// A named, themed numeric series.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChartSeries {
    /// Legend template.
    pub name: String,
    /// Numeric expressions.
    pub values: Vec<Value>,
    /// Theme role.
    #[serde(default = "accent")]
    pub tone: Tone,
}
const fn accent() -> Tone {
    Tone::Accent
}
pub(super) const fn five() -> u16 {
    5
}

/// One event on a timeline.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineEvent {
    /// Time template.
    pub time: String,
    /// Title template.
    pub title: String,
    /// Optional description template.
    pub text: Option<String>,
    /// Theme role.
    #[serde(default = "accent")]
    pub tone: Tone,
}

pub(super) fn view_rows(nodes: &[Node]) -> u16 {
    nodes
        .iter()
        .fold(0u16, |rows, node| rows.saturating_add(node.rows()))
}
fn children(node: &Node) -> Vec<&[Node]> {
    match node {
        Node::Tabs { items, .. } => items.iter().map(|item| item.view.as_slice()).collect(),
        Node::Accordion { items } => items.iter().map(|item| item.view.as_slice()).collect(),
        Node::Form { fields, .. } => vec![fields],
        _ => Vec::new(),
    }
}
pub(super) fn max_visible(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|node| {
            1 + match node {
                Node::Tabs { items, .. } => items
                    .iter()
                    .map(|item| max_visible(&item.view))
                    .max()
                    .unwrap_or(0),
                _ => children(node).into_iter().map(max_visible).sum(),
            }
        })
        .sum()
}
pub(super) fn tree_ok(nodes: &[Node], depth: usize, budget: &mut usize) -> Result<()> {
    ensure!(depth < 8, "component nesting exceeds eight levels");
    for node in nodes {
        spend(budget)?;
        for branch in children(node) {
            tree_ok(branch, depth + 1, budget)?;
        }
    }
    Ok(())
}
fn walk<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
    for node in nodes {
        out.push(node);
        for branch in children(node) {
            walk(branch, out);
        }
    }
}

impl Document {
    /// All nodes in stable preorder, including hidden child views.
    #[must_use]
    pub fn all_nodes(&self) -> Vec<&Node> {
        let mut nodes = Vec::new();
        walk(&self.view, &mut nodes);
        nodes
    }
    /// Resolve a stable preorder node index.
    #[must_use]
    pub fn node(&self, index: usize) -> Option<&Node> {
        self.all_nodes().get(index).copied()
    }
    /// Stable index of a node borrowed from this document.
    #[must_use]
    pub fn node_index(&self, node: &Node) -> usize {
        self.all_nodes()
            .iter()
            .position(|candidate| std::ptr::eq(*candidate, node))
            .expect("document node")
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn validate_new_component(
        &self,
        node: &Node,
        budget: &mut usize,
    ) -> Result<Option<usize>> {
        let mut controls = 0;
        match node {
            Node::Tabs { items, key } => {
                item_count(items.len(), 1, 8)?;
                for item in items {
                    Self::template_ok(&item.label)?;
                }
                if let Some(key) = key {
                    binding_ok(&self.state, key, Value::is_number)?;
                }
                controls = 1;
            }
            Node::Accordion { items } => {
                item_count(items.len(), 1, 8)?;
                for item in items {
                    Self::template_ok(&item.title)?;
                }
                controls = items.len();
            }
            Node::Form {
                title,
                fields,
                submit_label,
                action,
            } => {
                Self::template_ok(title)?;
                line_ok(submit_label, 120)?;
                Self::template_ok(submit_label)?;
                item_count(fields.len(), 1, 8)?;
                ensure!(
                    fields.iter().all(|field| matches!(
                        field,
                        Node::Input { .. }
                            | Node::Select { .. }
                            | Node::Stepper { .. }
                            | Node::Toggle { .. }
                            | Node::Checklist { .. }
                            | Node::Slider { .. }
                            | Node::Rating { .. }
                    )),
                    "form fields must be bound controls"
                );
                ensure!(
                    self.actions.contains_key(action),
                    "unknown form action: {action}"
                );
                ensure!(
                    !self.actions.get(action).is_some_and(|steps| steps
                        .iter()
                        .any(|step| matches!(step, Step::Answer { .. }))),
                    "form actions cannot answer"
                );
                controls = 1;
            }
            Node::Chart {
                kind,
                series,
                labels,
                rows,
            } => {
                item_count(series.len(), 1, 6)?;
                ensure!((4..=12).contains(rows), "chart rows must be 4–12");
                item_count(labels.len(), 0, 64)?;
                for label in labels {
                    line_ok(label, 120)?;
                    Self::template_ok(label)?;
                }
                for item in series {
                    Self::template_ok(&item.name)?;
                    item_count(item.values.len(), 1, 64)?;
                    for value in &item.values {
                        self.numeric_ok(value, budget)?;
                        if *kind == ChartKind::Pie
                            && let Some(value) = value.as_f64()
                        {
                            ensure!(value >= 0.0, "pie values must be nonnegative");
                        }
                    }
                }
            }
            Node::Timeline { items } => {
                item_count(items.len(), 1, 8)?;
                for item in items {
                    Self::template_ok(&item.time)?;
                    Self::template_ok(&item.title)?;
                    if let Some(text) = &item.text {
                        Self::template_ok(text)?;
                    }
                }
            }
            Node::Rating { key, label, max } => {
                Self::template_ok(label)?;
                ensure!((1..=10).contains(max), "rating max must be 1–10");
                binding_ok(&self.state, key, Value::is_number)?;
                controls = 1;
            }
            Node::Code { source, lang } => {
                text_ok(source, MAX_TEXT)?;
                ensure!(
                    source.lines().count() <= 8,
                    "code needs at most eight lines"
                );
                if let Some(lang) = lang {
                    ensure!(
                        lang.len() <= 32
                            && lang
                                .bytes()
                                .all(|c| c.is_ascii_alphanumeric()
                                    || matches!(c, b'_' | b'-' | b'+')),
                        "invalid code language token"
                    );
                }
                controls = 1;
            }
            Node::Badge { text, .. } => Self::template_ok(text)?,
            Node::Meter {
                label, value, max, ..
            } => {
                Self::template_ok(label)?;
                self.numeric_ok(value, budget)?;
                self.numeric_ok(max, budget)?;
            }
            _ => return Ok(None),
        }
        for branch in children(node) {
            item_count(branch.len(), 0, 16)?;
            for child in branch {
                controls += self.validate_node(child, budget)?;
            }
        }
        Ok(Some(controls))
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct ComponentState {
    tabs: BTreeMap<usize, usize>,
    open: BTreeMap<(usize, usize), bool>,
    tables: BTreeMap<usize, TableState>,
    copy: Option<String>,
}
/// Local table presentation state; selected rows remain original data indices.
#[derive(Clone, Debug, Default)]
pub struct TableState {
    /// Sort column and descending flag.
    pub sort: Option<(usize, bool)>,
    /// Zero-based page.
    pub page: usize,
    /// Plain-text filter.
    pub query: String,
    /// Whether typing edits the filter.
    pub searching: bool,
}

impl Session {
    /// Queue literal code for the frontend's existing clipboard helper.
    pub fn copy_code(&mut self, node: usize) {
        if let Some(Node::Code { source, .. }) = self.document.node(node) {
            self.components.copy = Some(source.clone());
        }
    }
    /// Take a pending code copy without accessing the operating system.
    pub fn take_copy(&mut self) -> Option<String> {
        self.components.copy.take()
    }
    /// Current tab index, normalized against the child count.
    #[must_use]
    pub fn tab(&self, node: usize) -> usize {
        let Some(Node::Tabs { items, key }) = self.document.node(node) else {
            return 0;
        };
        key.as_ref()
            .map_or_else(
                || self.components.tabs.get(&node).copied().unwrap_or(0),
                |key| self.selected_row(key),
            )
            .min(items.len().saturating_sub(1))
    }
    /// Switch to a tab, updating its optional binding transactionally.
    /// # Errors
    /// Rejects non-tabs and invalid resulting bindings.
    pub fn select_tab(&mut self, node: usize, tab: usize) -> Result<()> {
        let Some(Node::Tabs { items, key }) = self.document.node(node) else {
            bail!("not tabs");
        };
        let tab = tab.min(items.len().saturating_sub(1));
        if let Some(key) = key {
            let mut next = self.state.clone();
            next.insert(key.clone(), json!(tab));
            self.bindings_ok(&mut next)?;
            self.state = next;
        } else {
            self.components.tabs.insert(node, tab);
        }
        Ok(())
    }
    /// Current section expansion state.
    #[must_use]
    pub fn section_open(&self, node: usize, section: usize) -> bool {
        self.components.open.get(&(node, section)).copied().unwrap_or_else(|| matches!(self.document.node(node), Some(Node::Accordion { items }) if items.get(section).is_some_and(|s| s.open)))
    }
    /// Toggle a declared section.
    /// # Errors
    /// Rejects missing sections.
    pub fn expand_section(&mut self, node: usize, section: usize) -> Result<()> {
        ensure!(
            matches!(self.document.node(node), Some(Node::Accordion { items }) if section < items.len()),
            "not a section"
        );
        self.components
            .open
            .insert((node, section), !self.section_open(node, section));
        Ok(())
    }
    /// Visible nodes in preorder, retaining stable indices for hidden branches.
    #[must_use]
    pub fn visible_nodes(&self) -> Vec<(usize, &Node)> {
        fn visit<'a>(session: &'a Session, nodes: &'a [Node], out: &mut Vec<(usize, &'a Node)>) {
            for node in nodes {
                let index = session.document.node_index(node);
                out.push((index, node));
                match node {
                    Node::Tabs { items, .. } => {
                        if let Some(item) = items.get(session.tab(index)) {
                            visit(session, &item.view, out);
                        }
                    }
                    Node::Accordion { items } => {
                        for (section, item) in items.iter().enumerate() {
                            if session.section_open(index, section) {
                                visit(session, &item.view, out);
                            }
                        }
                    }
                    Node::Form { fields, .. } => visit(session, fields, out),
                    _ => {}
                }
            }
        }
        let mut nodes = Vec::new();
        visit(self, &self.document.view, &mut nodes);
        nodes
    }
    /// Whether all fields permit form submission.
    #[must_use]
    pub fn form_valid(&self, node: usize) -> bool {
        let Some(Node::Form { fields, .. }) = self.document.node(node) else {
            return false;
        };
        fields.iter().all(|field| match field {
            Node::Input { key, rules, .. } => {
                super::validation::error(rules, &self.value(key)).is_none()
            }
            _ => true,
        })
    }
    /// Submit current bound values through the form's action.
    /// # Errors
    /// Rejects incomplete forms or failed actions without changing state.
    pub fn submit_form(&mut self, node: usize) -> Result<Option<String>> {
        ensure!(
            self.form_valid(node),
            "complete required fields before submitting"
        );
        let Some(Node::Form { action, .. }) = self.document.node(node) else {
            bail!("not a form");
        };
        let action = action.clone();
        self.activate(&action)
    }
    /// Set a rating, clamped to its declared scale.
    /// # Errors
    /// Rejects non-ratings or invalid resulting bindings.
    pub fn rate(&mut self, node: usize, value: u16) -> Result<()> {
        let Some(Node::Rating { key, max, .. }) = self.document.node(node) else {
            bail!("not a rating");
        };
        let mut next = self.state.clone();
        next.insert(key.clone(), json!(value.min(*max)));
        self.bindings_ok(&mut next)?;
        self.state = next;
        Ok(())
    }
    /// Set a slider from a pointer fraction, snapping to its step.
    /// # Errors
    /// Rejects non-sliders and invalid resulting bindings.
    pub fn slide(&mut self, node: usize, fraction: f64) -> Result<()> {
        let Some(Node::Slider {
            key,
            min,
            max,
            step,
            ..
        }) = self.document.node(node)
        else {
            bail!("not a slider");
        };
        let value = ((fraction.clamp(0.0, 1.0) * (max - min) / step).round() * step + min)
            .clamp(*min, *max);
        let mut next = self.state.clone();
        next.insert(key.clone(), json!(value));
        self.bindings_ok(&mut next)?;
        self.state = next;
        Ok(())
    }
    pub(super) fn follow_table_row(&mut self, node: usize, page: usize) {
        self.components.tables.entry(node).or_default().page = page;
    }
    /// Current local table presentation state.
    #[must_use]
    pub fn table_state(&self, node: usize) -> TableState {
        self.components
            .tables
            .get(&node)
            .cloned()
            .unwrap_or_default()
    }
    /// Filtered and sorted original row indices, before pagination.
    #[must_use]
    pub fn table_rows(&self, node: usize) -> Vec<usize> {
        let Some(Node::Table { rows_data, .. }) = self.document.node(node) else {
            return Vec::new();
        };
        let state = self.table_state(node);
        let query = state.query.to_lowercase();
        let text = |cell: &str| self.text(cell).unwrap_or_default();
        let mut rows: Vec<_> = (0..rows_data.len())
            .filter(|row| {
                rows_data[*row]
                    .iter()
                    .any(|cell| text(cell).to_lowercase().contains(&query))
            })
            .collect();
        if let Some((column, descending)) = state.sort {
            rows.sort_by(|a, b| {
                let a = text(&rows_data[*a][column]);
                let b = text(&rows_data[*b][column]);
                let order = match (a.parse::<f64>(), b.parse::<f64>()) {
                    (Ok(a), Ok(b)) => a.total_cmp(&b),
                    _ => a.cmp(&b),
                };
                if descending { order.reverse() } else { order }
            });
        }
        rows
    }
    /// Sort by a column, toggling direction on a repeated selection.
    /// # Errors
    /// Rejects tables without sorting or unknown columns.
    pub fn sort_table(&mut self, node: usize, column: usize) -> Result<()> {
        ensure!(
            matches!(self.document.node(node), Some(Node::Table { columns, sortable: true, .. }) if column < columns.len()),
            "not a sortable column"
        );
        let state = self.components.tables.entry(node).or_default();
        state.sort = Some((
            column,
            state
                .sort
                .is_some_and(|(old, descending)| old == column && !descending),
        ));
        state.page = 0;
        Ok(())
    }
    /// Change the table page, clamped to filtered data.
    /// # Errors
    /// Rejects tables without pagination.
    pub fn page_table(&mut self, node: usize, forward: bool) -> Result<()> {
        let Some(Node::Table {
            page_size: Some(size),
            ..
        }) = self.document.node(node)
        else {
            bail!("not a paginated table");
        };
        let max = self
            .table_rows(node)
            .len()
            .div_ceil(*size)
            .saturating_sub(1);
        let state = self.components.tables.entry(node).or_default();
        state.page = if forward {
            state.page.saturating_add(1).min(max)
        } else {
            state.page.saturating_sub(1)
        };
        Ok(())
    }
    /// Edit the local table filter without changing bound selection.
    /// # Errors
    /// Rejects tables without search or invalid text.
    pub fn search_table(&mut self, node: usize, query: String, editing: bool) -> Result<()> {
        ensure!(
            matches!(
                self.document.node(node),
                Some(Node::Table { search: true, .. })
            ),
            "not a searchable table"
        );
        line_ok(&query, MAX_VALUE)?;
        let state = self.components.tables.entry(node).or_default();
        state.query = query;
        state.searching = editing;
        state.page = 0;
        Ok(())
    }
}
