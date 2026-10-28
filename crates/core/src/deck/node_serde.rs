//! Image radius shares its JSON key with frame/shape radius, without changing
//! the existing kind payloads or their Rust pattern-matching API.
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::model::{Animation, Kind, Node, Placeholder, Position, Sizing};
use super::{Focus, Mask, Overlay, Shadow};

#[derive(Deserialize)]
#[serde(remote = "Node")]
struct NodeWire {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    slot: String,
    #[serde(default)]
    source_placeholder: Option<Placeholder>,
    #[serde(default)]
    template_background: bool,
    #[serde(default)]
    sizing: Sizing,
    #[serde(default)]
    position: Position,
    #[serde(default = "super::model::one")]
    opacity: f64,
    #[serde(default)]
    rotation: Option<f64>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    locked: bool,
    #[serde(default)]
    animation: Option<Animation>,
    #[serde(default)]
    shadow: Option<Shadow>,
    #[serde(default)]
    focus: Option<Focus>,
    #[serde(default)]
    mask: Option<Mask>,
    #[serde(default)]
    overlay: Option<Overlay>,
    #[serde(skip)]
    image_radius: Option<f64>,
    #[serde(flatten)]
    kind: Kind,
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = Value::deserialize(deserializer)?;
        let radius = if value["kind"] == "image" {
            value
                .as_object_mut()
                .and_then(|object| object.remove("radius"))
        } else {
            None
        };
        let mut node = NodeWire::deserialize(value).map_err(serde::de::Error::custom)?;
        node.image_radius = radius
            .map(serde_json::from_value)
            .transpose()
            .map_err(serde::de::Error::custom)?;
        Ok(node)
    }
}
