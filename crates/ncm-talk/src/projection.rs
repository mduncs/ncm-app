//! Serde skips unknown fields while streaming, instead of allocating snapshot/tool-output trees.
use serde::{
    de::{self, MapAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::Value;
use std::fmt;

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Record {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uuid: Option<String>,
    #[serde(rename = "parentUuid", skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    #[serde(rename = "isSidechain", skip_serializing_if = "Option::is_none")]
    sidechain: Option<bool>,
    #[serde(rename = "isCompactSummary", skip_serializing_if = "Option::is_none")]
    compact_summary: Option<bool>,
    #[serde(rename = "isMeta", skip_serializing_if = "Option::is_none")]
    meta: Option<bool>,
    #[serde(rename = "turnOrigin", skip_serializing_if = "Option::is_none")]
    turn_origin: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<Value>,
    #[serde(rename = "promptSource", skip_serializing_if = "Option::is_none")]
    prompt_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attachment: Option<Attachment>,
    #[serde(rename = "toolUseResult", skip_serializing_if = "Option::is_none")]
    result: Option<ResultFields>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<Payload>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Message {
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<Value>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

impl<'de> Deserialize<'de> for Content {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ContentVisitor;
        impl<'de> Visitor<'de> for ContentVisitor {
            type Value = Content;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("message text or content blocks")
            }
            fn visit_str<E: de::Error>(self, text: &str) -> Result<Self::Value, E> {
                Ok(Content::Text(display_body(text)))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut blocks = Vec::new();
                while let Some(block) = seq.next_element::<Block>()? {
                    blocks.push(block);
                }
                Ok(Content::Blocks(blocks))
            }
        }
        deserializer.deserialize_any(ContentVisitor)
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Block {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<DisplayBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<InputFields>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<ResultFields>,
    // tool_result.content keeps only agent identity; thinking/signatures/images are skipped.
}

fn display_body(text: &str) -> String {
    // Scan the entire original string before clipping: report markers can follow a long prefix.
    if text.len() <= 16 * 1024 || crate::adapters::content_report(text) {
        return text.into();
    }
    let mut end = 16 * 1024;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n… display clipped; full text remains in transcript",
        &text[..end]
    )
}

#[derive(Serialize)]
#[serde(transparent)]
struct DisplayBody(String);
impl<'de> Deserialize<'de> for DisplayBody {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BodyVisitor;
        impl<'de> Visitor<'de> for BodyVisitor {
            type Value = DisplayBody;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("message body text")
            }
            fn visit_str<E: de::Error>(self, text: &str) -> Result<Self::Value, E> {
                Ok(DisplayBody(display_body(text)))
            }
        }
        deserializer.deserialize_str(BodyVisitor)
    }
}

// Tool prompts, file contents and patches are not presentation data. In particular, a
// historical Agent prompt can be megabytes long; serde skips it without allocating a tree.
#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct InputFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<DisplayText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_path: Option<DisplayText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<DisplayText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pattern: Option<DisplayText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<DisplayText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<DisplayText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<DisplayText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_name: Option<DisplayText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<DisplayText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_type: Option<DisplayText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    team_name: Option<DisplayText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_in_background: Option<Value>,
}

#[derive(Serialize)]
#[serde(transparent)]
struct DisplayText(String);
impl<'de> Deserialize<'de> for DisplayText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TextVisitor;
        impl<'de> Visitor<'de> for TextVisitor {
            type Value = DisplayText;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("tool label text")
            }
            fn visit_str<E: de::Error>(self, text: &str) -> Result<Self::Value, E> {
                Ok(DisplayText(text.chars().take(512).collect()))
            }
        }
        deserializer.deserialize_str(TextVisitor)
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Attachment {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<Value>,
    #[serde(rename = "commandMode", skip_serializing_if = "Option::is_none")]
    command_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<Value>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Payload {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    info: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<ResultFields>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct ResultObject {
    #[serde(
        rename = "agentId",
        alias = "agent_id",
        skip_serializing_if = "Option::is_none"
    )]
    agent_id: Option<String>,
    #[serde(rename = "taskId", skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(alias = "nickname", skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<Value>,
}

#[derive(Default, Serialize)]
struct ResultFields(ResultObject);

impl<'de> Deserialize<'de> for ResultFields {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SummaryVisitor;
        impl<'de> Visitor<'de> for SummaryVisitor {
            type Value = ResultFields;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("tool result object or text")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                ResultObject::deserialize(de::value::MapAccessDeserializer::new(map))
                    .map(ResultFields)
            }
            fn visit_str<E: de::Error>(self, text: &str) -> Result<Self::Value, E> {
                let mut fields = serde_json::from_str::<ResultObject>(text).unwrap_or_default();
                if fields.agent_id.is_none() {
                    fields.agent_id = text
                        .split("agentId:")
                        .nth(1)
                        .and_then(|s| s.split_whitespace().next())
                        .map(str::to_owned);
                }
                Ok(ResultFields(fields))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<de::IgnoredAny>()?.is_some() {}
                Ok(ResultFields::default())
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(ResultFields::default())
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(ResultFields::default())
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(ResultFields::default())
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(ResultFields::default())
            }
        }
        deserializer.deserialize_any(SummaryVisitor)
    }
}
