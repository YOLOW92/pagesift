use super::*;
use crate::schema::validate;

#[test]
fn definitions_are_well_formed() {
    let caps = Capabilities::basic();
    let defs = definitions(&caps);
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        [SCRAPE, MAP, CRAWL, POLL_JOB, CANCEL_JOB, BATCH_SCRAPE]
    );
    for d in &defs {
        assert_eq!(d.input_schema["type"], "object", "{}", d.name);
        assert!(d.description.is_some());
        assert!(d.output_schema.is_some(), "{} has no outputSchema", d.name);
    }
    assert!(interact_def(&["click".into()]).output_schema.is_some());
    assert!(interact_stop_def().output_schema.is_some());
    assert!(agent_def().output_schema.is_some());
    assert!(agent_status_def().output_schema.is_some());
    assert!(graph_query_def().output_schema.is_some());
    assert!(sync_def().output_schema.is_some());
    for d in &defs {
        let out = d.output_schema.as_ref().unwrap();
        assert_eq!(out["type"], "object", "{} outputSchema", d.name);
    }
    assert_eq!(scrape_def(&caps).input_schema["required"], json!(["url"]));
    assert!(
        crawl_def(&caps).input_schema["properties"]["scrapeOptions"]["properties"]["url"].is_null()
    );
}

/// 工具参数（含嵌套对象和数组元素）都有说明。
#[test]
fn every_tool_parameter_has_a_description() {
    fn walk(schema: &Value, path: &str) {
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            for (k, v) in props {
                let here = format!("{path}.{k}");
                let described = v
                    .get("description")
                    .and_then(Value::as_str)
                    .is_some_and(|d| !d.trim().is_empty());
                assert!(described, "{here} has no description");
                walk(v, &here);
            }
        }
        if let Some(items) = schema.get("items") {
            walk(items, &format!("{path}[]"));
        }
        if let Some(alts) = schema.get("anyOf").and_then(Value::as_array) {
            for (i, alt) in alts.iter().enumerate() {
                walk(alt, &format!("{path}<{i}>"));
            }
        }
    }
    let caps = Capabilities::basic();
    let mut defs = definitions(&caps);
    defs.push(interact_def(&["click".into()]));
    defs.push(interact_stop_def());
    defs.push(agent_def());
    defs.push(agent_status_def());
    defs.push(graph_query_def());
    defs.push(sync_def());
    for d in &defs {
        walk(&d.input_schema, &d.name);
    }
}

#[test]
fn firecrawl_style_arguments_validate() {
    let caps = Capabilities::basic();
    let (scrape_def, crawl_def, batch_scrape_def) = (
        || scrape_def(&caps),
        || crawl_def(&caps),
        || batch_scrape_def(&caps),
    );
    let ok = json!({
        "url": "https://example.com",
        "formats": ["markdown", "links"],
        "onlyMainContent": true,
        "waitFor": 500,
        "location": { "country": "US", "languages": ["en"] },
        "engine": "http"
    });
    validate(&scrape_def().input_schema, &ok).unwrap();
    let ok = json!({ "urls": ["https://a.example", "https://b.example"], "options": { "formats": ["html"] } });
    validate(&batch_scrape_def().input_schema, &ok).unwrap();
    let ok = json!({ "url": "https://example.com", "limit": 10, "scrapeOptions": { "formats": ["markdown"] } });
    validate(&crawl_def().input_schema, &ok).unwrap();

    assert!(
        validate(
            &scrape_def().input_schema,
            &json!({"url": "https://a", "formats": ["pdf"]})
        )
        .is_err()
    );
    assert!(validate(&batch_scrape_def().input_schema, &json!({"urls": []})).is_err());
    assert!(validate(&poll_job_def().input_schema, &json!({})).is_err());
    assert!(validate(&cancel_job_def().input_schema, &json!({})).is_err());
    validate(
        &sync_def().input_schema,
        &json!({"action": "create", "sourceId": "docs", "seeds": ["https://example.com/docs"]}),
    )
    .unwrap();
    validate(
        &graph_query_def().input_schema,
        &json!({"claims": true, "subject": "sku:pro", "asOf": "1000"}),
    )
    .unwrap();
}

/// crawl、map 的每个属性都是请求类型的真实字段。
#[test]
fn agent_properties_are_real_fields() {
    use pagesift_api::fields::names;
    use pagesift_api::types::AgentRequest;
    let def = agent_def();
    let props = def.input_schema["properties"].as_object().unwrap();
    for k in props.keys() {
        assert!(names::<AgentRequest>().contains(&k.as_str()), "{k}");
    }
}

#[test]
fn crawl_and_map_properties_are_real_fields() {
    use pagesift_api::fields::names;
    use pagesift_api::types::{CrawlRequest, MapRequest};
    let caps = Capabilities::basic();
    for (schema, fields) in [
        (crawl_def(&caps).input_schema, names::<CrawlRequest>()),
        (map_def().input_schema, names::<MapRequest>()),
    ] {
        for k in schema["properties"].as_object().unwrap().keys() {
            assert!(fields.contains(&k.as_str()), "{k}");
        }
    }
}

#[tokio::test]
async fn placeholders_return_examples() {
    let tools = placeholder_tools();
    let args = |v: Value| v.as_object().unwrap().clone();
    let out = tools[0]
        .call(
            args(json!({"url": "https://example.com"})),
            ToolContext::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        out.structured_content.unwrap()["metadata"]["statusCode"],
        200
    );
    let err = tools[3]
        .call(args(json!({"id": "nope"})), ToolContext::default())
        .await
        .unwrap_err();
    assert!(err.message.contains("Unknown job id"));
}

#[tokio::test]
async fn placeholder_outputs_match_schemas() {
    let tools = placeholder_tools();
    for (name, args) in [
        (SCRAPE, json!({"url":"https://example.com"})),
        (MAP, json!({"url":"https://example.com"})),
        (CRAWL, json!({"url":"https://example.com"})),
        (BATCH_SCRAPE, json!({"urls":["https://example.com"]})),
        (POLL_JOB, json!({"id":"placeholder-crawl"})),
        (CANCEL_JOB, json!({"id":"placeholder-crawl"})),
    ] {
        let tool = tools.iter().find(|t| t.definition().name == name).unwrap();
        let output = tool
            .call(args.as_object().unwrap().clone(), ToolContext::default())
            .await
            .unwrap();
        validate(
            tool.definition().output_schema.as_ref().unwrap(),
            output.structured_content.as_ref().unwrap(),
        )
        .unwrap();
    }
}
