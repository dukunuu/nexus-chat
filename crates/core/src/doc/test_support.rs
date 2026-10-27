use super::*;
use serde_json::json;
pub(crate) struct Temp(pub std::path::PathBuf);
impl Temp {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nexus-doc-p2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.join("assets")).unwrap();
        Self(path)
    }
    pub fn image(&self) {
        let image = image::RgbImage::from_fn(40, 80, |x, y| {
            image::Rgb([
                u8::try_from(x * 5).unwrap(),
                u8::try_from(y * 3).unwrap(),
                100,
            ])
        });
        image.save(self.0.join("assets/sample.png")).unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub(crate) fn checked(value: serde_json::Value) -> Doc {
    Doc::check(&value.to_string()).unwrap()
}
pub(crate) fn sample() -> Doc {
    checked(
        json!({"version":1,"title":"Research report","page":{"header":"{title}","footer":"{page} / {pages}"},"footnotes":{"source":"A primary source."},"blocks":[
            {"id":"title","kind":"title","text":"Research report"},
            {"id":"intro","kind":"paragraph","text":"A carefully set document with **bold**, *italic*, ~~strike~~, `inline code`, a [source](https://example.test/report) and a footnote[^source]."},
            {"id":"toc","kind":"toc"},
            {"id":"heading","kind":"heading","level":1,"text":"Findings"},
            {"id":"paragraph","kind":"paragraph","text":"Reliable evidence should be legible. A clear hierarchy and a comfortable reading measure help readers distinguish the finding from its supporting detail."},
            {"id":"list","kind":"list","ordered":true,"items":[{"text":"Gather evidence","children":[{"text":"Check the primary source"}]},{"text":"Review","checked":true}]},
            {"id":"quote","kind":"quote","text":"Clarity is part of correctness."},
            {"id":"callout","kind":"callout","tone":"note","text":"Figures are preliminary; keep their sources nearby."},
            {"id":"code","kind":"code","lang":"sql","text":"select region, revenue\n  from quarterly_results;"},
            {"id":"table","kind":"table","header":true,"align":["left","right"],"rows":[["Region","Revenue"],["Europe","4.1"],["Asia","3.2"]]},
            {"id":"image","kind":"image","src":"assets/sample.png","alt":"Color study","caption":"Figure 1. A color study.","width":"inline"},
            {"id":"chart","kind":"chart","chart":{"chart":"bar","labels":["Q1","Q2"],"series":[{"name":"Revenue","values":[3,4]}]}},
            {"id":"divider","kind":"divider"},
            {"id":"break","kind":"page_break"},
            {"id":"heading2","kind":"heading","level":2,"text":"Next steps"},
            {"id":"last","kind":"paragraph","text":"Share the report and review the evidence together."}
        ]}),
    )
}
pub(crate) fn thread(doc: &Doc, resolved: bool) -> feedback::Thread {
    feedback::Thread {
        id: if resolved { "resolved" } else { "open" }.into(),
        anchor: feedback::Anchor {
            block: Some("intro".into()),
            range: Some(feedback::Range { start: 2, end: 11 }),
            quote: doc
                .block("intro")
                .unwrap()
                .plain_text()
                .chars()
                .skip(2)
                .take(9)
                .collect(),
        },
        severity: feedback::Severity::Should,
        created: 1,
        resolved: resolved.then_some(2),
        messages: vec![feedback::Message {
            id: "m1".into(),
            role: feedback::Role::You,
            text: if resolved {
                "closed comment"
            } else {
                "open comment"
            }
            .into(),
            created: 1,
            edits: vec![],
        }],
    }
}
/// A multi-page report used for pagination previews and PDF checks.
pub(crate) fn long() -> Doc {
    let mut blocks = vec![
        json!({"id":"t","kind":"title","text":"Quarterly operating review"}),
        json!({"id":"lead","kind":"paragraph","text":"This review covers revenue, retention and the plan for the next quarter. It is long enough to flow across several pages and exercise every pagination rule."}),
        json!({"id":"toc","kind":"toc"}),
    ];
    let sentence = "Revenue grew while churn fell, and the team shipped the planned work with fewer incidents than the previous quarter, which let us invest the saved time in reliability. ";
    for section in 1..=4 {
        blocks.push(json!({"id":format!("h{section}"),"kind":"heading","level":1,"text":format!("Section {section}")}));
        for paragraph in 1..=3 {
            blocks.push(json!({"id":format!("p{section}{paragraph}"),"kind":"paragraph","text":sentence.repeat(3 + paragraph)}));
        }
        blocks.push(json!({"id":format!("s{section}"),"kind":"heading","level":2,"text":format!("Details {section}")}));
        blocks.push(json!({"id":format!("l{section}"),"kind":"list","ordered":section % 2 == 0,"items":[{"text":"First point with enough words to wrap onto a second line when the column is narrow, as it usually is."},{"text":"Second point","children":[{"text":"Nested detail"}]},{"text":"Third point"}]}));
        blocks.push(json!({"id":format!("q{section}"),"kind":"quote","text":"Good documents are easy to scan and pleasant to read."}));
        if section == 2 {
            let mut all = vec![json!(["Region", "Revenue", "Growth"])];
            all.extend((0..40).map(|i| {
                json!([
                    format!("Region {i}"),
                    format!("{}", 100 + i * 7),
                    format!("{}%", i % 9)
                ])
            }));
            blocks.push(json!({"id":"table","kind":"table","header":true,"align":["left","right","right"],"rows":all}));
        }
        if section == 3 {
            let code = (0..60)
                .map(|i| format!("let value_{i} = compute({i});"))
                .collect::<Vec<_>>()
                .join("\n");
            blocks.push(json!({"id":"code","kind":"code","lang":"rust","text":code}));
            blocks.push(json!({"id":"note","kind":"callout","tone":"warn","text":"Numbers in this section are preliminary and subject to the final close."}));
        }
    }
    checked(
        json!({"version":1,"title":"Quarterly operating review","page":{"header":"{title}","footer":"Page {page} of {pages}"},"blocks":blocks}),
    )
}
