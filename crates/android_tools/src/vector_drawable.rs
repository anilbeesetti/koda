use anyhow::{Context as _, Result, bail, ensure};
use kurbo::{Affine, BezPath, ParamCurve, ParamCurveArclen, PathEl};
use quick_xml::{Reader, events::Event};
use roxmltree::{Document, Node};
use std::collections::HashMap;

const ANDROID: &str = "http://schemas.android.com/apk/res/android";
const AAPT: &str = "http://schemas.android.com/aapt";
const MAX_XML_SIZE: usize = 1024 * 1024;
const PREVIEW_SIZE: f64 = 256.;

pub fn is_vector_drawable(xml: &str) -> bool {
    if xml.len() > MAX_XML_SIZE {
        return false;
    }
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element)) => {
                return element.name().as_ref() == b"vector";
            }
            Ok(Event::Decl(_) | Event::Comment(_) | Event::PI(_) | Event::Text(_)) => {}
            _ => return false,
        }
    }
}

#[derive(Default)]
pub struct Resources(HashMap<String, String>);

impl Resources {
    pub fn add_values_xml(&mut self, xml: &str) -> Result<()> {
        let document = parse_xml(xml)?;
        ensure!(
            document.root_element().has_tag_name("resources"),
            "Expected <resources>"
        );
        for node in document.root_element().children().filter(Node::is_element) {
            let kind = if node.has_tag_name("item") {
                node.attribute("type").unwrap_or("")
            } else {
                node.tag_name().name()
            };
            if matches!(kind, "color" | "dimen" | "string" | "integer")
                && let Some(name) = node.attribute("name")
                && !node.children().any(|child| child.is_element())
            {
                self.0.insert(
                    format!("@{kind}/{name}"),
                    node.text().unwrap_or("").trim().into(),
                );
            }
        }
        Ok(())
    }

    fn resolve(&self, value: &str) -> Result<String> {
        let mut value = value.trim();
        for _ in 0..32 {
            if let Some(color) = match value {
                "@android:color/transparent" => Some("#00000000"),
                "@android:color/black" => Some("#ff000000"),
                "@android:color/white" => Some("#ffffffff"),
                _ => None,
            } {
                return Ok(color.into());
            }
            if value == "@null" {
                return Ok("#00000000".into());
            }
            if value.starts_with('@') {
                value = self.0.get(value).with_context(|| format!("Cannot resolve {value}. Vector previews use this source set's res/values resources."))?;
            } else if value.starts_with('?') {
                bail!(
                    "Cannot resolve theme attribute {value}. Use a literal color or a res/values resource for this preview."
                );
            } else {
                return Ok(value.into());
            }
        }
        bail!("Cyclic or excessively nested resource reference: {value}")
    }
}

fn parse_xml(xml: &str) -> Result<Document<'_>> {
    ensure!(
        xml.len() <= MAX_XML_SIZE,
        "Vector preview XML exceeds 1 MiB"
    );
    Document::parse(xml).context("Invalid vector preview XML")
}

