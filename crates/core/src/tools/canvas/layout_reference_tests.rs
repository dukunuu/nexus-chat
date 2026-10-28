use super::{SCHEMA, read};

#[test]
fn rich_layout_slots_are_deferred_and_fit_a_small_result() {
    for name in [
        "cover_image",
        "section_color",
        "stats",
        "timeline",
        "image_grid",
        "quote_photo",
        "cards",
        "team",
        "chart_focus",
        "closing_image",
    ] {
        assert!(SCHEMA.contains(name), "missing {name}");
    }
    for fields in [
        "stat{1..3}_{value,label,detail}",
        "step{1..5}_{date,title,body}",
        "card{1..3}_{icon,title,body}",
        "person{1..4}_{image,name,role}",
    ] {
        assert!(SCHEMA.contains(fields), "missing {fields}");
    }
    assert!(
        SCHEMA.chars().count() + 1000 <= read::RESULT_BUDGET,
        "field reference leaves no room for the result: {}",
        SCHEMA.chars().count()
    );
}
