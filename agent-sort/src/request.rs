use crate::config::{Preset, SortEntry, SortKey};
use serde_json::{json, Map, Value};

fn entry(e: &SortEntry) -> Value {
    let field = match &e.key {
        SortKey::Field(f) => json!(f),
        SortKey::Token(t) => json!({ "token": t }),
    };
    json!({ "field": field, "order": e.order() })
}

// One agent.view.set request; the filter goes through only when the preset has one. Params keep
// a fixed insertion order (filter, label, source, sort): serde_json keeps it, and a stable wire
// form makes the socket log readable.
pub fn set(plugin_id: &str, preset: &Preset) -> Value {
    let mut params = Map::new();
    if let Some(filter) = &preset.filter {
        params.insert("filter".into(), filter.clone());
    }
    params.insert("label".into(), json!(preset.name));
    params.insert("source".into(), json!(format!("plugin:{plugin_id}")));
    params.insert(
        "sort".into(),
        preset.sort.iter().map(entry).collect::<Vec<_>>().into(),
    );
    json!({ "id": "agent-sort", "method": "agent.view.set", "params": params })
}

pub fn clear(plugin_id: &str) -> Value {
    json!({
        "id": "agent-sort",
        "method": "agent.view.clear",
        "params": { "source": format!("plugin:{plugin_id}") },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SortKey;

    fn preset(name: &str, sort: Vec<SortEntry>, filter: Option<Value>) -> Preset {
        Preset {
            name: name.into(),
            sort,
            filter,
        }
    }

    #[test]
    fn a_set_request_holds_source_label_sort_and_optional_filter() {
        let p = preset(
            "prio",
            vec![
                SortEntry {
                    key: SortKey::Token("prio".into()),
                    desc: true,
                },
                SortEntry {
                    key: SortKey::Field("attention".into()),
                    desc: true,
                },
                SortEntry {
                    key: SortKey::Field("state_change_seq".into()),
                    desc: false,
                },
            ],
            Some(json!({"op": "in", "field": "status", "values": ["blocked", "done"]})),
        );
        assert_eq!(
            set("test.agent-sort", &p),
            json!({
                "id": "agent-sort",
                "method": "agent.view.set",
                "params": {
                    "source": "plugin:test.agent-sort",
                    "label": "prio",
                    "sort": [
                        {"field": {"token": "prio"}, "order": "desc"},
                        {"field": "attention", "order": "desc"},
                        {"field": "state_change_seq", "order": "asc"},
                    ],
                    "filter": {"op": "in", "field": "status", "values": ["blocked", "done"]},
                },
            })
        );
        let wire = serde_json::to_string(&set("test.agent-sort", &p)).unwrap_or_default();
        assert_eq!(
            wire,
            "{\"id\":\"agent-sort\",\"method\":\"agent.view.set\",\"params\":{\"filter\":{\"op\":\"in\",\"field\":\"status\",\"values\":[\"blocked\",\"done\"]},\"label\":\"prio\",\"source\":\"plugin:test.agent-sort\",\"sort\":[{\"field\":{\"token\":\"prio\"},\"order\":\"desc\"},{\"field\":\"attention\",\"order\":\"desc\"},{\"field\":\"state_change_seq\",\"order\":\"asc\"}]}}"
        );
        let no_filter = preset(
            "repo",
            vec![SortEntry {
                key: SortKey::Token("repo".into()),
                desc: false,
            }],
            None,
        );
        let params = &set("test.agent-sort", &no_filter)["params"];
        assert_eq!(
            params["sort"],
            json!([{"field": {"token": "repo"}, "order": "asc"}])
        );
        assert!(params.get("filter").is_none());
    }

    #[test]
    fn a_clear_request_names_only_the_source() {
        assert_eq!(
            clear("test.agent-sort"),
            json!({
                "id": "agent-sort",
                "method": "agent.view.clear",
                "params": {"source": "plugin:test.agent-sort"},
            })
        );
    }
}
