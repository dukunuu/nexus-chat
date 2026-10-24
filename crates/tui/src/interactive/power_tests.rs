use super::*;

fn entry(source: &str) -> Entry {
    Entry {
        session: Ok(Session::new(Document::parse(source).unwrap()).unwrap()),
        focus: 0,
        error: String::new(),
        scroll: 0,
    }
}
fn lines(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[test]
fn validated_form_snapshot_and_reviewed_payload() {
    let source = "root = Panel('Contact', [Input($email, 'Email', {required: true, email: true}), Buttons([Button('Save', @ToAssistant('Contact', {kind: 'signup'}))])])\n$email = ''";
    let mut entry = entry(source);
    let (buffer, _) = entry.canvas(50, &Theme::default(), true, true);
    let rendered = lines(&buffer);
    assert!(
        rendered.iter().any(|line| line.contains("Email")),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|line| line.contains("Required")),
        "{rendered:?}"
    );
    let (snapshot, _) = entry.canvas(50, &Theme::default(), false, true);
    let snapshot = lines(&snapshot);
    let fields: Vec<_> = snapshot
        .iter()
        .filter(|line| line.contains("Email") || line.contains("Required"))
        .map(|line| line.trim_matches([' ', '│']).trim())
        .collect();
    assert_eq!(fields, ["Email", "Required"]);
    assert!(entry.activate(1, true, true).is_none());
    assert!(entry.error.contains("Required"));
    entry
        .session
        .as_mut()
        .unwrap()
        .input("email", "ada@example.com".into())
        .unwrap();
    let message = entry.activate(1, true, true).unwrap();
    assert!(message.starts_with("UI response — Contact\nContact\ncontext and form values = "));
    let payload: serde_json::Value =
        serde_json::from_str(message.split_once("context and form values = ").unwrap().1).unwrap();
    assert_eq!(payload["context"]["kind"], "signup");
    assert_eq!(payload["state"]["email"], "ada@example.com");
    let (buffer, _) = entry.canvas(50, &Theme::default(), true, true);
    assert!(!lines(&buffer).iter().any(|line| line.contains("Required")));
}

#[test]
fn each_generated_list_snapshot() {
    let entry = entry(
        "root = Panel('People', [Hero('Team'), @Each([{name:'Ada'}, {name:'Lin'}], 'row', Text(row.name))])",
    );
    let (buffer, _) = entry.canvas(36, &Theme::default(), false, true);
    let rendered = lines(&buffer);
    let body: Vec<_> = rendered
        .iter()
        .filter(|line| line.contains("Ada") || line.contains("Lin"))
        .map(|line| line.trim_matches([' ', '│']).trim())
        .collect();
    assert_eq!(body, ["Ada", "Lin"], "{rendered:?}");
}

#[test]
fn completed_patch_updates_original_session_once_and_failed_patch_preserves_it() {
    let source = "root = Panel('Counter', [Text('Old'), Buttons([Button('Add', @Set($n, $n + 1))])])\n$n = 1";
    let base = Placement {
        row: 0,
        width: 60,
        owner: 0,
        ordinal: 0,
        height: 10,
        source: source.into(),
        complete: true,
    };
    let patch = Placement { row: 10, owner: 2, source: "edit p1-1\nroot = Panel('Counter', [Text('New'), Buttons([Button('Add', @Set($n, $n + 1))])])\n$n = 9".into(), ..base.clone() };
    let mut widgets = Widgets::default();
    widgets.register(&base.key());
    widgets
        .entries
        .get_mut(&base.key())
        .unwrap()
        .activate(0, true, true);
    widgets.reconcile_patches(&[base.clone(), patch.clone()]);
    let s = widgets
        .entries
        .get(&base.key())
        .unwrap()
        .session
        .as_ref()
        .unwrap();
    assert_eq!(s.value("n"), "2");
    assert!(s.document().plain_text().contains("New"));
    widgets
        .entries
        .get_mut(&base.key())
        .unwrap()
        .activate(0, true, true);
    widgets.reconcile_patches(&[base.clone(), patch]);
    assert_eq!(
        widgets.entries[&base.key()]
            .session
            .as_ref()
            .unwrap()
            .value("n"),
        "3"
    );
    let bad = Placement {
        owner: 4,
        source: "edit p1-1\nroot = Unknown()".into(),
        ..base.clone()
    };
    widgets.reconcile_patches(&[base.clone(), bad.clone()]);
    assert!(widgets.entries[&bad.key()].session.is_err());
    assert_eq!(
        widgets.entries[&base.key()]
            .session
            .as_ref()
            .unwrap()
            .value("n"),
        "3"
    );
}

#[test]
fn open_url_confirmation_is_visible_before_any_opener_runs() {
    let mut entry = entry(
        "root = Panel('Visit', [Buttons([Button('Visit', @OpenUrl('https://example.com'))])])",
    );
    entry.activate(0, true, true);
    let (buffer, _) = entry.canvas(90, &Theme::default(), true, true);
    assert!(
        lines(&buffer)
            .iter()
            .any(|line| line.contains("Open https://example.com? Enter confirm"))
    );
    entry
        .session
        .as_mut()
        .unwrap()
        .confirm_url(false, |_| panic!("cancelled"))
        .unwrap();
}
