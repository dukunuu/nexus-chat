//! Hand-built `.pptx` packages for the import tests: a minimal presentation
//! with one master, one layout and one theme, each replaceable.

use std::io::Write as _;

pub(super) const NS: &str = "xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\"";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

pub(super) const THEME: &str = "<a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" name=\"Test\"><a:themeElements><a:clrScheme name=\"Test\"><a:dk1><a:srgbClr val=\"111111\"/></a:dk1><a:lt1><a:srgbClr val=\"FFFFFF\"/></a:lt1><a:dk2><a:srgbClr val=\"44546A\"/></a:dk2><a:lt2><a:srgbClr val=\"E7E6E6\"/></a:lt2><a:accent1><a:srgbClr val=\"4472C4\"/></a:accent1><a:accent2><a:srgbClr val=\"ED7D31\"/></a:accent2><a:accent3><a:srgbClr val=\"A5A5A5\"/></a:accent3><a:accent4><a:srgbClr val=\"FFC000\"/></a:accent4><a:accent5><a:srgbClr val=\"5B9BD5\"/></a:accent5><a:accent6><a:srgbClr val=\"70AD47\"/></a:accent6></a:clrScheme><a:fontScheme name=\"Test\"><a:majorFont><a:latin typeface=\"Georgia\"/></a:majorFont><a:minorFont><a:latin typeface=\"Verdana\"/></a:minorFont></a:fontScheme><a:fmtScheme name=\"Test\"><a:lnStyleLst><a:ln w=\"6350\"/><a:ln w=\"12700\"/><a:ln w=\"19050\"/></a:lnStyleLst></a:fmtScheme></a:themeElements></a:theme>";

/// A master with a title and a body placeholder and the usual text styles.
pub(super) const MASTER_TREE: &str = "<p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"Title\"/><p:cNvSpPr/><p:nvPr><p:ph type=\"title\"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"609600\" y=\"304800\"/><a:ext cx=\"10972800\" cy=\"1143000\"/></a:xfrm></p:spPr><p:txBody><a:bodyPr anchor=\"ctr\"/><a:lstStyle/><a:p><a:r><a:t>Title</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id=\"3\" name=\"Body\"/><p:cNvSpPr/><p:nvPr><p:ph type=\"body\" idx=\"1\"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"609600\" y=\"1600200\"/><a:ext cx=\"10972800\" cy=\"4525963\"/></a:xfrm></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>Body</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id=\"4\" name=\"Slide number\"/><p:cNvSpPr/><p:nvPr><p:ph type=\"sldNum\" idx=\"4\"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"8610600\" y=\"6356350\"/><a:ext cx=\"2743200\" cy=\"365125\"/></a:xfrm></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:fld type=\"slidenum\"><a:t>7</a:t></a:fld></a:p></p:txBody></p:sp>";

pub(super) const MASTER_STYLES: &str = "<p:txStyles><p:titleStyle><a:lvl1pPr algn=\"l\"><a:lnSpc><a:spcPct val=\"90000\"/></a:lnSpc><a:defRPr sz=\"4400\" b=\"1\"><a:solidFill><a:schemeClr val=\"tx1\"/></a:solidFill><a:latin typeface=\"+mj-lt\"/></a:defRPr></a:lvl1pPr></p:titleStyle><p:bodyStyle><a:lvl1pPr marL=\"228600\" indent=\"-228600\"><a:buChar char=\"&#8226;\"/><a:defRPr sz=\"2800\"><a:solidFill><a:schemeClr val=\"tx1\"/></a:solidFill><a:latin typeface=\"+mn-lt\"/></a:defRPr></a:lvl1pPr><a:lvl2pPr marL=\"685800\" indent=\"-228600\"><a:buChar char=\"-\"/><a:defRPr sz=\"2400\"/></a:lvl2pPr></p:bodyStyle><p:otherStyle><a:lvl1pPr><a:defRPr sz=\"1800\"><a:solidFill><a:schemeClr val=\"tx1\"/></a:solidFill><a:latin typeface=\"+mn-lt\"/></a:defRPr></a:lvl1pPr></p:otherStyle></p:txStyles>";

pub(super) const CLR_MAP: &str = "<p:clrMap bg1=\"lt1\" tx1=\"dk1\" bg2=\"lt2\" tx2=\"dk2\" accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" hlink=\"accent1\" folHlink=\"accent1\"/>";

/// One slide: its full XML and extra relationships `(id, type, target)`.
pub(super) struct SlideSpec {
    pub xml: String,
    pub rels: Vec<(String, String, String)>,
}

/// A slide whose shape tree holds `shapes`.
pub(super) fn slide(shapes: &str) -> SlideSpec {
    SlideSpec {
        xml: format!(
            "<p:sld {NS}><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{shapes}</p:spTree></p:cSld></p:sld>"
        ),
        rels: Vec::new(),
    }
}

impl SlideSpec {
    pub fn rel(mut self, id: &str, kind: &str, target: &str) -> Self {
        self.rels.push((id.into(), kind.into(), target.into()));
        self
    }

    /// The slide element with extra children after its shape tree.
    pub fn after_tree(mut self, extra: &str) -> Self {
        self.xml = self.xml.replace("</p:cSld>", &format!("</p:cSld>{extra}"));
        self
    }

    pub fn background(mut self, bg: &str) -> Self {
        self.xml = self
            .xml
            .replace("<p:cSld>", &format!("<p:cSld><p:bg>{bg}</p:bg>"));
        self
    }
}

