use super::*;
use crate::interactive::{Node, Session};

const SOURCE: &str = r#"root = Panel("Trip planner", [heading, days, progress, save], "rounded", "accent")
$days = 5
heading = Hero("Make room for adventure", "A plan that moves with you", "YOUR NEXT TRIP")
days = Stepper($days, "Days away", 1, 30, 1)
progress = Progress("Planned days", $days * 2, 30)
save = Buttons([Button("Save plan", @ToAssistant("Plan " + $days + " days"), true)])"#;

#[test]
fn positional_calls_references_state_and_actions_share_the_validated_runtime() {
    let document = Document::check(SOURCE).unwrap();
    assert_eq!(document.title(), "Trip planner");
    assert_eq!(document.nodes().len(), 4);
    assert_eq!(document.control_count(), 2);
    let mut session = Session::new(document).unwrap();
    session.step(1, true).unwrap();
    assert_eq!(
        session.activate_button(3, 0).unwrap(),
        Some("UI response — Trip planner\nPlan 6 days".into())
    );
    let Node::Progress { value, max, .. } = &session.document().nodes()[2] else {
        panic!("progress")
    };
    assert_eq!(session.progress(value, max).unwrap(), (12.0, 30.0));
}

#[test]
fn forward_references_fill_the_root_in_authored_order_not_arrival_order() {
    let mut parser = StreamParser::default();
    let root = "root = Panel(\"Live\", [first, second])\n";
    assert!(parser.set(root).is_none());
    let second = format!("{root}second = Text(\"Second\")\n");
    assert_eq!(parser.set(&second).unwrap().nodes().len(), 1);
    let both = format!("{second}first = Text(\"First\")\norphan = Text(\"Hidden\")\n");
    let session = Session::new(parser.set(&both).unwrap()).unwrap();
    let Node::Text { text, .. } = &session.document().nodes()[0] else {
        panic!("text")
    };
    assert_eq!(session.text(text).unwrap(), "First");
    assert_eq!(session.document().nodes().len(), 2);
    assert_eq!(parser.symbols.len(), 4);
    assert_eq!(parser.consumed, both.len());
}

#[test]
fn incomplete_strings_stream_unicode_text_and_calls_before_closing() {
    let source = "root = Panel(\"Live\", [heading])\nheading = Hero(\"Café 東京";
    let doc = StreamParser::default().set(source).unwrap();
    let Node::Hero { title, .. } = &doc.nodes()[0] else {
        panic!("hero")
    };
    assert_eq!(title, "Café 東京");
    assert!(
        Document::check(source).is_err(),
        "strict final validation still rejects truncation"
    );
    let escaped = "root = Panel(\"Live\", [Text(\"\\uD83C\\uDF0D\\nhello\")])";
    let doc = Document::parse(escaped).unwrap();
    let Node::Text { text, .. } = &doc.nodes()[0] else {
        panic!("text")
    };
    assert_eq!(text, "🌍\nhello");
}

#[test]
fn every_utf8_prefix_is_safe_and_stream_end_matches_batch_parse() {
    let mut parser = StreamParser::default();
    for end in SOURCE
        .char_indices()
        .map(|(index, _)| index)
        .chain([SOURCE.len()])
    {
        let _ = parser.set(&SOURCE[..end]);
    }
    let streamed = parser.set(SOURCE).unwrap();
    let batch = Document::parse(SOURCE).unwrap();
    assert_eq!(streamed.title(), batch.title());
    assert_eq!(streamed.nodes().len(), batch.nodes().len());
    assert_eq!(streamed.rows(), batch.rows());
}

#[test]
fn incomplete_redefinitions_do_not_replace_completed_statements() {
    let prefix = "root = Panel(\"Live\", [text])\ntext = Text(\"Original\")\n";
    let mut parser = StreamParser::default();
    let pending = format!("{prefix}text = Text(\"Replac");
    let doc = parser.set(&pending).unwrap();
    assert!(matches!(&doc.nodes()[0], Node::Text { text, .. } if text == "Original"));
    let complete = format!("{pending}ement\")");
    let doc = parser.set(&complete).unwrap();
    assert!(matches!(&doc.nodes()[0], Node::Text { text, .. } if text == "Replacement"));
    let replaced = "root = Panel(\"Other\", [Text(\"Reset\")])";
    assert_eq!(parser.set(replaced).unwrap().title(), "Other");
    assert!(!parser.symbols.contains_key("text"));
}

#[test]
fn local_actions_and_expression_precedence_are_native_not_json() {
    let source = r#"root = Panel("Counter", [Text("Count: " + $count), Progress("Score", score, 50), buttons])
$count = 2
score = ($count + 3) * 4 - 2
buttons = Buttons([Button("Add", Action([@Set($count, $count + 1)]))])"#;
    let doc = Document::parse(source).unwrap();
    let Node::Progress { value, max, .. } = doc.nodes()[1].clone() else {
        panic!("progress")
    };
    let mut session = Session::new(doc).unwrap();
    assert_eq!(session.progress(&value, &max).unwrap(), (18.0, 50.0));
    session.activate_button(2, 0).unwrap();
    assert_eq!(session.value("count"), "3");
    assert_eq!(session.progress(&value, &max).unwrap(), (22.0, 50.0));
    let Node::Text { text, .. } = &session.document().nodes()[0] else {
        panic!("text")
    };
    assert_eq!(session.text(text).unwrap(), "Count: 3");
}