/// Converts Android's vector tree to a self-contained SVG for Koda's renderer.
/// Like Studio's VdPreview, this does not require layoutlib, an SDK, or a build.
pub fn to_svg(xml: &str, resources: &Resources) -> Result<String> {
    let document = parse_xml(xml)?;
    let root = document.root_element();
    ensure!(root.has_tag_name("vector"), "Expected a <vector> drawable");
    let mut writer = VectorWriter {
        resources,
        definitions: String::new(),
        next_id: 0,
    };
    let width = writer.dimension(root, "width")?;
    let height = writer.dimension(root, "height")?;
    let viewport_width = writer.number(root, "viewportWidth", None)?;
    let viewport_height = writer.number(root, "viewportHeight", None)?;
    ensure!(
        viewport_width > 0. && viewport_height > 0.,
        "Vector viewport dimensions must be positive"
    );
    let scale = PREVIEW_SIZE / width.max(height);
    let (width, height) = (width * scale, height * scale);
    ensure!(
        width >= 1. && height >= 1.,
        "Vector aspect ratio is too extreme to preview"
    );
    let transform = Affine::scale_non_uniform(width / viewport_width, height / viewport_height);
    let stroke_scale = (width / viewport_width).min(height / viewport_height);
    let content = writer.children(root, transform, Affine::IDENTITY, stroke_scale)?;
    let alpha = writer.number(root, "alpha", Some(1.))?.clamp(0., 1.);
    let content = if let Some(tint) = root
        .attribute((ANDROID, "tint"))
        .filter(|tint| *tint != "@null")
    {
        let (color, opacity) = writer.color(tint)?;
        let mode = writer.value(root, "tintMode", "src_in")?;
        let composite = match mode.as_str() {
            "src_in" | "5" => "<feComposite in=\"tint\" in2=\"SourceGraphic\" operator=\"in\"/>",
            "src_atop" | "9" => {
                "<feComposite in=\"tint\" in2=\"SourceGraphic\" operator=\"atop\"/>"
            }
            "src_over" | "3" => {
                "<feComposite in=\"tint\" in2=\"SourceGraphic\" operator=\"over\"/>"
            }
            "multiply" | "14" => {
                "<feComposite in=\"tint\" in2=\"SourceGraphic\" operator=\"arithmetic\" k1=\"1\"/>"
            }
            "screen" | "15" => "<feBlend in=\"tint\" in2=\"SourceGraphic\" mode=\"screen\"/>",
            "add" | "16" => {
                "<feComposite in=\"tint\" in2=\"SourceGraphic\" operator=\"arithmetic\" k2=\"1\" k3=\"1\"/>"
            }
            _ => bail!("Unsupported vector tintMode: {mode}"),
        };
        writer.definitions.push_str(&format!("<filter id=\"tint\" x=\"0\" y=\"0\" width=\"{width}\" height=\"{height}\" filterUnits=\"userSpaceOnUse\" color-interpolation-filters=\"sRGB\"><feFlood flood-color=\"{color}\" flood-opacity=\"{opacity}\" result=\"tint\"/>{composite}</filter>"));
        format!("<g filter=\"url(#tint)\">{content}</g>")
    } else {
        content
    };
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\"><defs>{}</defs><g opacity=\"{alpha}\">{content}</g></svg>",
        writer.definitions
    ))
}

struct VectorWriter<'a> {
    resources: &'a Resources,
    definitions: String,
    next_id: usize,
}

