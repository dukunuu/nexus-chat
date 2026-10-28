use super::*;
use std::sync::Mutex as StdMutex;

/// Answers from a table, and records what was asked.
struct Fixture {
    answers: HashMap<String, Result<Vec<u8>, String>>,
    asked: StdMutex<Vec<String>>,
}
impl Source for Fixture {
    fn get<'a>(&'a self, url: &'a str, limit: usize) -> Fetched<'a> {
        self.asked.lock().unwrap().push(url.to_string());
        let answer = self
            .answers
            .get(url)
            .cloned()
            .unwrap_or_else(|| Err("HTTP 400 Bad Request".into()));
        Box::pin(async move {
            let bytes = answer.map_err(|e| anyhow::anyhow!(e))?;
            ensure!(bytes.len() <= limit, "too large");
            Ok(bytes)
        })
    }
}

fn css(url: &str) -> Vec<u8> {
    format!("@font-face {{\n  font-family: 'X';\n  font-style: normal;\n  font-weight: 400;\n  src: url({url}) format('truetype');\n}}\n").into_bytes()
}

#[test]
fn stylesheets_name_one_upright_ttf_on_the_files_host() {
    assert_eq!(
        stylesheet_url("Space Grotesk", 700),
        "https://fonts.googleapis.com/css2?family=Space+Grotesk:wght@700"
    );
    let sheet = "@font-face { font-style: italic; src: url(https://fonts.gstatic.com/s/a/i.ttf) }\n@font-face { font-style: normal; src: url(https://fonts.gstatic.com/s/a/r.ttf) format('truetype'); }";
    assert_eq!(
        ttf_url(sheet).as_deref(),
        Some("https://fonts.gstatic.com/s/a/r.ttf")
    );
    assert_eq!(
        ttf_url("@font-face { src: url(https://evil.example/a.ttf) }"),
        None
    );
    assert_eq!(
        ttf_url("@font-face { src: url(https://fonts.gstatic.com/s/a/r.woff2) }"),
        None
    );
    assert!(plausible("DM Serif Display"));
    for odd in ["", "serif", "Inter&x=1", "../fonts", &"A".repeat(49)] {
        assert!(!plausible(odd), "{odd}");
    }
    assert_eq!(file_name("Space Grotesk", 700), "space-grotesk-700.ttf");
}

#[test]
fn the_families_a_deck_draws_with_resolve_roles_and_snap_weights() {
    let mut deck = Deck::titled("Fonts");
    deck.theme.fonts = BTreeMap::from([
        ("heading".into(), "Fraunces".into()),
        ("body".into(), "Inter".into()),
        ("mono".into(), "JetBrains Mono".into()),
    ]);
    deck.theme.text_styles = BTreeMap::from([(
        "title".into(),
        Font {
            family: Some("heading".into()),
            weight: Some(650),
            ..Font::default()
        },
    )]);
    let wanted = wanted(&deck);
    assert_eq!(wanted["Fraunces"], BTreeSet::from([400, 700]));
    assert_eq!(wanted["Inter"], BTreeSet::from([400]));
    assert!(wanted.contains_key("JetBrains Mono"));
}

#[tokio::test]
async fn missing_families_are_downloaded_once_into_the_deck_assets() {
    let Some(ttf) = installed_ttf() else { return };
    let root = std::env::temp_dir().join(format!("nexus-google-fonts-{}", uuid::Uuid::new_v4()));
    let mut deck = Deck::titled("Fonts");
    deck.theme.fonts = BTreeMap::from([
        ("heading".into(), "Fraunces".into()),
        ("body".into(), "Inter".into()),
    ]);
    deck.theme.text_styles = BTreeMap::from([(
        "title".into(),
        Font {
            family: Some("heading".into()),
            weight: Some(700),
            ..Font::default()
        },
    )]);
    let file = |w: u16| format!("https://fonts.gstatic.com/s/fraunces/{w}.ttf");
    let fixture = Fixture {
        answers: HashMap::from([
            (stylesheet_url("Fraunces", 400), Ok(css(&file(400)))),
            (stylesheet_url("Fraunces", 700), Ok(css(&file(700)))),
            (file(400), Ok(ttf.clone())),
            (file(700), Ok(ttf.clone())),
        ]),
        asked: StdMutex::default(),
    };
    // Inter counts as installed here, so only Fraunces is fetched.
    let problems = provide(&fixture, &root, &mut deck, &["Inter".into()]).await;
    assert!(problems.is_empty(), "{problems:?}");
    let files: Vec<_> = deck
        .theme
        .font_files
        .iter()
        .map(|f| (f.family.as_str(), f.weight, f.file.as_str()))
        .collect();
    assert_eq!(
        files,
        [
            ("Fraunces", 400, "assets/fonts/fraunces-400.ttf"),
            ("Fraunces", 700, "assets/fonts/fraunces-700.ttf")
        ]
    );
    assert!(root.join("fonts/fraunces-700.ttf").exists());
    assert!(
        fixture
            .asked
            .lock()
            .unwrap()
            .iter()
            .all(|u| !u.contains("Inter"))
    );
    // A second deck in the same folder reuses the files without asking.
    let asked = fixture.asked.lock().unwrap().len();
    let mut other = deck.clone();
    other.theme.font_files.clear();
    assert!(
        provide(&fixture, &root, &mut other, &["Inter".into()])
            .await
            .is_empty()
    );
    assert_eq!(other.theme.font_files.len(), 2);
    assert_eq!(fixture.asked.lock().unwrap().len(), asked);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn unknown_families_and_bad_files_fall_back_with_a_note() {
    let root = std::env::temp_dir().join(format!("nexus-google-fonts-{}", uuid::Uuid::new_v4()));
    let url = "https://fonts.gstatic.com/s/notafont/400.ttf";
    let fixture = Fixture {
        answers: HashMap::from([
            (stylesheet_url("Not A Font", 400), Ok(css(url))),
            (url.to_string(), Ok(b"<html>nope</html>".to_vec())),
        ]),
        asked: StdMutex::default(),
    };
    let mut deck = Deck::titled("Fonts");
    deck.theme.fonts = BTreeMap::from([
        ("heading".into(), "Nexus Imaginary Sans".into()),
        ("body".into(), "Not A Font".into()),
    ]);
    let problems = provide(&fixture, &root, &mut deck, &[]).await;
    assert!(deck.theme.font_files.is_empty());
    assert!(
        problems.iter().any(|p| p.contains("Nexus Imaginary Sans")),
        "{problems:?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("Not A Font") && p.contains("not a font file")),
        "{problems:?}"
    );
    // Google's "no such family" is remembered, so the next edit does not ask again.
    let asked = fixture.asked.lock().unwrap().len();
    provide(&fixture, &root, &mut deck, &[]).await;
    assert_eq!(
        fixture
            .asked
            .lock()
            .unwrap()
            .iter()
            .filter(|u| u.contains("Imaginary"))
            .count(),
        1,
        "asked once"
    );
    assert!(
        fixture.asked.lock().unwrap().len() > asked,
        "a bad file is not remembered as missing"
    );
    let _ = std::fs::remove_dir_all(root);
}