pub(super) struct Package {
    /// Slide size in EMU.
    pub size: (u32, u32),
    pub master_tree: String,
    pub master_styles: String,
    pub master_extra: String,
    pub layout_tree: String,
    pub layout_attrs: String,
    pub slides: Vec<SlideSpec>,
    pub theme: String,
    pub extra: Vec<(String, Vec<u8>)>,
    pub master_rels: Vec<(String, String, String)>,
}

impl Package {
    pub fn new(slides: Vec<SlideSpec>) -> Self {
        Self {
            size: (12_192_000, 6_858_000),
            master_tree: MASTER_TREE.into(),
            master_styles: MASTER_STYLES.into(),
            master_extra: String::new(),
            layout_tree: String::new(),
            layout_attrs: String::new(),
            slides,
            theme: THEME.into(),
            extra: Vec::new(),
            master_rels: Vec::new(),
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        let tree = |shapes: &str| {
            format!(
                "<p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{shapes}</p:spTree></p:cSld>"
            )
        };
        let rels = |entries: &[(String, String, String)]| {
            let body: String = entries
                .iter()
                .map(|(id, kind, target)| {
                    let kind = if kind.starts_with("http") {
                        kind.clone()
                    } else {
                        format!("{REL}/{kind}")
                    };
                    format!("<Relationship Id=\"{id}\" Type=\"{kind}\" Target=\"{target}\"/>")
                })
                .collect();
            format!(
                "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{body}</Relationships>"
            )
        };
        let mut parts: Vec<(String, Vec<u8>)> = vec![
            (
                "[Content_Types].xml".into(),
                b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>"
                    .to_vec(),
            ),
            (
                "_rels/.rels".into(),
                rels(&[(
                    "rId1".into(),
                    "officeDocument".into(),
                    "ppt/presentation.xml".into(),
                )])
                .into_bytes(),
            ),
        ];
        let ids: String = (0..self.slides.len())
            .map(|i| format!("<p:sldId id=\"{}\" r:id=\"rId{}\"/>", 256 + i, i + 2))
            .collect();
        parts.push((
            "ppt/presentation.xml".into(),
            format!(
                "<p:presentation {NS}><p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rId1\"/></p:sldMasterIdLst><p:sldIdLst>{ids}</p:sldIdLst><p:sldSz cx=\"{}\" cy=\"{}\"/></p:presentation>",
                self.size.0, self.size.1
            )
            .into_bytes(),
        ));
        let mut presentation_rels = vec![(
            "rId1".to_string(),
            "slideMaster".to_string(),
            "slideMasters/slideMaster1.xml".to_string(),
        )];
        for i in 0..self.slides.len() {
            presentation_rels.push((
                format!("rId{}", i + 2),
                "slide".into(),
                format!("slides/slide{}.xml", i + 1),
            ));
        }
        parts.push((
            "ppt/_rels/presentation.xml.rels".into(),
            rels(&presentation_rels).into_bytes(),
        ));
        parts.push((
            "ppt/slideMasters/slideMaster1.xml".into(),
            format!(
                "<p:sldMaster {NS}>{}{}{}{}</p:sldMaster>",
                tree(&self.master_tree),
                CLR_MAP,
                self.master_extra,
                self.master_styles
            )
            .into_bytes(),
        ));
        let mut master_rels = vec![
            (
                "rId1".to_string(),
                "slideLayout".to_string(),
                "../slideLayouts/slideLayout1.xml".to_string(),
            ),
            (
                "rId2".to_string(),
                "theme".to_string(),
                "../theme/theme1.xml".to_string(),
            ),
        ];
        master_rels.extend(self.master_rels.clone());
        parts.push((
            "ppt/slideMasters/_rels/slideMaster1.xml.rels".into(),
            rels(&master_rels).into_bytes(),
        ));
        parts.push((
            "ppt/slideLayouts/slideLayout1.xml".into(),
            format!(
                "<p:sldLayout {NS} {}>{}</p:sldLayout>",
                self.layout_attrs,
                tree(&self.layout_tree)
            )
            .into_bytes(),
        ));
        parts.push((
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels".into(),
            rels(&[(
                "rId1".into(),
                "slideMaster".into(),
                "../slideMasters/slideMaster1.xml".into(),
            )])
            .into_bytes(),
        ));
        parts.push((
            "ppt/theme/theme1.xml".into(),
            self.theme.clone().into_bytes(),
        ));
        for (i, slide) in self.slides.iter().enumerate() {
            let n = i + 1;
            parts.push((
                format!("ppt/slides/slide{n}.xml"),
                slide.xml.clone().into_bytes(),
            ));
            let mut entries = vec![(
                "rIdLayout".to_string(),
                "slideLayout".to_string(),
                "../slideLayouts/slideLayout1.xml".to_string(),
            )];
            entries.extend(slide.rels.clone());
            parts.push((
                format!("ppt/slides/_rels/slide{n}.xml.rels"),
                rels(&entries).into_bytes(),
            ));
        }
        parts.extend(self.extra.clone());
        zip(&parts)
    }
}

pub(super) fn zip(parts: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in parts {
        writer.start_file(name.as_str(), options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// A shape at `(x, y)` `w` by `h` EMU.
pub(super) fn xfrm(x: i64, y: i64, w: i64, h: i64) -> String {
    format!("<a:xfrm><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{w}\" cy=\"{h}\"/></a:xfrm>")
}

/// A PNG of `w` by `h` pixels: left half red, right half blue.
pub(super) fn png(w: u32, h: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(w, h, |x, _| {
        if x < w / 2 {
            image::Rgb([220, 30, 30])
        } else {
            image::Rgb([30, 30, 220])
        }
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}