impl VectorWriter<'_> {
    fn id(&mut self) -> usize {
        self.next_id += 1;
        self.next_id
    }

    fn value(&self, node: Node<'_, '_>, name: &str, default: &str) -> Result<String> {
        self.resources
            .resolve(node.attribute((ANDROID, name)).unwrap_or(default))
    }

    fn number(&self, node: Node<'_, '_>, name: &str, default: Option<f64>) -> Result<f64> {
        let value = match node.attribute((ANDROID, name)) {
            Some(value) => self
                .resources
                .resolve(value)?
                .parse::<f64>()
                .with_context(|| format!("Invalid android:{name}"))?,
            None => default.with_context(|| format!("Missing android:{name}"))?,
        };
        ensure!(
            value.is_finite() && value.abs() <= 1e9,
            "Invalid android:{name}: expected a finite number within ±1e9"
        );
        Ok(value)
    }

    fn dimension(&self, node: Node<'_, '_>, name: &str) -> Result<f64> {
        let value = self.value(node, name, "")?;
        // Preview at mdpi; dp and px therefore have the same logical size.
        let value = value
            .strip_suffix("dip")
            .or_else(|| value.strip_suffix("dp"))
            .or_else(|| value.strip_suffix("px"))
            .unwrap_or(&value);
        let value: f64 = value
            .trim()
            .parse()
            .with_context(|| format!("Invalid android:{name}: expected dp or px"))?;
        ensure!(
            value.is_finite() && value > 0. && value <= 1e9,
            "Vector dimensions must be positive and finite"
        );
        Ok(value)
    }

    fn color(&self, value: &str) -> Result<(String, f64)> {
        let value = self.resources.resolve(value)?;
        let hex = value
            .strip_prefix('#')
            .context("Expected an Android #RGB, #ARGB, #RRGGBB or #AARRGGBB color")?;
        ensure!(
            hex.is_ascii() && hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid Android color: {value}"
        );
        let (rgb, alpha) = match hex.len() {
            3 => (hex.chars().flat_map(|c| [c, c]).collect::<String>(), 255),
            4 => (
                hex[1..].chars().flat_map(|c| [c, c]).collect::<String>(),
                u8::from_str_radix(&hex[..1], 16)? * 17,
            ),
            6 => (hex.into(), 255),
            8 => (hex[2..].into(), u8::from_str_radix(&hex[..2], 16)?),
            _ => bail!("Invalid Android color: {value}"),
        };
        Ok((format!("#{rgb}"), f64::from(alpha) / 255.))
    }

    fn children(
        &mut self,
        node: Node<'_, '_>,
        transform: Affine,
        group_transform: Affine,
        stroke_scale: f64,
    ) -> Result<String> {
        ensure!(
            node.ancestors().count() <= 64,
            "Vector groups are nested too deeply"
        );
        ensure!(
            transform
                .as_coeffs()
                .iter()
                .all(|value| value.is_finite() && value.abs() <= 1e12),
            "Vector transform is too large to preview"
        );
        let mut content = String::new();
        let mut clips = 0;
        for child in node.children().filter(Node::is_element) {
            match child.tag_name().name() {
                "group" => {
                    let pivot_x = self.number(child, "pivotX", Some(0.))?;
                    let pivot_y = self.number(child, "pivotY", Some(0.))?;
                    let local = Affine::translate((
                        self.number(child, "translateX", Some(0.))? + pivot_x,
                        self.number(child, "translateY", Some(0.))? + pivot_y,
                    )) * Affine::rotate(
                        self.number(child, "rotation", Some(0.))?.to_radians(),
                    ) * Affine::scale_non_uniform(
                        self.number(child, "scaleX", Some(1.))?,
                        self.number(child, "scaleY", Some(1.))?,
                    ) * Affine::translate((-pivot_x, -pivot_y));
                    content.push_str(&self.children(
                        child,
                        transform * local,
                        group_transform * local,
                        stroke_scale,
                    )?);
                }
                "path" => {
                    content.push_str(&self.path(child, transform, group_transform, stroke_scale)?)
                }
                "clip-path" => {
                    let id = self.id();
                    let path = transform * self.path_data(child)?;
                    let rule = self.fill_rule(child)?;
                    self.definitions.push_str(&format!("<clipPath id=\"clip{id}\" clipPathUnits=\"userSpaceOnUse\"><path d=\"{}\" clip-rule=\"{rule}\"/></clipPath>", path.to_svg()));
                    content.push_str(&format!("<g clip-path=\"url(#clip{id})\">"));
                    clips += 1;
                }
                _ => bail!("Unsupported vector element: <{}>", child.tag_name().name()),
            }
        }
        content.push_str(&"</g>".repeat(clips));
        Ok(content)
    }

    fn path_data(&self, node: Node<'_, '_>) -> Result<BezPath> {
        let data = self.value(node, "pathData", "")?;
        ensure!(!data.is_empty(), "Missing android:pathData");
        // usvg silently drops malformed paths; reject them before rendering.
        let mut path = BezPath::new();
        for segment in svgtypes::SimplifyingPathParser::from(data.as_str()) {
            use svgtypes::SimplePathSegment;
            match segment.context("Invalid android:pathData")? {
                SimplePathSegment::MoveTo { x, y } => path.move_to((x, y)),
                SimplePathSegment::LineTo { x, y } => path.line_to((x, y)),
                SimplePathSegment::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x,
                    y,
                } => path.curve_to((x1, y1), (x2, y2), (x, y)),
                SimplePathSegment::Quadratic { x1, y1, x, y } => path.quad_to((x1, y1), (x, y)),
                SimplePathSegment::ClosePath => path.close_path(),
            }
        }
        let bounds = path.control_box();
        ensure!(
            path.is_finite()
                && [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
                    .iter()
                    .all(|value| value.abs() <= 1e9),
            "Invalid android:pathData: coordinates must be finite and within ±1e9"
        );
        Ok(path)
    }

    fn fill_rule(&self, node: Node<'_, '_>) -> Result<&'static str> {
        match self.value(node, "fillType", "nonZero")?.as_str() {
            "nonZero" | "0" => Ok("nonzero"),
            "evenOdd" | "1" => Ok("evenodd"),
            value => bail!("Unsupported fillType: {value}"),
        }
    }

    fn path(
        &mut self,
        node: Node<'_, '_>,
        transform: Affine,
        group_transform: Affine,
        stroke_scale: f64,
    ) -> Result<String> {
        let path = self.path_data(node)?;
        let start = self.number(node, "trimPathStart", Some(0.))?;
        let end = self.number(node, "trimPathEnd", Some(1.))?;
        let offset = self.number(node, "trimPathOffset", Some(0.))?;
        let path = if start != 0. || end != 1. {
            trim_path(&path, start, end, offset)
        } else {
            path
        };
        let path = transform * path;
        let (fill, fill_opacity) = self.paint(node, "fillColor", transform)?;
        let (stroke, stroke_opacity) = self.paint(node, "strokeColor", transform)?;
        let fill_opacity = fill_opacity * self.number(node, "fillAlpha", Some(1.))?.clamp(0., 1.);
        let stroke_opacity =
            stroke_opacity * self.number(node, "strokeAlpha", Some(1.))?.clamp(0., 1.);
        // Android transforms the geometry first, then scales stroke width by
        // determinant / longest basis vector (not SVG's anisotropic stroke).
        let [a, b, c, d, _, _] = group_transform.as_coeffs();
        let longest = a.hypot(b).max(c.hypot(d));
        let matrix_scale = if longest == 0. {
            0.
        } else {
            (a * d - b * c).abs() / longest
        };
        let stroke_width =
            self.number(node, "strokeWidth", Some(0.))?.max(0.) * stroke_scale * matrix_scale;
        let linecap = match self.value(node, "strokeLineCap", "butt")?.as_str() {
            "butt" | "0" => "butt",
            "round" | "1" => "round",
            "square" | "2" => "square",
            value => bail!("Unsupported strokeLineCap: {value}"),
        };
        let linejoin = match self.value(node, "strokeLineJoin", "miter")?.as_str() {
            "miter" | "0" => "miter",
            "round" | "1" => "round",
            "bevel" | "2" => "bevel",
            value => bail!("Unsupported strokeLineJoin: {value}"),
        };
        let miter = self.number(node, "strokeMiterLimit", Some(4.))?.max(1.);
        let rule = self.fill_rule(node)?;
        Ok(format!(
            "<path d=\"{}\" fill=\"{fill}\" fill-opacity=\"{fill_opacity}\" fill-rule=\"{rule}\" stroke=\"{stroke}\" stroke-opacity=\"{stroke_opacity}\" stroke-width=\"{stroke_width}\" stroke-linecap=\"{linecap}\" stroke-linejoin=\"{linejoin}\" stroke-miterlimit=\"{miter}\"/>",
            path.to_svg()
        ))
    }

    fn paint(
        &mut self,
        node: Node<'_, '_>,
        name: &str,
        transform: Affine,
    ) -> Result<(String, f64)> {
        if let Some(attribute) = node.children().find(|child| {
            child.has_tag_name((AAPT, "attr"))
                && child
                    .attribute("name")
                    .is_some_and(|value| value.rsplit(':').next() == Some(name))
        }) {
            let gradient = attribute
                .children()
                .find(Node::is_element)
                .context("Missing gradient")?;
            ensure!(
                gradient.has_tag_name("gradient"),
                "Only gradient complex colors are supported"
            );
            return Ok((self.gradient(gradient, transform)?, 1.));
        }
        match node.attribute((ANDROID, name)) {
            Some(value) => self.color(value),
            None => Ok(("none".into(), 1.)),
        }
    }

    fn gradient(&mut self, node: Node<'_, '_>, transform: Affine) -> Result<String> {
        let id = self.id();
        let kind = self.value(node, "type", "linear")?;
        let (tag, coordinates) = match kind.as_str() {
            "linear" | "0" => (
                "linearGradient",
                format!(
                    "x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"",
                    self.number(node, "startX", Some(0.))?,
                    self.number(node, "startY", Some(0.))?,
                    self.number(node, "endX", Some(0.))?,
                    self.number(node, "endY", Some(0.))?
                ),
            ),
            "radial" | "1" => {
                let radius = self.number(node, "gradientRadius", None)?;
                ensure!(radius > 0., "Radial gradient radius must be positive");
                (
                    "radialGradient",
                    format!(
                        "cx=\"{}\" cy=\"{}\" r=\"{radius}\"",
                        self.number(node, "centerX", Some(0.))?,
                        self.number(node, "centerY", Some(0.))?
                    ),
                )
            }
            _ => bail!("Unsupported gradient type: {kind}. Sweep gradients cannot be previewed."),
        };
        let spread = match self.value(node, "tileMode", "clamp")?.as_str() {
            "clamp" | "0" => "pad",
            "repeat" | "1" => "repeat",
            "mirror" | "2" => "reflect",
            value => bail!("Unsupported gradient tileMode: {value}"),
        };
        let mut stops = Vec::new();
        for item in node.children().filter(Node::is_element) {
            ensure!(item.has_tag_name("item"), "Expected gradient <item>");
            stops.push((
                self.number(item, "offset", None)?,
                self.color(&self.value(item, "color", "")?)?,
            ));
        }
        if stops.is_empty() {
            stops.push((
                0.,
                self.color(&self.value(node, "startColor", "#00000000")?)?,
            ));
            if let Some(color) = node.attribute((ANDROID, "centerColor")) {
                stops.push((0.5, self.color(color)?));
            }
            stops.push((1., self.color(&self.value(node, "endColor", "#00000000")?)?));
        }
        ensure!(
            stops.len() >= 2,
            "A gradient needs at least two color stops"
        );
        let mut previous = 0.;
        let mut content = String::new();
        for (offset, (color, alpha)) in stops {
            ensure!(
                (previous..=1.).contains(&offset),
                "Gradient offsets must be ordered within 0..1"
            );
            previous = offset;
            content.push_str(&format!(
                "<stop offset=\"{offset}\" stop-color=\"{color}\" stop-opacity=\"{alpha}\"/>"
            ));
        }
        let [a, b, c, d, e, f] = transform.as_coeffs();
        self.definitions.push_str(&format!("<{tag} id=\"gradient{id}\" gradientUnits=\"userSpaceOnUse\" gradientTransform=\"matrix({a} {b} {c} {d} {e} {f})\" spreadMethod=\"{spread}\" {coordinates}>{content}</{tag}>"));
        Ok(format!("url(#gradient{id})"))
    }
}

