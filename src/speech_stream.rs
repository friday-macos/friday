use serde_json::Value;
use std::collections::HashMap;

#[derive(Default)]
pub struct SpeechStream {
    items: HashMap<String, Item>,
}

#[derive(Default)]
struct Item {
    text: String,
    spoken: usize,
}

impl SpeechStream {
    pub fn accept(&mut self, method: &str, params: &Value) -> Option<String> {
        match method {
            "item/started" => {
                let item = &params["item"];
                if speakable(item) {
                    self.items
                        .entry(item["id"].as_str()?.to_owned())
                        .or_default();
                }
                None
            }
            "item/agentMessage/delta" => {
                let item = self.items.get_mut(params["itemId"].as_str()?)?;
                item.text.push_str(params["delta"].as_str()?);
                let pending = &item.text[item.spoken..];
                let boundary = sentence_end(pending)?;
                item.spoken += boundary;
                Some(pending[..boundary].trim().to_owned())
            }
            "item/completed" => {
                let final_item = &params["item"];
                let id = final_item["id"].as_str()?;
                let item = self.items.remove(id);
                if !speakable(final_item) {
                    return None;
                }
                let final_text = final_item["text"].as_str()?;
                let tail = match item {
                    Some(item) if item.spoken > 0 => {
                        final_text.strip_prefix(&item.text[..item.spoken])?
                    }
                    _ => final_text,
                };
                let tail = tail.trim();
                (!tail.is_empty()).then(|| tail.to_owned())
            }
            _ => None,
        }
    }
}

fn speakable(item: &Value) -> bool {
    item["type"] == "agentMessage"
        && item["phase"] == "final_answer"
        && item["delivery"].is_null()
        && item["moderation"]["moderated"] != true
}

fn sentence_end(pending: &str) -> Option<usize> {
    let trimmed = pending.trim_start();
    let body = trimmed
        .strip_prefix("[실행 결과물]")
        .map_or(trimmed, str::trim_start);
    let lead = pending.len() - body.len();
    // Formatted text may span several deltas. Let the completed-item renderer handle it.
    let mut chars = body.char_indices().peekable();
    let mut count = 0;
    while let Some((index, ch)) = chars.next() {
        if matches!(ch, '`' | '[' | '<' | '#') {
            return None;
        }
        count += 1;
        if count >= 22
            && ".!?。！？".contains(ch)
            && chars.peek().is_some_and(|(_, next)| next.is_whitespace())
        {
            return Some(lead + index + ch.len_utf8());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_final_prose_streams_and_the_completed_item_does_not_repeat_it() {
        let mut stream = SpeechStream::default();
        let commentary = json!({"item":{"id":"note","type":"agentMessage","phase":"commentary"}});
        stream.accept("item/started", &commentary);
        let first = "산책은 잠깐이라도 매일 시간을 정해 하면 주의를 환기하는 데 도움이 돼.";
        let prefix = "[실행 결과물]\n\n";
        let delta = format!("{prefix}{first} 다음으로 ");
        assert!(
            stream
                .accept(
                    "item/agentMessage/delta",
                    &json!({"itemId":"note","delta":delta})
                )
                .is_none()
        );
        stream.accept(
            "item/started",
            &json!({"item":{"id":"answer","type":"agentMessage","phase":"final_answer"}}),
        );
        assert_eq!(
            stream
                .accept(
                    "item/agentMessage/delta",
                    &json!({"itemId":"answer","delta":delta})
                )
                .as_deref(),
            Some(format!("{prefix}{first}").as_str())
        );
        let ending = "다음으로 신발을 신어 보자.";
        let complete = json!({"item":{"id":"answer","type":"agentMessage","phase":"final_answer","text":format!("{prefix}{first} {ending}")}});
        assert_eq!(
            stream.accept("item/completed", &complete).as_deref(),
            Some(ending)
        );
    }
}