#[test]
fn comments_multiline_arrays_strings_and_optional_arguments_are_unambiguous() {
    let source = r#"# ) ] are just a comment
root = Panel("Demo", [
    Text("https://example.com/#section"), // ) ignored
    callout,
    Hero('Hello', null, 'NEW'),
])
callout = Callout("tip", "One small thing at a time")
"#;
    let doc = Document::parse(source).unwrap();
    assert_eq!(doc.nodes().len(), 3);
    assert_eq!(
        StreamParser::default().set(source).unwrap().nodes().len(),
        3
    );
}

#[test]
fn invalid_constructs_cycles_missing_references_and_budgets_fail_closed() {
    for source in [
        "root = Panel(\"Bad\", [missing])",
        "root = Panel(\"Bad\", [a])\na = b\nb = a",
        "root = Panel(\"Bad\", [Canvas(4)])",
        "root = Panel(\"Bad\", [Text(\"Hi\", \"muted\", 1, 2)])",
        "root = Panel(\"Bad\", [Input(\"not_a_binding\", \"Name\")])",
        "root = Panel(\"Bad\", [Button(\"Go\", @OpenUrl(\"https://example.com\"))])",
        "root = Query(\"read_file\")",
        "root = {title: \"Not a component language\"}",
    ] {
        assert!(Document::check(source).is_err(), "{source}");
    }
    assert!(
        StreamParser::default()
            .set(&"x".repeat(MAX_SOURCE + 1))
            .is_none()
    );
    let nested = format!("root = {}1{}", "[".repeat(100), "]".repeat(100));
    assert!(Document::check(&nested).is_err());
    let nodes = format!(
        "root = Panel(\"Bounded\", [{}])",
        "Text(\"Hi\"),".repeat(17)
    );
    assert!(Document::check(&nodes).is_err());
    // The same reference repeated can otherwise expand exponentially.
    let mut dag = "root = Panel(\"Bounded\", x)\nx = [a,a,a,a,a,a,a,a]\n".to_string();
    dag.push_str("a = [b,b,b,b,b,b,b,b]\nb = [c,c,c,c,c,c,c,c]\nc = Text(\"Hi\")");
    assert!(Document::check(&dag).is_err());
}

#[test]
fn invalid_siblings_do_not_hide_valid_streamed_elements() {
    let source =
        "root = Panel(\"Live\", [Text(\"Visible\"), unsupported])\nunsupported = Canvas(10)\n";
    assert_eq!(
        StreamParser::default().set(source).unwrap().nodes().len(),
        1
    );
    assert!(Document::check(source).is_err());
}

#[test]
fn every_terminal_component_has_a_native_positional_contract() {
    let cases = [
        "Hero(\"Title\", \"Subtitle\", \"NEW\")",
        "Text(\"Hello \" + $name, \"accent\")",
        "Image(\"https://example.com/picture.png\", \"A picture\", 4)",
        "Columns([Column(\"A\", [Item(\"Good\", \"ok\")]), Column(\"B\", [Item(\"Tradeoff\", \"no\")])])",
        "Cards([Card(\"A\", \"Details\", \"NEW\", \"success\")])",
        "Stats([Stat($n, \"Count\", \"+12%\")])",
        "Steps([Step(\"Start\", null, \"done\"), Step(\"Finish\", \"Next up\", \"current\")])",
        "Callout(\"tip\", \"Try this\", \"A helpful tip\")",
        "Quote(\"Stay curious\", \"Ada\")",
        "Divider(\"Details\")",
        "Buttons([Button(\"Continue\", @Answer())])",
        "Input($name, \"Your name\")",
        "Kv([[\"Count\", \"{{n}}\"]])",
        "Table([\"Name\"], [[\"Ada\"]], $n)",
        "Progress(\"Done\", $ready ? $n * 2 : 0, 10)",
        "Bars([Bar(\"A\", $n), Bar(\"B\", 3)])",
        "Sparkline([1, $n, 3, -2], 2)",
        "Checklist([Check(\"Ready\", $ready)])",
        "Select($mode, \"Mode\", [\"Fast\", \"Full\"])",
        "Stepper($n, \"Count\", -5, 5, 0.5)",
        "Toggle($ready, \"Ready\")",
    ];
    for component in cases {
        let source = format!(
            "root = Panel(\"Contracts\", [{component}])\n$n = 1\n$ready = true\n$name = \"Ada\"\n$mode = \"Fast\""
        );
        let document =
            Document::parse(&source).unwrap_or_else(|error| panic!("{component}: {error}"));
        assert_eq!(document.nodes().len(), 1, "{component}");
        assert!(
            StreamParser::default().set(&source).is_some(),
            "{component}"
        );
    }
}

#[test]
fn text_never_silently_concatenates_numeric_arithmetic() {
    let source = "root = Panel(\"Numbers\", [Text($a + $b)])\n$a = 1\n$b = 2";
    let errors = Document::check(source).unwrap_err();
    assert!(errors[0].message.contains("numeric arithmetic in text"));
    let source = "root = Panel(\"Numbers\", [Text(\"Sum: \" + ($a + $b))])\n$a = 1\n$b = 2";
    assert!(Document::check(source).is_err());
}

#[test]
fn semantic_errors_in_arriving_siblings_do_not_blank_valid_content() {
    let source = "root = Panel(\"Live\", [Text(\"Visible\"), Text(\"Bad tone\", \"invented\"), Input($missing, \"Pending\")])";
    assert!(Document::check(source).is_err());
    let document = StreamParser::default().set(source).unwrap();
    assert_eq!(document.nodes().len(), 1);
    assert!(matches!(&document.nodes()[0], Node::Text { text, .. } if text == "Visible"));
}