fn trim_path(path: &BezPath, start: f64, end: f64, offset: f64) -> BezPath {
    // Android PathMeasure trims only the first contour.
    let first_contour: BezPath = path
        .elements()
        .iter()
        .take(1)
        .chain(
            path.elements()
                .iter()
                .skip(1)
                .take_while(|element| !matches!(element, PathEl::MoveTo(_))),
        )
        .copied()
        .collect();
    let length = first_contour
        .segments()
        .map(|segment| segment.arclen(1e-5))
        .sum::<f64>();
    let start = ((start + offset) % 1.) * length;
    let end = ((end + offset) % 1.) * length;
    let mut trimmed = BezPath::new();
    let ranges = if start > end {
        vec![start..length, 0.0..end]
    } else {
        vec![start..end]
    };
    for range in ranges {
        let mut position = 0.;
        let mut needs_move = true;
        for segment in first_contour.segments() {
            let segment_length = segment.arclen(1e-5);
            let from = (range.start - position).max(0.);
            let to = (range.end - position).min(segment_length);
            if from < to {
                let part = segment
                    .subsegment(segment.inv_arclen(from, 1e-5)..segment.inv_arclen(to, 1e-5));
                if needs_move {
                    trimmed.move_to(part.start());
                    needs_move = false;
                }
                trimmed.push(part.as_path_el());
            }
            position += segment_length;
        }
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;
    use resvg::{tiny_skia, usvg};

    fn vector(attributes: &str, content: &str) -> String {
        format!(
            r#"<vector xmlns:android="{ANDROID}" xmlns:aapt="{AAPT}" android:width="10dp" android:height="10dp" android:viewportWidth="10" android:viewportHeight="10" {attributes}>{content}</vector>"#
        )
    }

    fn render(xml: &str, resources: &Resources) -> tiny_skia::Pixmap {
        let svg = to_svg(xml, resources).expect("convert vector");
        let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).expect("parse SVG");
        let size = tree.size().to_int_size();
        let mut pixmap =
            tiny_skia::Pixmap::new(size.width(), size.height()).expect("allocate image");
        resvg::render(
            &tree,
            tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        pixmap
    }

    fn pixel(pixmap: &tiny_skia::Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let pixel = pixmap.pixel(x, y).expect("pixel in bounds");
        (pixel.red(), pixel.green(), pixel.blue(), pixel.alpha())
    }

    #[test]
    fn recognizes_only_vector_roots_even_while_children_are_being_edited() {
        assert!(is_vector_drawable(
            "<?xml version=\"1.0\"?><!-- icon --><vector><path"
        ));
        assert!(is_vector_drawable("<vector/>"));
        assert!(!is_vector_drawable("<!-- <vector/> --><resources/>"));
        assert!(!is_vector_drawable(
            "<selector><item><vector/></item></selector>"
        ));
        assert!(!is_vector_drawable(
            "<!DOCTYPE vector SYSTEM 'file:///etc/passwd'><vector/>"
        ));
    }

    #[test]
    fn renders_android_argb_colors_and_composites_root_alpha_once() {
        let xml = vector(
            r#"android:alpha="0.5""#,
            r##"<path android:fillColor="#80ff0000" android:pathData="M0,0h10v10H0z"/><path android:fillColor="#ff0000" android:pathData="M0,0h10v10H0z"/>"##,
        );
        let image = render(&xml, &Resources::default());
        assert_eq!(pixel(&image, 128, 128), (128, 0, 0, 128));
        let xml = vector(
            "",
            r##"<path android:fillColor="#8f00" android:fillAlpha="0.5" android:pathData="M0,0h10v10H0z"/>"##,
        );
        assert_eq!(
            pixel(&render(&xml, &Resources::default()), 128, 128),
            (68, 0, 0, 68)
        );
    }

    #[test]
    fn resolves_resource_aliases_dimensions_and_path_strings() {
        let mut resources = Resources::default();
        resources.add_values_xml(r##"<resources><color name="red">#f00</color><item type="color" name="alias">@color/red</item><dimen name="size">20dp</dimen><string name="path">M0,0h10v10H0z</string></resources>"##).expect("values XML");
        let xml = vector(
            "",
            r#"<path android:fillColor="@color/alias" android:pathData="@string/path"/>"#,
        )
        .replace("10dp", "@dimen/size");
        assert_eq!(pixel(&render(&xml, &resources), 128, 128), (255, 0, 0, 255));
    }

    #[test]
    fn applies_pivot_rotation_scale_and_translation_in_android_order() {
        let xml = vector(
            "",
            r##"<group android:pivotX="2" android:pivotY="2" android:rotation="90" android:scaleX="2" android:translateX="3"><path android:fillColor="#0f0" android:pathData="M2,2h2v1H2z"/></group>"##,
        );
        let image = render(&xml, &Resources::default());
        assert_eq!(pixel(&image, 115, 102), (0, 255, 0, 255));
        assert_eq!(pixel(&image, 160, 102), (0, 0, 0, 0));
    }

    #[test]
    fn clips_only_following_siblings_and_keeps_parent_clips_in_nested_groups() {
        let xml = vector(
            "",
            r##"<path android:fillColor="#f00" android:pathData="M0,0h2v10H0z"/><group><clip-path android:pathData="M4,0h2v10H4z"/><group><path android:fillColor="#0f0" android:pathData="M0,0h10v10H0z"/></group><path android:fillColor="#00f" android:pathData="M0,5h10v5H0z"/></group><path android:fillColor="#fff" android:pathData="M8,0h2v10H8z"/>"##,
        );
        let image = render(&xml, &Resources::default());
        assert_eq!(pixel(&image, 25, 25), (255, 0, 0, 255));
        assert_eq!(pixel(&image, 80, 25), (0, 0, 0, 0));
        assert_eq!(pixel(&image, 128, 25), (0, 255, 0, 255));
        assert_eq!(pixel(&image, 128, 200), (0, 0, 255, 255));
        assert_eq!(pixel(&image, 230, 25), (255, 255, 255, 255));
    }

    #[test]
    fn renders_even_odd_holes_and_clip_rules() {
        let xml = vector(
            "",
            r##"<path android:fillType="evenOdd" android:fillColor="#fff" android:pathData="M0,0h10v10H0z M2,2h6v6H2z"/>"##,
        );
        let image = render(&xml, &Resources::default());
        assert_eq!(pixel(&image, 128, 128), (0, 0, 0, 0));
        assert_eq!(pixel(&image, 25, 25), (255, 255, 255, 255));
    }

    #[test]
    fn tint_preserves_path_and_color_alpha() {
        let xml = vector(
            r##"android:tint="#8000ff00""##,
            r##"<path android:fillColor="#80ff0000" android:pathData="M2,2h6v6H2z"/>"##,
        );
        let image = render(&xml, &Resources::default());
        assert_eq!(pixel(&image, 128, 128), (0, 64, 0, 64));
        assert_eq!(pixel(&image, 25, 25), (0, 0, 0, 0));
    }

    #[test]
    fn uses_porter_duff_multiply_instead_of_svg_multiply_blending() {
        let xml = vector(
            r##"android:tint="#8000ff00" android:tintMode="multiply""##,
            r##"<path android:fillColor="#80ff0000" android:pathData="M2,2h6v6H2z"/>"##,
        );
        let image = render(&xml, &Resources::default());
        assert_eq!(pixel(&image, 128, 128), (0, 0, 0, 64));
        assert_eq!(pixel(&image, 25, 25), (0, 0, 0, 0));
    }

    #[test]
    fn supports_framework_colors_and_null_tint() {
        let xml = vector(
            r#"android:tint="@null""#,
            r#"<path android:fillColor="@android:color/white" android:pathData="M0,0h10v10H0z"/>"#,
        );
        assert_eq!(
            pixel(&render(&xml, &Resources::default()), 128, 128),
            (255, 255, 255, 255)
        );
    }

    #[test]
    fn renders_linear_and_radial_complex_color_gradients() {
        let xml = vector(
            "",
            r##"<path android:pathData="M0,0h10v10H0z"><aapt:attr name="android:fillColor"><gradient android:startX="0" android:endX="10"><item android:offset="0" android:color="#f00"/><item android:offset="1" android:color="#00f"/></gradient></aapt:attr></path>"##,
        );
        let image = render(&xml, &Resources::default());
        let (red, _, blue, alpha) = pixel(&image, 25, 128);
        assert!(red > 200 && blue < 30 && alpha == 255);
        let xml = vector(
            "",
            r##"<path android:pathData="M0,0h10v10H0z"><aapt:attr name="android:fillColor"><gradient android:type="radial" android:centerX="5" android:centerY="5" android:gradientRadius="5" android:startColor="#fff" android:endColor="#000"/></aapt:attr></path>"##,
        );
        let image = render(&xml, &Resources::default());
        assert!(pixel(&image, 128, 128).0 > 250);
        assert!(pixel(&image, 254, 128).0 < 5);
    }

    #[test]
    fn trims_strokes_with_wraparound_and_ignores_later_contours() {
        let xml = vector(
            "",
            r##"<path android:strokeColor="#f00" android:strokeWidth="1" android:trimPathStart="0.75" android:trimPathEnd="0.25" android:pathData="M0,5H10 M0,8H10"/>"##,
        );
        let image = render(&xml, &Resources::default());
        assert_eq!(pixel(&image, 25, 128), (255, 0, 0, 255));
        assert_eq!(pixel(&image, 128, 128), (0, 0, 0, 0));
        assert_eq!(pixel(&image, 230, 128), (255, 0, 0, 255));
        assert_eq!(pixel(&image, 25, 205), (0, 0, 0, 0));
        let path = BezPath::from_svg("M0,0C0,10 10,10 10,0").expect("cubic path");
        let trimmed = trim_path(&path, 0.25, 0.75, 0.);
        let length = trimmed
            .segments()
            .map(|segment| segment.arclen(1e-5))
            .sum::<f64>();
        assert!((length - 10.).abs() < 1e-3);
    }

    #[test]
    fn scales_strokes_like_android_under_nonuniform_group_and_viewport_scaling() {
        let xml = vector("", r##"<group android:scaleX="2"><path android:strokeColor="#f00" android:strokeWidth="1" android:pathData="M2,0V10"/></group>"##).replace("android:height=\"10dp\"", "android:height=\"5dp\"");
        let image = render(&xml, &Resources::default());
        assert_eq!(image.height(), 128);
        assert_eq!(pixel(&image, 102, 64), (255, 0, 0, 255));
        assert_eq!(pixel(&image, 112, 64), (0, 0, 0, 0));
    }

    #[test]
    fn handles_relative_arcs_and_resets_smooth_control_points_between_curve_types() {
        for (shorthand, explicit) in [
            ("M0,0 Q0,4 4,4 S8,8 10,0", "M0,0 Q0,4 4,4 C4,4 8,8 10,0"),
            ("M0,0 C0,4 4,4 4,0 T8,0", "M0,0 C0,4 4,4 4,0 Q4,0 8,0"),
            ("m2,2 a2,2 0 0,1 4,0", "M2,2 A2,2 0 0,1 6,2"),
        ] {
            let first = vector(
                "",
                &format!(r##"<path android:fillColor="#fff" android:pathData="{shorthand}"/>"##),
            );
            let second = vector(
                "",
                &format!(r##"<path android:fillColor="#fff" android:pathData="{explicit}"/>"##),
            );
            assert_eq!(
                to_svg(&first, &Resources::default()).expect("shorthand"),
                to_svg(&second, &Resources::default()).expect("explicit path")
            );
        }
    }

    #[test]
    fn reports_invalid_xml_paths_dimensions_and_unresolved_values() {
        for xml in [
            "<vector>",
            "<selector/>",
            &vector(
                "",
                r##"<path android:fillColor="#white" android:pathData="M0,0h10"/>"##,
            ),
            &vector(
                "",
                r##"<path android:fillColor="#fff" android:pathData="M0,0 X10,10"/>"##,
            ),
            &vector(
                "",
                r#"<path android:fillColor="@color/missing" android:pathData="M0,0h10"/>"#,
            ),
            &vector(
                "",
                r#"<path android:fillColor="?attr/colorControlNormal" android:pathData="M0,0h10"/>"#,
            ),
            &vector("", "").replace("10dp", "0dp"),
            &vector("", "").replace("viewportWidth=\"10\"", "viewportWidth=\"NaN\""),
        ] {
            assert!(
                to_svg(xml, &Resources::default()).is_err(),
                "accepted {xml}"
            );
        }
        let mut resources = Resources::default();
        resources
            .add_values_xml("<resources><color name=\"cycle\">@color/cycle</color></resources>")
            .expect("values XML");
        assert!(resources.resolve("@color/cycle").is_err());
        assert!(to_svg(&" ".repeat(MAX_XML_SIZE + 1), &resources).is_err());
    }

    #[test]
    fn rejects_sweep_gradients_and_external_entities_with_a_visible_error() {
        let xml = vector(
            "",
            r#"<path android:pathData="M0,0h10v10z"><aapt:attr name="android:fillColor"><gradient android:type="sweep"/></aapt:attr></path>"#,
        );
        assert!(
            to_svg(&xml, &Resources::default())
                .expect_err("unsupported sweep")
                .to_string()
                .contains("Sweep")
        );
        assert!(
            to_svg(
                "<!DOCTYPE vector [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><vector>&x;</vector>",
                &Resources::default()
            )
            .is_err()
        );
    }
}
