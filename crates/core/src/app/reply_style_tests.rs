use super::tests::app_with_key;
use super::*;

fn prompt(a: &mut App, style: &str) -> String {
    a.interactive_ui = true;
    a.execute(AppCommand::SetSetting {
        key: "reply_style".into(),
        value: style.into(),
    })
    .unwrap();
    a.system_prompt()
}

#[test]
fn style_command_parses_the_three_modes_and_rejects_others() {
    let a = app_with_key();
    for style in ["auto", "rich", "plain"] {
        assert_eq!(
            a.parse_command(&format!("style {style}")).unwrap(),
            AppCommand::SetSetting {
                key: "reply_style".into(),
                value: style.into()
            }
        );
    }
    assert!(a.parse_command("style").is_err());
    assert!(a.parse_command("style fancy").is_err());
    assert!(COMMANDS.iter().any(|c| c.name == "style"));
}

#[test]
fn reply_style_defaults_to_auto_and_round_trips_through_the_db() {
    let mut a = app_with_key();
    assert_eq!(a.settings.reply_style, "auto");
    a.run_command("style rich").unwrap();
    assert_eq!(a.settings.reply_style, "rich");
    assert!(
        a.db.load_settings()
            .unwrap()
            .iter()
            .any(|(k, v)| k == "reply_style" && v == "rich")
    );
    // A fresh App over the same db restores it.
    let reloaded = App::new(
        a.db,
        Some("sk-or-test-key"),
        crate::space::Space {
            root: std::env::temp_dir().join(format!("nexus-test-{}", uuid::Uuid::new_v4())),
        },
    );
    assert_eq!(reloaded.settings.reply_style, "rich");
    let mut b = app_with_key();
    assert!(
        b.execute(AppCommand::SetSetting {
            key: "reply_style".into(),
            value: "loud".into()
        })
        .is_err()
    );
    assert_eq!(b.settings.reply_style, "auto");
}

#[test]
fn reply_style_controls_the_layout_guidance_in_the_prompt() {
    let mut a = app_with_key();
    let auto = prompt(&mut a, "auto");
    assert!(auto.contains(crate::interactive::LAYOUTS));
    assert!(!auto.contains("Prefer a layout for most substantive answers"));
    let rich = prompt(&mut a, "rich");
    assert!(rich.contains(crate::interactive::LAYOUTS));
    assert!(rich.contains("Prefer a layout for most substantive answers"));
    let plain = prompt(&mut a, "plain");
    assert!(!plain.contains("## Designed replies"));
    // The interactive UI guidance is independent of the reply style.
    assert!(plain.contains(crate::interactive::INSTRUCTIONS));
    // Clients without a renderer never get layout guidance.
    a.interactive_ui = false;
    a.settings.reply_style = "rich".into();
    assert!(!a.system_prompt().contains("## Designed replies"));
}

#[test]
fn changing_reply_style_bumps_the_prompt_cache_epoch_once_per_change() {
    let mut a = app_with_key();
    let epoch = a.cache_epoch;
    a.run_command("style plain").unwrap();
    assert_eq!(a.cache_epoch, epoch + 1);
    a.run_command("style plain").unwrap();
    assert_eq!(a.cache_epoch, epoch + 1);
    a.run_command("style rich").unwrap();
    assert_eq!(a.cache_epoch, epoch + 2);
}

#[test]
fn context_estimate_counts_layout_guidance_only_when_it_is_sent() {
    let mut a = app_with_key();
    a.interactive_ui = true;
    a.settings.reply_style = "plain".into();
    let plain = a.context_breakdown().system_tokens;
    a.settings.reply_style = "auto".into();
    let auto = a.context_breakdown().system_tokens;
    assert!(auto > plain, "{auto} vs {plain}");
    a.interactive_ui = false;
    assert!(a.context_breakdown().system_tokens < plain);
}
