use android_tools::{
    import_facts::{ImportFactsBinding, ImportFactsSnapshot, parse_import_facts},
    kotlin_import_facts::{
        CaptureContext, CaptureLimits, CaptureValue, GetterOutcome, KotlinFactsSnapshot,
        parse_kotlin_facts,
    },
    project_model::{ProjectModel, VariantId, parse_model},
    project_tree_facts::FactsUnavailableReason,
};
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use std::fs;

fn path_aliases(value: &str) -> Result<Vec<String>> {
    let path = std::path::Path::new(value);
    let parent = path
        .parent()
        .and_then(|parent| parent.to_str())
        .context("UTF-8 fixture parent")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("UTF-8 fixture path name")?;
    let separator = std::path::MAIN_SEPARATOR;
    let mut aliases = vec![
        format!("{parent}{separator}.{separator}{name}"),
        format!("{parent}{separator}{separator}{name}"),
        format!("{value}{separator}"),
    ];
    if cfg!(windows) {
        aliases.push(value.replace('\\', "/"));
    }
    Ok(aliases)
}
fn provenance_path_pointers(fixture: &Fixture) -> Result<Vec<String>> {
    let mut pointers = vec![
        "/kotlinFacts/root".to_string(),
        "/kotlinFacts/context/imports/buildIdentity/rootDirectory/result/value".to_string(),
    ];
    for (section, field) in [
        ("/kotlinFacts/modules", "directory"),
        ("/kotlinFacts/context/runtime/artifacts", "path"),
    ] {
        for ordinal in 0..fixture
            .wire
            .pointer(section)
            .and_then(Value::as_array)
            .context("Fixture provenance entries")?
            .len()
        {
            pointers.push(format!("{section}/{ordinal}/{field}"));
        }
    }
    let projects = "/kotlinFacts/context/imports/projectCatalogue/result/value";
    for ordinal in 0..fixture
        .wire
        .pointer(projects)
        .and_then(Value::as_array)
        .context("Fixture projects")?
        .len()
    {
        for field in ["projectDirectory", "rootDirectory"] {
            pointers.push(format!("{projects}/{ordinal}/{field}/result/value"));
        }
    }
    Ok(pointers)
}

struct Fixture {
    _directory: tempfile::TempDir,
    wire: Value,
    model: ProjectModel,
    imports: ImportFactsSnapshot,
    expected: CaptureContext,
}
fn replace_root(value: &mut Value, root: &str) {
    match value {
        Value::String(value) => *value = value.replace("$ROOT", root),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| replace_root(value, root)),
        Value::Object(values) => values
            .values_mut()
            .for_each(|value| replace_root(value, root)),
        _ => {}
    }
}
fn model_output(value: &Value) -> Result<String> {
    Ok(format!(
        "KODA_ANDROID_PROJECT_MODEL={}",
        serde_json::to_string(value)?
    ))
}
fn import_binding() -> ImportFactsBinding {
    ImportFactsBinding {
        model_revision: 7,
        selection_revision: 3,
        selected_variants: vec![
            VariantId {
                module: ":android".into(),
                variant: "debug".into(),
            },
            VariantId {
                module: ":nested:library".into(),
                variant: "jvm".into(),
            },
        ],
    }
}
fn synthetic_fixture() -> Result<Fixture> {
    let directory = tempfile::Builder::new()
        .prefix("kotlin-capture-")
        .tempdir()?;
    let root = directory.path().canonicalize()?;
    for name in ["android", "strange-parent", "shared-directory"] {
        fs::create_dir(root.join(name))?;
    }
    let mut wire: Value =
        serde_json::from_str(include_str!("../test_data/import_facts/wire-template.json"))?;
    replace_root(&mut wire, root.to_str().context("UTF-8 fixture root")?);
    let model = parse_model(&model_output(&wire)?, &root)?;
    let imports = parse_import_facts(&model_output(&wire)?, &model, import_binding())?;
    let mut packet: Value = serde_json::from_str(include_str!(
        "../test_data/kotlin_import_facts/protocol-template.json"
    ))?;
    replace_root(&mut packet, root.to_str().context("UTF-8 fixture root")?);
    packet["modules"] = wire["importFacts"]["modules"].clone();
    packet["context"]["imports"] = json!({
        "buildIdentity": wire["importFacts"]["buildIdentity"],
        "projectCatalogue": wire["importFacts"]["projectCatalogue"],
    });
    let expected = serde_json::from_value(packet["context"].clone())?;
    wire["kotlinFacts"] = packet;
    Ok(Fixture {
        _directory: directory,
        wire,
        model,
        imports,
        expected,
    })
}
fn decode_with(fixture: &Fixture, limits: CaptureLimits) -> Result<KotlinFactsSnapshot> {
    Ok(parse_kotlin_facts(
        &model_output(&fixture.wire)?,
        &fixture.model,
        &fixture.imports,
        &fixture.expected,
        limits,
    )?)
}
fn decode(fixture: &Fixture) -> Result<KotlinFactsSnapshot> {
    decode_with(fixture, CaptureLimits::default())
}
fn rejects(fixture: &Fixture, reason: FactsUnavailableReason) -> Result<()> {
    let error = parse_kotlin_facts(
        &model_output(&fixture.wire)?,
        &fixture.model,
        &fixture.imports,
        &fixture.expected,
        CaptureLimits::default(),
    )
    .expect_err("Invalid synthetic capture cannot be accepted");
    assert_eq!(error.reason, reason, "{error}");
    Ok(())
}
// Mutating both contracts tests structural validation; mutating only the packet
// tests rebinding against the independently retained request plan.
fn change_plan(fixture: &mut Fixture) -> Result<()> {
    fixture.expected = serde_json::from_value(fixture.wire["kotlinFacts"]["context"].clone())?;
    Ok(())
}
fn packet(fixture: &mut Fixture) -> &mut Value {
    &mut fixture.wire["kotlinFacts"]
}
fn outcome(fixture: &mut Fixture, ordinal: usize, value: Value) {
    let container = if value["status"] == "available"
        && matches!(value["value"]["kind"].as_str(), Some("strings" | "objects"))
    {
        json!("list-container")
    } else {
        Value::Null
    };
    packet(fixture)["events"][ordinal]["container"] = container;
    packet(fixture)["events"][ordinal]["outcome"] = value;
}
fn available(kind: &str, value: Value) -> Value {
    json!({"status":"available","value":{"kind":kind,"value":value}})
}
fn unavailable(kind: &str, stage: &str) -> Value {
    json!({"status":"unavailable","value":{"kind":kind,"stage":stage,"capability":"synthetic getter","detail":"original failure retained","actualClass":null,
        "exceptions":[{"class":"java.lang.reflect.InvocationTargetException","message":null},{"class":"synthetic.UninitializedProperty","message":"original message"}]}})
}
fn only_first(fixture: &mut Fixture) -> Result<()> {
    packet(fixture)["context"]["requests"]
        .as_array_mut()
        .context("Requests")?
        .truncate(1);
    packet(fixture)["events"]
        .as_array_mut()
        .context("Events")?
        .truncate(1);
    Ok(())
}
fn list_fixture() -> Result<Fixture> {
    let mut fixture = synthetic_fixture()?;
    only_first(&mut fixture)?;
    let request = &mut packet(&mut fixture)["context"]["requests"][0];
    request["owner"] = "project-object".into();
    request["catalogue"] = "projects".into();
    request["method"] = json!({"kind":"selected","value":"task-list"});
    request["purpose"] = "raw".into();
    request["returnShape"]["kind"] = "strings".into();
    request["returnShape"]["order"] = "list".into();
    outcome(&mut fixture, 0, available("strings", json!([])));
    change_plan(&mut fixture)?;
    Ok(fixture)
}
fn property_fixture() -> Result<Fixture> {
    let mut fixture = synthetic_fixture()?;
    let requests = &mut packet(&mut fixture)["context"]["requests"];
    requests[0]["method"] = json!({"kind":"selected","value":"source-property"});
    requests[0]["returnShape"] =
        json!({"kind":"object","nullable":true,"objectKind":"property","order":null});
    requests[1]["owner"] = "property-object".into();
    requests[1]["catalogue"] = "properties".into();
    requests[1]["method"] = json!({"kind":"selected","value":"property-get"});
    requests[1]["purpose"] = "propertyGet".into();
    requests[1]["after"] = "source-first".into();
    outcome(
        &mut fixture,
        0,
        available("object", json!("property-object")),
    );
    change_plan(&mut fixture)?;
    Ok(fixture)
}
fn resolver_fixture() -> Result<Fixture> {
    let mut fixture = synthetic_fixture()?;
    let requests = &mut packet(&mut fixture)["context"]["requests"];
    requests[0]["owner"] = Value::Null;
    requests[0]["catalogue"] = "resolvers".into();
    requests[0]["method"] = json!({"kind":"selected","value":"resolver-instance"});
    requests[0]["arguments"] = json!([{"kind":"object","value":"project-object"}]);
    requests[0]["returnShape"] =
        json!({"kind":"object","nullable":true,"objectKind":"resolver","order":null});
    requests[0]["purpose"] = "resolverInstance".into();
    requests[1]["owner"] = "resolver-object".into();
    requests[1]["catalogue"] = "resolvers".into();
    requests[1]["method"] = json!({"kind":"selected","value":"arguments"});
    requests[1]["arguments"] = json!([{"kind":"object","value":"first-task"}]);
    requests[1]["returnShape"] =
        json!({"kind":"strings","nullable":true,"objectKind":null,"order":"list"});
    requests[1]["purpose"] = "compilerArguments".into();
    requests[1]["after"] = "source-first".into();
    outcome(
        &mut fixture,
        0,
        available("object", json!("resolver-object")),
    );
    outcome(
        &mut fixture,
        1,
        available(
            "strings",
            json!([
                "-P",
                "plugin:a=b c",
                "-P",
                "plugin:quote=\"x\"",
                "",
                "𐀀",
                "-Xdummy"
            ]),
        ),
    );
    change_plan(&mut fixture)?;
    Ok(fixture)
}

fn ordered_map_fixture(task_map: bool) -> Result<Fixture> {
    let mut fixture = synthetic_fixture()?;
    for class in [
        json!({"id":"map","name":"java.util.Map","loader":"bootstrap","origin":{"kind":"jdk","value":"java.base"},"superclass":null,"interfaces":[]}),
        json!({"id":"map-implementation","name":"synthetic.OrderedMap","loader":"kgp","origin":{"kind":"artifact","value":"kgp-jar"},"superclass":"object","interfaces":["map"]}),
        json!({"id":"named-interface","name":"org.gradle.api.NamedDomainObjectContainer","loader":"gradle","origin":{"kind":"artifact","value":"gradle-jar"},"superclass":null,"interfaces":[]}),
        json!({"id":"named-implementation","name":"synthetic.NamedContainer","loader":"kgp","origin":{"kind":"artifact","value":"kgp-jar"},"superclass":"object","interfaces":["named-interface"]}),
    ] {
        packet(&mut fixture)["context"]["runtime"]["classes"]
            .as_array_mut()
            .context("Classes")?
            .push(class);
    }
    for (id, class_id) in [
        ("map-container", "map-implementation"),
        ("second-map-container", "map-implementation"),
        ("named-container", "named-implementation"),
    ] {
        packet(&mut fixture)["context"]["objects"]
            .as_array_mut()
            .context("Objects")?
            .push(json!({
                "id":id,"kind":"container","project":":android","classId":class_id,"task":null
            }));
    }
    packet(&mut fixture)["context"]["catalogues"].as_array_mut().context("Catalogues")?.extend([
        json!({"id":"named-container","classId":"named-implementation","methods":[
            {"id":"as-map","name":"getAsMap","descriptor":"()Ljava/util/Map;","declaringClass":"named-implementation","isStatic":false,"parameterClasses":[],"returnClass":"map"}
        ]}),
        json!({"id":"map-container","classId":"map-implementation","methods":[
            {"id":"map-values","name":"values","descriptor":"()Ljava/util/List;","declaringClass":"map-implementation","isStatic":false,"parameterClasses":[],"returnClass":"list"},
            {"id":"map-get","name":"get","descriptor":"(Ljava/lang/Object;)Ljava/lang/Object;","declaringClass":"map-implementation","isStatic":false,"parameterClasses":["object"],"returnClass":"object"}
        ]}),
    ]);
    let mut first = packet(&mut fixture)["context"]["requests"][0].clone();
    first["owner"] = "project-object".into();
    first["catalogue"] = "projects".into();
    first["purpose"] = "raw".into();
    first["returnShape"] =
        json!({"kind":"object","nullable":true,"objectKind":"container","order":null});
    first["variant"] = Value::Null;
    let mut second = first.clone();
    second["id"] = "source-second".into();
    second["after"] = "source-first".into();
    let mut requests = vec![first];
    let mut events = Vec::new();
    if task_map {
        packet(&mut fixture)["context"]["catalogues"][3]["methods"].as_array_mut().context("Project methods")?.push(json!({
            "id":"all-tasks","name":"getAllTasks","descriptor":"(Z)Ljava/util/Map;","declaringClass":"project","isStatic":false,"parameterClasses":[null],"returnClass":"map"
        }));
        requests[0]["method"]["value"] = "all-tasks".into();
        requests[0]["arguments"] = json!([{"kind":"boolean","value":false}]);
        events.push(json!({"id":"event-first","request":"source-first","container":null,"outcome":available("object",json!("map-container"))}));
        second["owner"] = "map-container".into();
        second["catalogue"] = "map-container".into();
        second["method"]["value"] = "map-get".into();
        second["arguments"] = json!([{"kind":"object","value":"project-object"}]);
        second["purpose"] = "containerIterate".into();
        second["returnShape"] = json!({"kind":"objects","nullable":true,"objectKind":"task","order":"projectTaskMapValues"});
        requests.push(second);
        events.push(json!({"id":"event-second","request":"source-second","container":"list-container","outcome":available("objects",json!(["second-task","first-task","second-task"]))}));
    } else {
        packet(&mut fixture)["context"]["catalogues"][3]["methods"].as_array_mut().context("Project methods")?.push(json!({
            "id":"named-container-getter","name":"syntheticNamedContainer","descriptor":"()Ljava/lang/Object;","declaringClass":"project","isStatic":false,"parameterClasses":[],"returnClass":"object"
        }));
        requests[0]["method"]["value"] = "named-container-getter".into();
        events.push(json!({"id":"event-first","request":"source-first","container":null,"outcome":available("object",json!("named-container"))}));
        second["owner"] = "named-container".into();
        second["catalogue"] = "named-container".into();
        second["method"]["value"] = "as-map".into();
        second["purpose"] = "containerGet".into();
        requests.push(second.clone());
        events.push(json!({"id":"event-second","request":"source-second","container":null,"outcome":available("object",json!("map-container"))}));
        let mut third = second;
        third["id"] = "iterate-values".into();
        third["owner"] = "map-container".into();
        third["catalogue"] = "map-container".into();
        third["method"]["value"] = "map-values".into();
        third["purpose"] = "containerIterate".into();
        third["after"] = "source-second".into();
        third["returnShape"] = json!({"kind":"objects","nullable":true,"objectKind":"task","order":"namedDomainObjectAsMapValues"});
        requests.push(third);
        events.push(json!({"id":"values-event","request":"iterate-values","container":"list-container","outcome":available("objects",json!(["second-task","first-task","second-task"]))}));
    }
    packet(&mut fixture)["context"]["requests"] = requests.into();
    packet(&mut fixture)["events"] = events.into();
    change_plan(&mut fixture)?;
    Ok(fixture)
}

fn missing_method_fixture() -> Result<Fixture> {
    let mut fixture = synthetic_fixture()?;
    let request = &mut packet(&mut fixture)["context"]["requests"][0];
    request["method"] = json!({"kind":"missing","value":{
        "name":"uncapturedGetter","descriptor":"()Ljava/lang/String;","isStatic":false
    }});
    request["purpose"] = "raw".into();
    outcome(
        &mut fixture,
        0,
        unavailable("missingMethod", "methodDiscovery"),
    );
    change_plan(&mut fixture)?;
    Ok(fixture)
}
fn nested_fixture(pointer: &str) -> Result<Fixture> {
    if pointer.contains("arguments/") {
        resolver_fixture()
    } else if pointer.ends_with("method/value") {
        missing_method_fixture()
    } else {
        let mut fixture = synthetic_fixture()?;
        if pointer.contains("outcome/value/") || pointer.ends_with("outcome/value") {
            outcome(&mut fixture, 0, unavailable("invocation", "invoke"));
        }
        Ok(fixture)
    }
}
fn rejects_raw_member(fixture: &Fixture, pointer: &str, replacement: &str) -> Result<()> {
    let member = fixture.wire.pointer(pointer).context("Raw member")?;
    let member = serde_json::to_string(member)?;
    let original = model_output(&fixture.wire)?;
    assert!(
        original.contains(&member),
        "Original wire member must be present"
    );
    let mutated = original.replacen(&member, replacement, 1);
    let error = parse_kotlin_facts(
        &mutated,
        &fixture.model,
        &fixture.imports,
        &fixture.expected,
        CaptureLimits::default(),
    )
    .expect_err("Duplicate literal members cannot normalize into raw evidence");
    assert_eq!(error.reason, FactsUnavailableReason::Malformed, "{error}");
    Ok(())
}

#[test]
fn observation_presence_states() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    for value in [
        json!({"status":"available","value":null}),
        available("string", json!("")),
        unavailable("invocation", "invoke"),
    ] {
        outcome(&mut fixture, 0, value);
        decode(&fixture)?;
    }
    let snapshot = decode(&fixture)?;
    let null = &snapshot.raw_events()[0].outcome;
    assert!(matches!(null, GetterOutcome::Unavailable(_)));
    outcome(&mut fixture, 0, json!({"status":"available","value":null}));
    assert_eq!(
        decode(&fixture)?.raw_events()[0].outcome,
        GetterOutcome::Available(None)
    );
    outcome(&mut fixture, 0, Value::Null);
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = list_fixture()?;
    assert_eq!(
        decode(&fixture)?.raw_events()[0].outcome,
        GetterOutcome::Available(Some(CaptureValue::Strings(vec![])))
    );
    outcome(&mut fixture, 0, json!({"status":"available","value":null}));
    assert_eq!(
        decode(&fixture)?.raw_events()[0].outcome,
        GetterOutcome::Available(None)
    );
    Ok(())
}
#[test]
fn nonnullable_endpoint_null() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["requests"][0]["returnShape"]["nullable"] = false.into();
    change_plan(&mut fixture)?;
    outcome(&mut fixture, 0, json!({"status":"available","value":null}));
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = list_fixture()?;
    outcome(
        &mut fixture,
        0,
        available("strings", json!(["valid", null])),
    );
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn object_only_every_layer() -> Result<()> {
    for pointer in [
        "/kotlinFacts",
        "/kotlinFacts/context",
        "/kotlinFacts/context/binding",
        "/kotlinFacts/context/binding/selectedVariants/0",
        "/kotlinFacts/context/imports",
        "/kotlinFacts/context/imports/buildIdentity",
        "/kotlinFacts/context/imports/projectCatalogue",
        "/kotlinFacts/context/runtime",
        "/kotlinFacts/context/runtime/java",
        "/kotlinFacts/context/runtime/locale",
        "/kotlinFacts/context/runtime/artifacts/0",
        "/kotlinFacts/context/runtime/loaders/0",
        "/kotlinFacts/context/runtime/classes/0",
        "/kotlinFacts/context/runtime/classes/0/origin",
        "/kotlinFacts/context/objects/0",
        "/kotlinFacts/context/objects/2/task",
        "/kotlinFacts/context/catalogues/0",
        "/kotlinFacts/context/catalogues/0/methods/0",
        "/kotlinFacts/context/requests/0",
        "/kotlinFacts/context/requests/0/method",
        "/kotlinFacts/context/requests/0/method/value",
        "/kotlinFacts/context/requests/0/arguments/0",
        "/kotlinFacts/context/requests/0/variant",
        "/kotlinFacts/context/requests/0/returnShape",
        "/kotlinFacts/context/requests/0/parameter",
        "/kotlinFacts/events/0",
        "/kotlinFacts/events/0/outcome",
        "/kotlinFacts/events/0/outcome/value",
        "/kotlinFacts/events/1/outcome/value",
        "/kotlinFacts/events/0/outcome/value/exceptions/0",
        "/kotlinFacts/modules/0",
    ] {
        let mut fixture = nested_fixture(pointer)?;
        let field = fixture
            .wire
            .pointer_mut(pointer)
            .context("Present fixture object")?;
        *field = Value::Array(
            field
                .as_object()
                .context("Object")?
                .values()
                .cloned()
                .collect(),
        );
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    Ok(())
}
#[test]
fn unknown_fields_every_layer() -> Result<()> {
    for pointer in [
        "/kotlinFacts",
        "/kotlinFacts/context",
        "/kotlinFacts/context/binding",
        "/kotlinFacts/context/binding/selectedVariants/0",
        "/kotlinFacts/context/imports",
        "/kotlinFacts/context/imports/buildIdentity",
        "/kotlinFacts/context/imports/projectCatalogue",
        "/kotlinFacts/context/runtime",
        "/kotlinFacts/context/runtime/java",
        "/kotlinFacts/context/runtime/locale",
        "/kotlinFacts/context/runtime/artifacts/0",
        "/kotlinFacts/context/runtime/loaders/0",
        "/kotlinFacts/context/runtime/classes/0",
        "/kotlinFacts/context/runtime/classes/0/origin",
        "/kotlinFacts/context/objects/0",
        "/kotlinFacts/context/objects/2/task",
        "/kotlinFacts/context/catalogues/0",
        "/kotlinFacts/context/catalogues/0/methods/0",
        "/kotlinFacts/context/requests/0",
        "/kotlinFacts/context/requests/0/method",
        "/kotlinFacts/context/requests/0/method/value",
        "/kotlinFacts/context/requests/0/arguments/0",
        "/kotlinFacts/context/requests/0/variant",
        "/kotlinFacts/context/requests/0/parameter",
        "/kotlinFacts/context/requests/0/returnShape",
        "/kotlinFacts/modules/0",
        "/kotlinFacts/events/0",
        "/kotlinFacts/events/0/outcome",
        "/kotlinFacts/events/0/outcome/value",
        "/kotlinFacts/events/1/outcome/value",
        "/kotlinFacts/events/0/outcome/value/exceptions/0",
    ] {
        let mut fixture = nested_fixture(pointer)?;
        fixture
            .wire
            .pointer_mut(pointer)
            .context("Fixture object")?["invented"] = true.into();
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    Ok(())
}
#[test]
fn required_explicit_outcome_value() -> Result<()> {
    for status in ["available", "unavailable", "invented"] {
        let mut fixture = synthetic_fixture()?;
        outcome(&mut fixture, 0, json!({"status":status}));
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["requests"][0]["parameter"] = json!({"kind":"explicit"});
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    for (pointer, field) in [
        ("/kotlinFacts/context/runtime/artifacts/0", "version"),
        ("/kotlinFacts/context/runtime/loaders/0", "parent"),
        ("/kotlinFacts/context/runtime/classes/0", "superclass"),
        ("/kotlinFacts/context/objects/0", "task"),
        ("/kotlinFacts/context/requests/0", "after"),
        ("/kotlinFacts/context/requests/0/returnShape", "order"),
        ("/kotlinFacts/events/0", "container"),
    ] {
        let mut fixture = synthetic_fixture()?;
        fixture
            .wire
            .pointer_mut(pointer)
            .context("Nullable field")?
            .as_object_mut()
            .context("Object")?
            .remove(field);
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    let fixture = synthetic_fixture()?;
    for replacement in [
        r#"{"status":"unavailable","status":"available","value":{"kind":"string","value":"debug"}}"#,
        r#"{"status":"available","value":null,"value":{"kind":"string","value":"debug"}}"#,
        r#"{"status":"available","st\u0061tus":"available","value":{"kind":"string","value":"debug"}}"#,
    ] {
        rejects_raw_member(&fixture, "/kotlinFacts/events/0/outcome", replacement)?;
    }
    for replacement in [
        r#"{"kind":"string","kind":"string","value":"debug"}"#,
        r#"{"kind":"string","value":"debug","v\u0061lue":"debug"}"#,
    ] {
        rejects_raw_member(&fixture, "/kotlinFacts/events/0/outcome/value", replacement)?;
    }
    rejects_raw_member(
        &fixture,
        "/kotlinFacts/context/requests/0/method",
        r#"{"kind":"selected","value":"source-string","value":"source-property"}"#,
    )?;
    let fixture = missing_method_fixture()?;
    rejects_raw_member(
        &fixture,
        "/kotlinFacts/context/requests/0/method/value",
        r#"{"name":"uncapturedGetter","n\u0061me":"uncapturedGetter","descriptor":"()Ljava/lang/String;","isStatic":false}"#,
    )?;
    let fixture = resolver_fixture()?;
    rejects_raw_member(
        &fixture,
        "/kotlinFacts/context/requests/0/arguments/0",
        r#"{"kind":"object","kind":"null","value":"project-object"}"#,
    )?;
    rejects_raw_member(
        &fixture,
        "/kotlinFacts/context/requests/0/arguments/0",
        r#"{"kind":"object","value":"project-object","v\u0061lue":"project-object"}"#,
    )?;
    let mut fixture = synthetic_fixture()?;
    outcome(&mut fixture, 0, unavailable("invocation", "invoke"));
    rejects_raw_member(
        &fixture,
        "/kotlinFacts/events/0/outcome/value/exceptions/0",
        r#"{"class":"java.lang.reflect.InvocationTargetException","class":"synthetic.DirectError","message":null}"#,
    )?;
    rejects_raw_member(
        &fixture,
        "/kotlinFacts/events/0/outcome/value/exceptions/0",
        r#"{"class":"java.lang.reflect.InvocationTargetException","message":null,"mess\u0061ge":"lost null"}"#,
    )?;
    let failure = fixture
        .wire
        .pointer("/kotlinFacts/events/0/outcome/value")
        .context("Failure")?;
    let failure = serde_json::to_string(failure)?;
    let duplicated = failure.replacen("\"stage\":", "\"stage\":\"classLoad\",\"stage\":", 1);
    rejects_raw_member(&fixture, "/kotlinFacts/events/0/outcome/value", &duplicated)?;
    Ok(())
}
#[test]
fn schema_type_and_version() -> Result<()> {
    for value in [Value::Null, json!("1"), json!(-1), json!(1.5)] {
        let mut fixture = synthetic_fixture()?;
        packet(&mut fixture)["schema"] = value;
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["schema"] = 2.into();
    rejects(&fixture, FactsUnavailableReason::UnsupportedSchema)
}
#[test]
fn single_record_and_exact_prefix() -> Result<()> {
    let fixture = synthetic_fixture()?;
    let wire = model_output(&fixture.wire)?;
    for output in ["ordinary output".to_owned(), format!("embedded {wire}")] {
        assert_eq!(
            parse_kotlin_facts(
                &output,
                &fixture.model,
                &fixture.imports,
                &fixture.expected,
                CaptureLimits::default()
            )
            .expect_err("No exact record")
            .reason,
            FactsUnavailableReason::MissingMetadata
        );
    }
    assert_eq!(
        parse_kotlin_facts(
            &format!("{wire}\n{wire}"),
            &fixture.model,
            &fixture.imports,
            &fixture.expected,
            CaptureLimits::default()
        )
        .expect_err("Ambiguous")
        .reason,
        FactsUnavailableReason::Malformed
    );
    parse_kotlin_facts(
        &format!("ordinary\r\n{wire}\r\n"),
        &fixture.model,
        &fixture.imports,
        &fixture.expected,
        CaptureLimits::default(),
    )?;
    Ok(())
}
#[test]
fn record_bytes_limit() -> Result<()> {
    let fixture = synthetic_fixture()?;
    let mut wire = model_output(&fixture.wire)?;
    let bytes = wire
        .strip_prefix("KODA_ANDROID_PROJECT_MODEL=")
        .context("Marker")?
        .len();
    let limit = 16 * 1024 * 1024;
    wire.extend(std::iter::repeat_n(' ', limit - bytes));
    parse_kotlin_facts(
        &wire,
        &fixture.model,
        &fixture.imports,
        &fixture.expected,
        CaptureLimits::default(),
    )?;
    wire.push(' ');
    assert_eq!(
        parse_kotlin_facts(
            &wire,
            &fixture.model,
            &fixture.imports,
            &fixture.expected,
            CaptureLimits::default()
        )
        .expect_err("Over record limit")
        .reason,
        FactsUnavailableReason::UnsupportedShape
    );
    Ok(())
}
#[test]
fn collection_and_exception_bounds() -> Result<()> {
    fn entries(value: &Value) -> usize {
        1 + match value {
            Value::Object(values) => values.values().map(entries).sum(),
            Value::Array(values) => values.iter().map(entries).sum(),
            _ => 0,
        }
    }
    let mut fixture = synthetic_fixture()?;
    let count = entries(&fixture.wire["kotlinFacts"]);
    decode_with(
        &fixture,
        CaptureLimits {
            entries: count,
            ..CaptureLimits::default()
        },
    )?;
    assert!(
        decode_with(
            &fixture,
            CaptureLimits {
                entries: count - 1,
                ..CaptureLimits::default()
            }
        )
        .is_err()
    );
    outcome(&mut fixture, 0, unavailable("invocation", "invoke"));
    decode_with(
        &fixture,
        CaptureLimits {
            exception_causes: 2,
            ..CaptureLimits::default()
        },
    )?;
    assert!(
        decode_with(
            &fixture,
            CaptureLimits {
                exception_causes: 1,
                ..CaptureLimits::default()
            }
        )
        .is_err()
    );
    Ok(())
}
#[test]
fn token_order_duplicates_and_quotes() -> Result<()> {
    let fixture = resolver_fixture()?;
    let expected = vec![
        "-P",
        "plugin:a=b c",
        "-P",
        "plugin:quote=\"x\"",
        "",
        "𐀀",
        "-Xdummy",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(
        decode(&fixture)?.raw_events()[1].outcome,
        GetterOutcome::Available(Some(CaptureValue::Strings(expected)))
    );
    Ok(())
}
#[test]
fn unicode_paths_no_case_or_separator_inference() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    only_first(&mut fixture)?;
    let request = &mut packet(&mut fixture)["context"]["requests"][0];
    request["method"]["value"] = "inherited".into();
    request["purpose"] = "raw".into();
    request["returnShape"]["kind"] = "file".into();
    let path = "/tmp/𐀀 /\"quoted\"/ê-file";
    outcome(&mut fixture, 0, available("file", json!(path)));
    change_plan(&mut fixture)?;
    assert_eq!(
        decode(&fixture)?.raw_events()[0].outcome,
        GetterOutcome::Available(Some(CaptureValue::File(path.into())))
    );
    for path in [
        "/tmp/𐀀 /./ê-file",
        "/tmp/𐀀 //ê-file/",
        "../raw/./relative-file",
        r"C:\work\.\项目\\app\generated.kt",
        r"\\?\C:\work\项目\generated.kt",
    ] {
        outcome(&mut fixture, 0, available("file", json!(path)));
        let snapshot = decode(&fixture)?;
        let GetterOutcome::Available(Some(CaptureValue::File(observed))) =
            &snapshot.raw_events()[0].outcome
        else {
            anyhow::bail!("Observed File spelling was not retained");
        };
        assert_eq!(observed.as_os_str(), std::ffi::OsStr::new(path));
    }
    let text = "𐀀".repeat(128);
    let mut fixture = synthetic_fixture()?;
    outcome(&mut fixture, 0, available("string", json!(text)));
    decode_with(
        &fixture,
        CaptureLimits {
            string_bytes: 512,
            ..CaptureLimits::default()
        },
    )?;
    assert!(
        decode_with(
            &fixture,
            CaptureLimits {
                string_bytes: 511,
                ..CaptureLimits::default()
            }
        )
        .is_err()
    );
    Ok(())
}
#[test]
fn basic_root_module_binding() -> Result<()> {
    for pointer in [
        "/kotlinFacts/root",
        "/kotlinFacts/modules/0/module",
        "/kotlinFacts/modules/0/directory",
        "/kotlinFacts/modules/0/kind",
        "/kotlinFacts/modules/0/variants/0",
    ] {
        let mut fixture = synthetic_fixture()?;
        *fixture.wire.pointer_mut(pointer).context("Identity")? =
            json!(if pointer.ends_with("kind") {
                "application"
            } else {
                "rebound"
            });
        rejects(&fixture, FactsUnavailableReason::Stale)?;
    }
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["modules"]
        .as_array_mut()
        .context("Modules")?
        .reverse();
    rejects(&fixture, FactsUnavailableReason::Stale)?;
    let fixture = synthetic_fixture()?;
    for pointer in provenance_path_pointers(&fixture)? {
        let mut changed = synthetic_fixture()?;
        let original = changed
            .wire
            .pointer(&pointer)
            .and_then(Value::as_str)
            .context("Literal provenance path")?
            .to_owned();
        let expected = changed.expected.clone();
        for alias in path_aliases(&original)? {
            assert_ne!(alias, original);
            *changed.wire.pointer_mut(&pointer).context("Bound path")? = alias.into();
            rejects(&changed, FactsUnavailableReason::Stale)?;
            if pointer.contains("/imports/") {
                change_plan(&mut changed)?;
                rejects(&changed, FactsUnavailableReason::Stale)?;
            }
            *changed
                .wire
                .pointer_mut(&pointer)
                .context("Original path")? = original.clone().into();
            changed.expected = expected.clone();
        }
    }
    Ok(())
}
#[test]
fn revision_and_selection_binding() -> Result<()> {
    for name in ["modelRevision", "selectionRevision"] {
        let mut fixture = synthetic_fixture()?;
        packet(&mut fixture)["context"]["binding"][name] = 99.into();
        rejects(&fixture, FactsUnavailableReason::Stale)?;
    }
    for name in [
        "captureId",
        "sourceEpoch",
        "fixtureBeforeSha256",
        "fixtureAfterSha256",
    ] {
        let mut fixture = synthetic_fixture()?;
        packet(&mut fixture)["context"]["binding"][name] = "rebound".into();
        rejects(&fixture, FactsUnavailableReason::Stale)?;
    }
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["binding"]["selectedVariants"]
        .as_array_mut()
        .context("Selection")?
        .reverse();
    rejects(&fixture, FactsUnavailableReason::Stale)
}
#[test]
fn selection_membership() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["requests"][0]["variant"]["variant"] = "release".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn project_build_owner_binding() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["objects"][0]["project"] = ":included:unobserved".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::MissingMetadata)?;
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["requests"][0]["owner"] = "other-project".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn unique_ids_and_legal_repetition() -> Result<()> {
    for (section, child) in [("objects", "id"), ("catalogues", "id"), ("requests", "id")] {
        let mut fixture = synthetic_fixture()?;
        let id = packet(&mut fixture)["context"][section][0][child].clone();
        packet(&mut fixture)["context"][section][1][child] = id;
        change_plan(&mut fixture)?;
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["events"][1]["id"] = "event-first".into();
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["requests"][1]["owner"] = "first-task".into();
    change_plan(&mut fixture)?;
    assert_eq!(decode(&fixture)?.raw_events().len(), 2);
    Ok(())
}
#[test]
fn task_path_project_binding() -> Result<()> {
    for (name, value) in [
        ("path", ":nested:library:compileFirst"),
        ("name", "compile:First"),
    ] {
        let mut fixture = synthetic_fixture()?;
        packet(&mut fixture)["context"]["objects"][2]["task"][name] = value.into();
        change_plan(&mut fixture)?;
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    Ok(())
}
#[test]
fn classloader_and_artifact_binding() -> Result<()> {
    let fixture = synthetic_fixture()?;
    for ordinal in 0..fixture.expected.runtime.artifacts.len() {
        let mut changed = synthetic_fixture()?;
        let pointer = format!("/kotlinFacts/context/runtime/artifacts/{ordinal}/path");
        let original = changed
            .wire
            .pointer(&pointer)
            .and_then(Value::as_str)
            .context("Runtime artifact path")?
            .to_owned();
        let expected = changed.expected.clone();
        for alias in path_aliases(&original)? {
            *changed
                .wire
                .pointer_mut(&pointer)
                .context("Artifact path")? = alias.into();
            change_plan(&mut changed)?;
            rejects(&changed, FactsUnavailableReason::Malformed)?;
            *changed
                .wire
                .pointer_mut(&pointer)
                .context("Original artifact path")? = original.clone().into();
            changed.expected = expected.clone();
        }
    }
    for pointer in [
        "/kotlinFacts/context/runtime/classes/6/loader",
        "/kotlinFacts/context/runtime/artifacts/1/sha256",
    ] {
        let mut fixture = synthetic_fixture()?;
        *fixture
            .wire
            .pointer_mut(pointer)
            .context("Runtime provenance")? = "rebound".into();
        rejects(&fixture, FactsUnavailableReason::Stale)?;
    }
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["runtime"]["classes"][6]["loader"] = "gradle".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = resolver_fixture()?;
    let mut rebound = packet(&mut fixture)["context"]["runtime"]["classes"][4].clone();
    rebound["id"] = "rebound-project".into();
    rebound["loader"] = "kgp".into();
    rebound["origin"]["value"] = "kgp-jar".into();
    packet(&mut fixture)["context"]["runtime"]["classes"]
        .as_array_mut()
        .context("Classes")?
        .push(rebound);
    packet(&mut fixture)["context"]["objects"][0]["classId"] = "rebound-project".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn method_catalogue_order_and_descriptor() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["catalogues"][0]["methods"]
        .as_array_mut()
        .context("Method catalogue")?
        .reverse();
    rejects(&fixture, FactsUnavailableReason::Stale)?;
    change_plan(&mut fixture)?;
    let snapshot = decode(&fixture)?;
    assert_eq!(
        snapshot.raw_context().catalogues[0].methods[0].id,
        "inherited"
    );
    assert_eq!(
        snapshot.raw_context().catalogues[0].methods[2].id,
        "source-argument"
    );
    packet(&mut fixture)["context"]["catalogues"][0]["methods"][0]["descriptor"] =
        "()Ljava/lang/String;".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn missing_method_vs_failed_getter() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["requests"][0]["method"] = json!({"kind":"missing","value":{"name":"getSourceSetNameMissing","descriptor":"()Ljava/lang/String;","isStatic":false}});
    outcome(
        &mut fixture,
        0,
        json!({"status":"unavailable","value":{"kind":"missingMethod","stage":"methodDiscovery","capability":"requested getter","detail":"catalogue has no method","actualClass":null,"exceptions":[]}}),
    );
    change_plan(&mut fixture)?;
    assert!(matches!(
        decode(&fixture)?.raw_events()[0].outcome,
        GetterOutcome::Unavailable(_)
    ));
    outcome(&mut fixture, 0, available("string", json!("main")));
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = synthetic_fixture()?;
    outcome(&mut fixture, 0, unavailable("access", "invoke"));
    decode(&fixture)?;
    Ok(())
}
#[test]
fn invocation_target_wrapper_and_cause() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    outcome(&mut fixture, 0, unavailable("invocation", "invoke"));
    let snapshot = decode(&fixture)?;
    let GetterOutcome::Unavailable(failure) = &snapshot.raw_events()[0].outcome else {
        anyhow::bail!("Expected failure");
    };
    assert_eq!(
        failure.exceptions[0].class,
        "java.lang.reflect.InvocationTargetException"
    );
    assert_eq!(failure.exceptions[0].message, None);
    assert_eq!(
        failure.exceptions[1].class,
        "synthetic.UninitializedProperty"
    );
    packet(&mut fixture)["events"][0]["outcome"]["value"]["exceptions"][0]["class"] =
        "synthetic.DirectException".into();
    let snapshot = decode(&fixture)?;
    let GetterOutcome::Unavailable(failure) = &snapshot.raw_events()[0].outcome else {
        anyhow::bail!("Expected failure");
    };
    assert_eq!(failure.exceptions[0].class, "synthetic.DirectException");
    packet(&mut fixture)["events"][0]["outcome"]["value"]["exceptions"][0]
        .as_object_mut()
        .context("Cause")?
        .remove("message");
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn property_event_provenance() -> Result<()> {
    let mut fixture = property_fixture()?;
    for value in [
        json!({"status":"available","value":null}),
        available("string", json!("")),
        unavailable("invocation", "propertyGet"),
    ] {
        outcome(&mut fixture, 1, value);
        decode(&fixture)?;
    }
    packet(&mut fixture)["context"]["requests"][1]["owner"] = "resolver-object".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)?;

    let mut fixture = property_fixture()?;
    let mut other_property = packet(&mut fixture)["context"]["objects"][4].clone();
    assert_eq!(other_property["kind"], "property");
    other_property["id"] = "second-property".into();
    packet(&mut fixture)["context"]["objects"]
        .as_array_mut()
        .context("Objects")?
        .push(other_property);
    packet(&mut fixture)["context"]["requests"][1]["owner"] = "second-property".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    for value in [
        json!({"status":"available","value":null}),
        unavailable("invocation", "invoke"),
    ] {
        let mut fixture = property_fixture()?;
        outcome(&mut fixture, 0, value);
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    let mut fixture = property_fixture()?;
    packet(&mut fixture)["context"]["requests"][0]["method"] = json!({"kind":"missing","value":{
        "name":"getSourceSetNameAbsent","descriptor":"()Lorg/gradle/api/provider/Property;","isStatic":false
    }});
    outcome(
        &mut fixture,
        0,
        unavailable("missingMethod", "methodDiscovery"),
    );
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = property_fixture()?;
    packet(&mut fixture)["events"]
        .as_array_mut()
        .context("Events")?
        .remove(0);
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = property_fixture()?;
    packet(&mut fixture)["context"]["requests"][1]["after"] = Value::Null;
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn unsupported_return_shape() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["runtime"]["classes"]
        .as_array_mut()
        .context("Classes")?
        .push(json!({
            "id":"java.lang.Integer","name":"java.lang.Integer","loader":"bootstrap",
            "origin":{"kind":"jdk","value":"java.base"},"superclass":"object","interfaces":[]
        }));
    packet(&mut fixture)["context"]["requests"][0]["method"]["value"] = "source-number".into();
    packet(&mut fixture)["context"]["requests"][0]["returnShape"] =
        json!({"kind":"unsupported","nullable":false,"objectKind":null,"order":null});
    outcome(
        &mut fixture,
        0,
        json!({"status":"unavailable","value":{"kind":"unsupportedReturnShape","stage":"returnDecode","capability":"numeric source getter","detail":"not String or Property","actualClass":"java.lang.Integer","exceptions":[]}}),
    );
    change_plan(&mut fixture)?;
    decode(&fixture)?;
    for incompatible in ["string", "task"] {
        packet(&mut fixture)["events"][0]["outcome"]["value"]["actualClass"] = incompatible.into();
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    packet(&mut fixture)["events"][0]["outcome"]["value"]["actualClass"] =
        "uncaptured-return-class".into();
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    packet(&mut fixture)["events"][0]["outcome"]["value"]["actualClass"] = Value::Null;
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = synthetic_fixture()?;
    outcome(&mut fixture, 0, available("string", json!(123)));
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    for selected in ["source-string", "inherited"] {
        let mut fixture = synthetic_fixture()?;
        only_first(&mut fixture)?;
        let request = &mut packet(&mut fixture)["context"]["requests"][0];
        request["method"]["value"] = selected.into();
        request["purpose"] = "raw".into();
        request["returnShape"] =
            json!({"kind":"strings","nullable":true,"objectKind":null,"order":"list"});
        outcome(&mut fixture, 0, available("strings", json!([])));
        change_plan(&mut fixture)?;
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    for container in [
        Value::Null,
        json!("first-task"),
        json!("other-project"),
        json!("missing-container"),
    ] {
        let mut fixture = list_fixture()?;
        packet(&mut fixture)["events"][0]["container"] = container;
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    for (descriptor, boxed) in [
        ("B", "java.lang.Byte"),
        ("C", "java.lang.Character"),
        ("S", "java.lang.Short"),
        ("I", "java.lang.Integer"),
        ("J", "java.lang.Long"),
        ("F", "java.lang.Float"),
        ("D", "java.lang.Double"),
    ] {
        let mut fixture = synthetic_fixture()?;
        only_first(&mut fixture)?;
        packet(&mut fixture)["context"]["runtime"]["classes"].as_array_mut().context("Classes")?.push(json!({
            "id":boxed,"name":boxed,"loader":"bootstrap","origin":{"kind":"jdk","value":"java.base"},"superclass":"object","interfaces":[]
        }));
        packet(&mut fixture)["context"]["catalogues"][0]["methods"].as_array_mut().context("Methods")?.push(json!({
            "id":"primitive","name":"syntheticPrimitive","descriptor":format!("(){descriptor}"),"declaringClass":"task","isStatic":false,"parameterClasses":[],"returnClass":null
        }));
        let request = &mut packet(&mut fixture)["context"]["requests"][0];
        request["method"]["value"] = "primitive".into();
        request["purpose"] = "raw".into();
        request["returnShape"] =
            json!({"kind":"unsupported","nullable":false,"objectKind":null,"order":null});
        outcome(
            &mut fixture,
            0,
            json!({"status":"unavailable","value":{"kind":"unsupportedReturnShape","stage":"returnDecode","capability":"synthetic primitive","detail":"raw unsupported boxed value","actualClass":boxed,"exceptions":[]}}),
        );
        change_plan(&mut fixture)?;
        decode(&fixture)?;
        packet(&mut fixture)["events"][0]["outcome"]["value"]["actualClass"] = "string".into();
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
        packet(&mut fixture)["events"][0]["outcome"]["value"]["actualClass"] = boxed.into();
        packet(&mut fixture)["context"]["requests"][0]["returnShape"]["nullable"] = true.into();
        change_plan(&mut fixture)?;
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    let mut reference = synthetic_fixture()?;
    only_first(&mut reference)?;
    packet(&mut reference)["context"]["requests"][0]["returnShape"] =
        json!({"kind":"unsupported","nullable":true,"objectKind":null,"order":null});
    change_plan(&mut reference)?;
    outcome(
        &mut reference,
        0,
        json!({"status":"available","value":null}),
    );
    assert_eq!(
        decode(&reference)?.raw_events()[0].outcome,
        GetterOutcome::Available(None)
    );
    outcome(
        &mut reference,
        0,
        json!({"status":"unavailable","value":{"kind":"unsupportedReturnShape","stage":"returnDecode","capability":"synthetic reference","detail":"raw unsupported value","actualClass":"string","exceptions":[]}}),
    );
    decode(&reference)?;
    packet(&mut reference)["events"][0]["outcome"]["value"]["actualClass"] = "task".into();
    rejects(&reference, FactsUnavailableReason::Malformed)?;
    packet(&mut reference)["events"][0]["outcome"]["value"]["actualClass"] = "string".into();
    packet(&mut reference)["events"][0]["outcome"]["value"]["stage"] = "invoke".into();
    rejects(&reference, FactsUnavailableReason::Malformed)?;

    let mut void = synthetic_fixture()?;
    only_first(&mut void)?;
    packet(&mut void)["context"]["catalogues"][0]["methods"].as_array_mut().context("Methods")?.push(json!({
        "id":"void","name":"getSourceSetNameVoid","descriptor":"()V","declaringClass":"task","isStatic":false,"parameterClasses":[],"returnClass":null
    }));
    packet(&mut void)["context"]["requests"][0]["method"]["value"] = "void".into();
    packet(&mut void)["context"]["requests"][0]["returnShape"] =
        json!({"kind":"unsupported","nullable":true,"objectKind":null,"order":null});
    change_plan(&mut void)?;
    outcome(&mut void, 0, json!({"status":"available","value":null}));
    assert_eq!(
        decode(&void)?.raw_events()[0].outcome,
        GetterOutcome::Available(None)
    );
    outcome(
        &mut void,
        0,
        json!({"status":"unavailable","value":{"kind":"unsupportedReturnShape","stage":"returnDecode","capability":"synthetic void","detail":"fabricated object return","actualClass":"string","exceptions":[]}}),
    );
    rejects(&void, FactsUnavailableReason::Malformed)?;
    outcome(&mut void, 0, json!({"status":"available","value":null}));
    packet(&mut void)["context"]["requests"][0]["returnShape"]["nullable"] = false.into();
    change_plan(&mut void)?;
    rejects(&void, FactsUnavailableReason::Malformed)?;

    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["events"][0]["container"] = "list-container".into();
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn request_parameter_distinctions() -> Result<()> {
    for value in [
        json!({"kind":"absent","value":null}),
        json!({"kind":"explicit","value":null}),
        json!({"kind":"explicit","value":""}),
        json!({"kind":"explicit","value":"*"}),
        json!({"kind":"explicit","value":"*,*"}),
        json!({"kind":"explicit","value":"*,debug"}),
        json!({"kind":"explicit","value":" debug,,I,İ,debug "}),
    ] {
        let mut fixture = synthetic_fixture()?;
        packet(&mut fixture)["context"]["requests"][0]["parameter"] = value.clone();
        change_plan(&mut fixture)?;
        // The formerly accepted contradictory packet must now fail.
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
        packet(&mut fixture)["context"]["requests"][1]["parameter"] = value.clone();
        change_plan(&mut fixture)?;
        assert_eq!(
            serde_json::to_value(&decode(&fixture)?.raw_context().requests[0].parameter)?,
            value
        );
        packet(&mut fixture)["context"]["requests"][1]["modelCall"] = "distinct-call".into();
        packet(&mut fixture)["context"]["requests"][1]["parameter"] =
            json!({"kind":"explicit","value":"independently retained"});
        packet(&mut fixture)["context"]["requests"][1]["consumer"] = "kapt".into();
        change_plan(&mut fixture)?;
        decode(&fixture)?;
    }
    for (name, value) in [
        ("project", json!(":nested:library")),
        ("consumer", json!("kapt")),
        ("parameter", json!({"kind":"absent","value":null})),
    ] {
        let mut fixture = synthetic_fixture()?;
        packet(&mut fixture)["context"]["requests"][1][name] = value;
        if name == "project" {
            packet(&mut fixture)["context"]["requests"][1]["variant"] =
                json!({"module":":nested:library","variant":"jvm"});
            packet(&mut fixture)["context"]["objects"][3]["project"] = ":nested:library".into();
            packet(&mut fixture)["context"]["objects"][3]["task"]["path"] =
                ":nested:library:compileSecond".into();
        }
        change_plan(&mut fixture)?;
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    let mut fixture = property_fixture()?;
    packet(&mut fixture)["context"]["requests"][1]["consumer"] = "kapt".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = synthetic_fixture()?;
    let request = &mut packet(&mut fixture)["context"]["requests"][0];
    request["variant"] = Value::Null;
    request["owner"] = "project-object".into();
    request["catalogue"] = "projects".into();
    request["method"]["value"] = "task-list".into();
    request["purpose"] = "raw".into();
    request["returnShape"] =
        json!({"kind":"strings","nullable":true,"objectKind":null,"order":"list"});
    outcome(&mut fixture, 0, available("strings", json!([])));
    change_plan(&mut fixture)?;
    let snapshot = decode(&fixture)?;
    assert!(snapshot.raw_context().requests[0].variant.is_none());
    assert!(snapshot.raw_context().requests[1].variant.is_some());
    Ok(())
}
#[test]
fn request_locale_provenance() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    for ordinal in 0..2 {
        packet(&mut fixture)["context"]["requests"][ordinal]["parameter"]["value"] =
            "I,İ, debug".into();
    }
    change_plan(&mut fixture)?;
    let snapshot = decode(&fixture)?;
    assert_eq!(snapshot.raw_context().runtime.locale.identifier, "tr-TR");
    assert_eq!(
        serde_json::to_value(&snapshot.raw_context().requests[0].parameter)?,
        json!({"kind":"explicit","value":"I,İ, debug"})
    );
    packet(&mut fixture)["context"]["runtime"]["locale"]["identifier"] = "en-US".into();
    rejects(&fixture, FactsUnavailableReason::Stale)
}
#[test]
fn five_candidate_requests_not_models() -> Result<()> {
    let fixture = synthetic_fixture()?;
    let snapshot = decode(&fixture)?;
    assert_eq!(
        snapshot
            .raw_context()
            .objects
            .iter()
            .filter(|value| value.kind == android_tools::kotlin_import_facts::ObjectKind::Task)
            .count(),
        2
    );
    let encoded = serde_json::to_string(snapshot.raw_context())?;
    assert!(encoded.contains("debugScreenshotTest"));
    assert!(!encoded.contains("compilerArgumentsBySourceSet"));
    assert_eq!(snapshot.raw_events().len(), 2);
    Ok(())
}
#[test]
fn discovery_without_invocation() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["mode"] = "discovery".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    packet(&mut fixture)["context"]["requests"] = json!([]);
    packet(&mut fixture)["events"] = json!([]);
    change_plan(&mut fixture)?;
    assert!(decode(&fixture)?.raw_events().is_empty());
    Ok(())
}
#[test]
fn invocation_response_completeness_order() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["events"]
        .as_array_mut()
        .context("Events")?
        .reverse();
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    packet(&mut fixture)["events"]
        .as_array_mut()
        .context("Events")?
        .truncate(1);
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["events"][1]["request"] = "unsolicited".into();
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn catalogue_changed_before_invoke() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    outcome(
        &mut fixture,
        0,
        json!({"status":"unavailable","value":{"kind":"catalogueChanged","stage":"methodDiscovery","capability":"catalogue contract","detail":"new runtime order","actualClass":null,"exceptions":[]}}),
    );
    assert!(matches!(
        decode(&fixture)?.raw_events()[0].outcome,
        GetterOutcome::Unavailable(_)
    ));
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["requests"][0]["method"]["value"] = "source-argument".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn ordered_containers_and_duplicate_source_names() -> Result<()> {
    let fixture = synthetic_fixture()?;
    let snapshot = decode(&fixture)?;
    assert_eq!(
        snapshot.raw_events()[0].outcome,
        snapshot.raw_events()[1].outcome
    );
    assert_ne!(
        snapshot.raw_context().requests[0].owner,
        snapshot.raw_context().requests[1].owner
    );
    let mut fixture = list_fixture()?;
    packet(&mut fixture)["context"]["requests"][0]["returnShape"] =
        json!({"kind":"objects","nullable":true,"objectKind":"task","order":"list"});
    outcome(
        &mut fixture,
        0,
        available(
            "objects",
            json!(["second-task", "first-task", "second-task"]),
        ),
    );
    change_plan(&mut fixture)?;
    assert_eq!(
        decode(&fixture)?.raw_events()[0].outcome,
        GetterOutcome::Available(Some(CaptureValue::Objects(vec![
            "second-task".into(),
            "first-task".into(),
            "second-task".into()
        ])))
    );
    for order in ["namedDomainObjectAsMapValues", "projectTaskMapValues"] {
        packet(&mut fixture)["context"]["requests"][0]["returnShape"]["order"] = order.into();
        change_plan(&mut fixture)?;
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    packet(&mut fixture)["context"]["requests"][0]["returnShape"]["order"] = "iterable".into();
    change_plan(&mut fixture)?;
    decode(&fixture)?;
    for task_map in [false, true] {
        let mut fixture = ordered_map_fixture(task_map)?;
        let ordinal = if task_map { 1 } else { 2 };
        let snapshot = decode(&fixture)?;
        assert_eq!(
            snapshot.raw_events()[ordinal].outcome,
            GetterOutcome::Available(Some(CaptureValue::Objects(vec![
                "second-task".into(),
                "first-task".into(),
                "second-task".into()
            ])))
        );
        assert_eq!(
            snapshot.raw_events()[ordinal].container.as_deref(),
            Some("list-container")
        );
        let encoded = serde_json::to_value(&snapshot.raw_context().requests[ordinal].return_shape)?;
        assert_eq!(
            encoded["order"],
            if task_map {
                "projectTaskMapValues"
            } else {
                "namedDomainObjectAsMapValues"
            }
        );
        let parent = ordinal - 1;
        outcome(
            &mut fixture,
            parent,
            available("object", json!("second-map-container")),
        );
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
        let mut fixture = ordered_map_fixture(task_map)?;
        packet(&mut fixture)["events"][ordinal]["container"] = "map-container".into();
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
        let mut fixture = ordered_map_fixture(task_map)?;
        packet(&mut fixture)["context"]["requests"][ordinal]["returnShape"]["order"] =
            if task_map {
                "namedDomainObjectAsMapValues"
            } else {
                "projectTaskMapValues"
            }
            .into();
        change_plan(&mut fixture)?;
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    let mut fixture = ordered_map_fixture(false)?;
    outcome(
        &mut fixture,
        0,
        available("object", json!("list-container")),
    );
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = ordered_map_fixture(true)?;
    packet(&mut fixture)["context"]["requests"][0]["arguments"][0]["value"] = true.into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = ordered_map_fixture(true)?;
    packet(&mut fixture)["context"]["requests"][1]["arguments"][0]["value"] =
        "other-project".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn target_vs_compilation_presence() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    for (identifier, kind) in [
        ("extension", "extension"),
        ("target", "target"),
        ("compilation", "compilation"),
    ] {
        packet(&mut fixture)["context"]["runtime"]["classes"].as_array_mut().context("Classes")?.push(json!({
            "id":identifier,"name":format!("synthetic.{identifier}"),"loader":"kgp","origin":{"kind":"artifact","value":"kgp-jar"},"superclass":"object","interfaces":[]
        }));
        packet(&mut fixture)["context"]["objects"]
            .as_array_mut()
            .context("Objects")?
            .push(json!({
                "id":identifier,"kind":kind,"project":":android","classId":identifier,"task":null
            }));
    }
    for (identifier, name, returned, return_class) in [
        ("extension", "getTarget", "Lsynthetic/target;", "target"),
        ("target", "getCompilations", "Ljava/util/List;", "list"),
    ] {
        packet(&mut fixture)["context"]["catalogues"].as_array_mut().context("Catalogues")?.push(json!({
            "id":identifier,"classId":identifier,"methods":[{"id":identifier,"name":name,"descriptor":format!("(){returned}"),"declaringClass":identifier,"isStatic":false,"parameterClasses":[],"returnClass":return_class}]
        }));
    }
    let requests = &mut packet(&mut fixture)["context"]["requests"];
    for (ordinal, identifier) in [(0, "extension"), (1, "target")] {
        requests[ordinal]["owner"] = identifier.into();
        requests[ordinal]["catalogue"] = identifier.into();
        requests[ordinal]["method"]["value"] = identifier.into();
        requests[ordinal]["purpose"] = "raw".into();
    }
    requests[0]["returnShape"] =
        json!({"kind":"object","nullable":true,"objectKind":"target","order":null});
    requests[1]["returnShape"] =
        json!({"kind":"objects","nullable":true,"objectKind":"compilation","order":"list"});
    outcome(&mut fixture, 0, available("object", json!("target")));
    outcome(
        &mut fixture,
        1,
        available("objects", json!(["compilation", "compilation"])),
    );
    change_plan(&mut fixture)?;
    assert_eq!(
        decode(&fixture)?.raw_events()[1].outcome,
        GetterOutcome::Available(Some(CaptureValue::Objects(vec![
            "compilation".into(),
            "compilation".into()
        ])))
    );
    for value in [
        json!({"status":"available","value":null}),
        available("objects", json!([])),
        unavailable("invocation", "invoke"),
    ] {
        outcome(&mut fixture, 1, value.clone());
        assert_eq!(
            serde_json::to_value(&decode(&fixture)?.raw_events()[1].outcome)?,
            value
        );
    }
    only_first(&mut fixture)?;
    outcome(&mut fixture, 0, json!({"status":"available","value":null}));
    change_plan(&mut fixture)?;
    assert_eq!(
        decode(&fixture)?.raw_events()[0].outcome,
        GetterOutcome::Available(None)
    );
    assert!(
        !serde_json::to_string(decode(&fixture)?.raw_context())?
            .contains("compilerArgumentsBySourceSet")
    );
    Ok(())
}
#[test]
fn compilation_task_lookup() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    only_first(&mut fixture)?;
    let request = &mut packet(&mut fixture)["context"]["requests"][0];
    request["owner"] = "project-object".into();
    request["catalogue"] = "projects".into();
    request["method"]["value"] = "find-task".into();
    request["purpose"] = "taskLookup".into();
    request["arguments"] = json!([{"kind":"string","value":"differentCompilationName"}]);
    request["returnShape"] =
        json!({"kind":"object","nullable":true,"objectKind":"task","order":null});
    outcome(&mut fixture, 0, json!({"status":"available","value":null}));
    change_plan(&mut fixture)?;
    assert_eq!(
        decode(&fixture)?.raw_events()[0].outcome,
        GetterOutcome::Available(None)
    );
    outcome(&mut fixture, 0, available("object", json!("missing-task")));
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["runtime"]["classes"]
        .as_array_mut()
        .context("Classes")?
        .push(json!({
            "id":"compilation","name":"synthetic.Compilation","loader":"kgp",
            "origin":{"kind":"artifact","value":"kgp-jar"},"superclass":"object","interfaces":[]
        }));
    packet(&mut fixture)["context"]["objects"].as_array_mut().context("Objects")?.push(json!({
        "id":"compilation","kind":"compilation","project":":android","classId":"compilation","task":null
    }));
    packet(&mut fixture)["context"]["catalogues"].as_array_mut().context("Catalogues")?.push(json!({
        "id":"compilation","classId":"compilation","methods":[
            {"id":"compilation-name","name":"getName","descriptor":"()Ljava/lang/String;","declaringClass":"compilation","isStatic":false,"parameterClasses":[],"returnClass":"string"},
            {"id":"compilation-task-name","name":"getCompileKotlinTaskName","descriptor":"()Ljava/lang/String;","declaringClass":"compilation","isStatic":false,"parameterClasses":[],"returnClass":"string"}
        ]
    }));
    for (ordinal, method) in [(0, "compilation-name"), (1, "compilation-task-name")] {
        let request = &mut packet(&mut fixture)["context"]["requests"][ordinal];
        request["owner"] = "compilation".into();
        request["catalogue"] = "compilation".into();
        request["method"]["value"] = method.into();
        request["purpose"] = "raw".into();
    }
    outcome(
        &mut fixture,
        0,
        available("string", json!("differentCompilationName")),
    );
    outcome(&mut fixture, 1, available("string", json!("compileSecond")));
    let mut lookup = packet(&mut fixture)["context"]["requests"][0].clone();
    lookup["id"] = "task-lookup".into();
    lookup["owner"] = "project-object".into();
    lookup["catalogue"] = "projects".into();
    lookup["method"]["value"] = "find-task".into();
    lookup["purpose"] = "taskLookup".into();
    lookup["arguments"] = json!([{"kind":"string","value":"compileSecond"}]);
    lookup["returnShape"] =
        json!({"kind":"object","nullable":true,"objectKind":"task","order":null});
    lookup["after"] = "source-second".into();
    packet(&mut fixture)["context"]["requests"]
        .as_array_mut()
        .context("Requests")?
        .push(lookup);
    packet(&mut fixture)["events"]
        .as_array_mut()
        .context("Events")?
        .push(json!({
            "id":"lookup-event","request":"task-lookup","container":null,
            "outcome":available("object",json!("second-task"))
        }));
    change_plan(&mut fixture)?;
    let snapshot = decode(&fixture)?;
    assert_eq!(
        snapshot.raw_events()[0].outcome,
        GetterOutcome::Available(Some(CaptureValue::String(
            "differentCompilationName".into()
        )))
    );
    assert_eq!(
        snapshot.raw_events()[1].outcome,
        GetterOutcome::Available(Some(CaptureValue::String("compileSecond".into())))
    );
    assert_eq!(
        serde_json::to_value(&snapshot.raw_context().requests[2].arguments)?,
        json!([{"kind":"string","value":"compileSecond"}])
    );
    assert_eq!(
        snapshot.raw_events()[2].outcome,
        GetterOutcome::Available(Some(CaptureValue::Object("second-task".into())))
    );
    for value in [
        json!({"status":"available","value":null}),
        unavailable("invocation", "taskLookup"),
    ] {
        outcome(&mut fixture, 2, value.clone());
        assert_eq!(
            serde_json::to_value(&decode(&fixture)?.raw_events()[2].outcome)?,
            value
        );
    }
    outcome(&mut fixture, 2, available("object", json!("missing-task")));
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn plugin_find_and_interface_facts() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    only_first(&mut fixture)?;
    for class in [
        json!({"id":"factory","name":"org.jetbrains.kotlin.gradle.plugin.KotlinJvmFactory","loader":"kgp","origin":{"kind":"artifact","value":"kgp-jar"},"superclass":null,"interfaces":[]}),
        json!({"id":"plugin-container","name":"synthetic.PluginContainer","loader":"kgp","origin":{"kind":"artifact","value":"kgp-jar"},"superclass":"object","interfaces":[]}),
        json!({"id":"plugin-class","name":"synthetic.KotlinPlugin","loader":"kgp","origin":{"kind":"artifact","value":"kgp-jar"},"superclass":"object","interfaces":["factory"]}),
    ] {
        packet(&mut fixture)["context"]["runtime"]["classes"]
            .as_array_mut()
            .context("Classes")?
            .push(class);
    }
    for object in [
        json!({"id":"plugins","kind":"container","project":":android","classId":"plugin-container","task":null}),
        json!({"id":"plugin","kind":"plugin","project":":android","classId":"plugin-class","task":null}),
    ] {
        packet(&mut fixture)["context"]["objects"]
            .as_array_mut()
            .context("Objects")?
            .push(object);
    }
    packet(&mut fixture)["context"]["catalogues"].as_array_mut().context("Catalogues")?.push(json!({"id":"plugins","classId":"plugin-container","methods":[{"id":"plugin-find","name":"findPlugin","descriptor":"(Ljava/lang/String;)Ljava/lang/Object;","declaringClass":"plugin-container","isStatic":false,"parameterClasses":["string"],"returnClass":"object"}]}));
    let request = &mut packet(&mut fixture)["context"]["requests"][0];
    request["owner"] = "plugins".into();
    request["catalogue"] = "plugins".into();
    request["method"]["value"] = "plugin-find".into();
    request["purpose"] = "raw".into();
    request["arguments"] = json!([{"kind":"string","value":"kotlin"}]);
    request["returnShape"] =
        json!({"kind":"object","nullable":true,"objectKind":"plugin","order":null});
    change_plan(&mut fixture)?;
    for value in [
        available("object", json!("plugin")),
        json!({"status":"available","value":null}),
        unavailable("invocation", "invoke"),
    ] {
        outcome(&mut fixture, 0, value.clone());
        assert_eq!(
            serde_json::to_value(&decode(&fixture)?.raw_events()[0].outcome)?,
            value
        );
    }
    let snapshot = decode(&fixture)?;
    assert_eq!(
        snapshot
            .raw_context()
            .runtime
            .classes
            .last()
            .context("Plugin class")?
            .interfaces,
        ["factory"]
    );
    assert!(!serde_json::to_string(snapshot.raw_context())?.contains("hasKotlinFacet"));
    Ok(())
}
#[test]
fn resolver_service_null_and_failure() -> Result<()> {
    let mut fixture = resolver_fixture()?;
    for value in [
        json!({"status":"available","value":null}),
        available("strings", json!([])),
        unavailable("invocation", "invoke"),
    ] {
        outcome(&mut fixture, 1, value.clone());
        assert_eq!(
            serde_json::to_value(&decode(&fixture)?.raw_events()[1].outcome)?,
            value
        );
    }
    only_first(&mut fixture)?;
    change_plan(&mut fixture)?;
    outcome(&mut fixture, 0, json!({"status":"available","value":null}));
    assert_eq!(decode(&fixture)?.raw_events().len(), 1);
    assert_eq!(
        decode(&fixture)?.raw_events()[0].outcome,
        GetterOutcome::Available(None)
    );
    Ok(())
}
#[test]
fn version_fields_and_artifact_provenance() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    for (ordinal, identifier) in [
        (0, "synthetic-plugin-version"),
        (1, "synthetic-compiler-version"),
    ] {
        packet(&mut fixture)["context"]["catalogues"][0]["methods"].as_array_mut().context("Methods")?.push(json!({"id":identifier,"name":identifier,"descriptor":"()Ljava/lang/String;","declaringClass":"task","isStatic":false,"parameterClasses":[],"returnClass":"string"}));
        packet(&mut fixture)["context"]["requests"][ordinal]["method"]["value"] = identifier.into();
        packet(&mut fixture)["context"]["requests"][ordinal]["purpose"] = "raw".into();
    }
    change_plan(&mut fixture)?;
    outcome(
        &mut fixture,
        0,
        available("string", json!("plugin-version")),
    );
    outcome(
        &mut fixture,
        1,
        available("string", json!("compiler-version")),
    );
    let snapshot = decode(&fixture)?;
    assert_ne!(
        snapshot.raw_events()[0].outcome,
        snapshot.raw_events()[1].outcome
    );
    assert_eq!(snapshot.raw_context().runtime.gradle_version, "9.6.1");
    assert_eq!(
        snapshot.raw_context().runtime.artifacts[1]
            .version
            .as_deref(),
        Some("synthetic-version")
    );
    Ok(())
}
#[test]
fn kapt_two_source_getter_events() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["requests"][0]["consumer"] = "kapt".into();
    packet(&mut fixture)["context"]["requests"][1]["consumer"] = "kapt".into();
    packet(&mut fixture)["context"]["requests"][1]["owner"] = "first-task".into();
    outcome(&mut fixture, 0, available("string", json!("debug")));
    outcome(
        &mut fixture,
        1,
        available("string", json!("changed-source-name")),
    );
    change_plan(&mut fixture)?;
    assert_ne!(
        decode(&fixture)?.raw_events()[0].outcome,
        decode(&fixture)?.raw_events()[1].outcome
    );
    outcome(&mut fixture, 0, unavailable("invocation", "invoke"));
    assert!(matches!(
        decode(&fixture)?.raw_events()[0].outcome,
        GetterOutcome::Unavailable(_)
    ));
    Ok(())
}
#[test]
fn kapt_with_java_static_directory_provenance() -> Result<()> {
    let mut with_java = synthetic_fixture()?;
    only_first(&mut with_java)?;
    packet(&mut with_java)["context"]["runtime"]["classes"].as_array_mut().context("Classes")?.push(json!({"id":"boolean","name":"java.lang.Boolean","loader":"bootstrap","origin":{"kind":"jdk","value":"java.base"},"superclass":"object","interfaces":[]}));
    packet(&mut with_java)["context"]["catalogues"][0]["methods"].as_array_mut().context("Methods")?.push(json!({"id":"with-java","name":"getWithJavaEnabled","descriptor":"()Ljava/lang/Boolean;","declaringClass":"task","isStatic":false,"parameterClasses":[],"returnClass":"boolean"}));
    let request = &mut packet(&mut with_java)["context"]["requests"][0];
    request["consumer"] = "kapt".into();
    request["method"]["value"] = "with-java".into();
    request["purpose"] = "raw".into();
    request["returnShape"] =
        json!({"kind":"boolean","nullable":true,"objectKind":null,"order":null});
    change_plan(&mut with_java)?;
    for value in [
        available("boolean", json!(false)),
        available("boolean", json!(true)),
        json!({"status":"available","value":null}),
        unavailable("invocation", "invoke"),
    ] {
        outcome(&mut with_java, 0, value.clone());
        assert_eq!(
            serde_json::to_value(&decode(&with_java)?.raw_events()[0].outcome)?,
            value
        );
    }
    let mut fixture = synthetic_fixture()?;
    only_first(&mut fixture)?;
    let method = json!({"id":"kapt-directory","name":"getKaptGeneratedSourcesDir","descriptor":"(Lorg/gradle/api/Project;Ljava/lang/String;)Ljava/io/File;","declaringClass":"task","isStatic":true,"parameterClasses":["project","string"],"returnClass":"file"});
    packet(&mut fixture)["context"]["catalogues"][0]["methods"]
        .as_array_mut()
        .context("Methods")?
        .push(method);
    let request = &mut packet(&mut fixture)["context"]["requests"][0];
    request["consumer"] = "kapt".into();
    request["owner"] = Value::Null;
    request["method"]["value"] = "kapt-directory".into();
    request["purpose"] = "raw".into();
    request["arguments"] =
        json!([{"kind":"object","value":"project-object"},{"kind":"string","value":"debug"}]);
    request["returnShape"] = json!({"kind":"file","nullable":true,"objectKind":null,"order":null});
    outcome(
        &mut fixture,
        0,
        available("file", json!("relative/raw/path")),
    );
    change_plan(&mut fixture)?;
    assert_eq!(
        decode(&fixture)?.raw_events()[0].outcome,
        GetterOutcome::Available(Some(CaptureValue::File("relative/raw/path".into())))
    );
    packet(&mut fixture)["context"]["requests"][0]["arguments"][0]["value"] =
        "other-project".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn dangling_and_cyclic_references() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["runtime"]["loaders"][0]["parent"] = "kgp".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["runtime"]["classes"][0]["superclass"] = "task".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["context"]["requests"][0]["after"] = "source-second".into();
    change_plan(&mut fixture)?;
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    for pointer in [
        "/kotlinFacts/context/runtime/java/artifact",
        "/kotlinFacts/context/runtime/loaders/1/parent",
        "/kotlinFacts/context/runtime/loaders/1/artifacts/0",
        "/kotlinFacts/context/runtime/classes/6/loader",
        "/kotlinFacts/context/runtime/classes/6/origin/value",
        "/kotlinFacts/context/runtime/classes/6/superclass",
        "/kotlinFacts/context/objects/2/classId",
        "/kotlinFacts/context/catalogues/0/classId",
        "/kotlinFacts/context/catalogues/0/methods/0/declaringClass",
        "/kotlinFacts/context/catalogues/0/methods/0/returnClass",
        "/kotlinFacts/context/requests/0/owner",
        "/kotlinFacts/context/requests/0/catalogue",
        "/kotlinFacts/context/requests/0/method/value",
        "/kotlinFacts/context/requests/0/after",
    ] {
        let mut fixture = synthetic_fixture()?;
        *fixture.wire.pointer_mut(pointer).context("Referenced ID")? = "dangling".into();
        change_plan(&mut fixture)?;
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    for pointer in [
        "/kotlinFacts/context/requests/0/arguments/0/value",
        "/kotlinFacts/context/catalogues/2/methods/0/parameterClasses/0",
    ] {
        let mut fixture = resolver_fixture()?;
        *fixture
            .wire
            .pointer_mut(pointer)
            .context("Resolver reference")? = "dangling".into();
        change_plan(&mut fixture)?;
        rejects(&fixture, FactsUnavailableReason::Malformed)?;
    }
    let mut fixture = synthetic_fixture()?;
    packet(&mut fixture)["events"][0]["request"] = "dangling".into();
    rejects(&fixture, FactsUnavailableReason::Malformed)?;
    let mut fixture = property_fixture()?;
    outcome(&mut fixture, 0, available("object", json!("dangling")));
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
#[test]
fn ensure_current_rechecks_publication() -> Result<()> {
    let fixture = synthetic_fixture()?;
    let snapshot = decode(&fixture)?;
    snapshot.ensure_current(&fixture.model, &fixture.imports, &fixture.expected)?;
    for pointer in provenance_path_pointers(&fixture)? {
        let original = fixture
            .wire
            .pointer(&pointer)
            .and_then(Value::as_str)
            .context("Published provenance path")?;
        for alias in path_aliases(original)? {
            if let Some(pointer) = pointer.strip_prefix("/kotlinFacts/context") {
                let mut expected = serde_json::to_value(&fixture.expected)?;
                *expected
                    .pointer_mut(pointer)
                    .context("Expected provenance path")? = alias.into();
                let expected = serde_json::from_value(expected)?;
                assert_eq!(
                    snapshot
                        .ensure_current(&fixture.model, &fixture.imports, &expected)
                        .expect_err("Changed literal context path cannot publish old capture")
                        .reason,
                    FactsUnavailableReason::Stale
                );
            } else {
                let mut model = fixture.model.clone();
                if pointer == "/kotlinFacts/root" {
                    model.root = alias.into();
                } else {
                    let ordinal = pointer
                        .strip_prefix("/kotlinFacts/modules/")
                        .and_then(|suffix| suffix.strip_suffix("/directory"))
                        .context("Basic directory pointer")?
                        .parse::<usize>()?;
                    model
                        .modules
                        .get_mut(ordinal)
                        .context("Current Basic module")?
                        .directory = alias.into();
                }
                assert_eq!(
                    snapshot
                        .ensure_current(&model, &fixture.imports, &fixture.expected)
                        .expect_err("Changed literal Basic path cannot publish old capture")
                        .reason,
                    FactsUnavailableReason::Stale
                );
            }
        }
    }
    let mut expected = fixture.expected.clone();
    expected.binding.source_epoch = "changed".into();
    assert_eq!(
        snapshot
            .ensure_current(&fixture.model, &fixture.imports, &expected)
            .expect_err("Stale epoch")
            .reason,
        FactsUnavailableReason::Stale
    );
    let mut model = fixture.model.clone();
    model.modules.reverse();
    assert_eq!(
        snapshot
            .ensure_current(&model, &fixture.imports, &fixture.expected)
            .expect_err("Stale model")
            .reason,
        FactsUnavailableReason::Stale
    );
    for (pointer, value) in [
        ("/binding/modelRevision", json!(8)),
        ("/binding/selectionRevision", json!(4)),
        ("/binding/captureId", json!("another-capture")),
        ("/binding/sourceEpoch", json!("another-epoch")),
        ("/binding/fixtureBeforeSha256", json!("a".repeat(64))),
        ("/binding/fixtureAfterSha256", json!("b".repeat(64))),
        ("/binding/selectedVariants/0/variant", json!("release")),
        ("/runtime/gradleVersion", json!("different-runtime")),
        ("/runtime/java/build", json!("another-jdk-build")),
        ("/runtime/locale/identifier", json!("en-US")),
        ("/runtime/artifacts/1/sha256", json!("a".repeat(64))),
    ] {
        let mut changed = serde_json::to_value(&fixture.expected)?;
        *changed.pointer_mut(pointer).context("Current binding")? = value;
        let changed: CaptureContext = serde_json::from_value(changed)?;
        assert_eq!(
            snapshot
                .ensure_current(&fixture.model, &fixture.imports, &changed)
                .expect_err("Changed current state cannot publish historical raw capture")
                .reason,
            FactsUnavailableReason::Stale
        );
    }
    let mut reordered = fixture.expected.clone();
    reordered.binding.selected_variants.reverse();
    assert_eq!(
        snapshot
            .ensure_current(&fixture.model, &fixture.imports, &reordered)
            .expect_err("Changed selection order")
            .reason,
        FactsUnavailableReason::Stale
    );
    for root_name in [false, true] {
        let mut replacement = fixture.wire.clone();
        if root_name {
            replacement["importFacts"]["buildIdentity"]["rootName"]["result"]["value"] =
                "replacement-root-name".into();
            let projects = replacement["importFacts"]["projectCatalogue"]["result"]["value"]
                .as_array_mut()
                .context("Projects")?;
            for project in projects {
                project["rootName"]["result"]["value"] = "replacement-root-name".into();
                if project["projectPath"]["result"]["value"] == ":" {
                    project["projectName"]["result"]["value"] = "replacement-root-name".into();
                }
            }
        } else {
            replacement["importFacts"]["projectCatalogue"]["result"]["value"][1]["projectName"]["result"]
                ["value"] = "replacement-holder-name".into();
        }
        let imports = parse_import_facts(
            &model_output(&replacement)?,
            &fixture.model,
            import_binding(),
        )?;
        imports.ensure_current(&fixture.model, &import_binding())?;
        assert_eq!(
            snapshot
                .ensure_current(&fixture.model, &imports, &fixture.expected)
                .expect_err("Different valid import identity cannot publish old capture")
                .reason,
            FactsUnavailableReason::Stale
        );
        let error = parse_kotlin_facts(
            &model_output(&fixture.wire)?,
            &fixture.model,
            &imports,
            &fixture.expected,
            CaptureLimits::default(),
        )
        .expect_err("Different valid import identity cannot accept capture");
        assert_eq!(error.reason, FactsUnavailableReason::Stale);
    }
    // Historical observations remain available after the caller recheck fails.
    assert_eq!(snapshot.raw_events().len(), 2);
    Ok(())
}
#[test]
fn large_valid_indexed_packet() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    let request = packet(&mut fixture)["context"]["requests"][0].clone();
    let event = packet(&mut fixture)["events"][0].clone();
    for ordinal in 2..1024 {
        let mut request = request.clone();
        request["id"] = format!("request-{ordinal}").into();
        let mut event = event.clone();
        event["id"] = format!("event-{ordinal}").into();
        event["request"] = request["id"].clone();
        packet(&mut fixture)["context"]["requests"]
            .as_array_mut()
            .context("Requests")?
            .push(request);
        packet(&mut fixture)["events"]
            .as_array_mut()
            .context("Events")?
            .push(event);
    }
    change_plan(&mut fixture)?;
    assert_eq!(decode(&fixture)?.raw_events().len(), 1024);
    assert!(
        decode_with(
            &fixture,
            CaptureLimits {
                ancestry_steps: 0,
                ..CaptureLimits::default()
            }
        )
        .is_err()
    );
    let mut bulk = synthetic_fixture()?;
    for ordinal in 0..3500 {
        packet(&mut bulk)["context"]["catalogues"][0]["methods"]
            .as_array_mut()
            .context("Methods")?
            .push(json!({
                "id":format!("bulk-method-{ordinal}"),"name":format!("bulkGetter{ordinal}"),
                "descriptor":"()Ljava/lang/Object;","declaringClass":"task","isStatic":false,
                "parameterClasses":[],"returnClass":"object"
            }));
    }
    for ordinal in 0..1000 {
        packet(&mut bulk)["context"]["runtime"]["classes"]
            .as_array_mut()
            .context("Classes")?
            .push(json!({
                "id":format!("bulk-class-{ordinal}"),"name":format!("synthetic.BulkClass{ordinal}"),
                "loader":"kgp","origin":{"kind":"artifact","value":"kgp-jar"},
                "superclass":"object","interfaces":[]
            }));
    }
    let mut request = packet(&mut bulk)["context"]["requests"][0].clone();
    request["method"]["value"] = "bulk-method-3499".into();
    request["purpose"] = "raw".into();
    request["returnShape"] =
        json!({"kind":"object","nullable":true,"objectKind":"task","order":null});
    let mut requests = Vec::new();
    let mut events = Vec::new();
    for ordinal in 0..1200 {
        let mut request = request.clone();
        request["id"] = format!("bulk-request-{ordinal}").into();
        requests.push(request.clone());
        events.push(
            json!({"id":format!("bulk-event-{ordinal}"),"request":request["id"],
            "container":null,"outcome":available("object",json!("first-task"))}),
        );
    }
    packet(&mut bulk)["context"]["requests"] = requests.into();
    packet(&mut bulk)["events"] = events.into();
    change_plan(&mut bulk)?;
    let snapshot = decode(&bulk)?;
    assert_eq!(snapshot.raw_context().runtime.classes.len(), 1011);
    assert_eq!(snapshot.raw_context().catalogues[0].methods.len(), 3505);
    assert_eq!(snapshot.raw_events().len(), 1200);
    assert_eq!(
        snapshot.raw_events()[1199].outcome,
        GetterOutcome::Available(Some(CaptureValue::Object("first-task".into())))
    );
    // Missing signatures must use the exact complete catalogue without losing raw order.
    for ordinal in 0..1200 {
        let request = &mut packet(&mut bulk)["context"]["requests"][ordinal];
        request["method"] = json!({"kind":"missing","value":{
            "name":format!("absentGetter{ordinal}"),"descriptor":"()Ljava/lang/Object;","isStatic":false
        }});
        outcome(
            &mut bulk,
            ordinal,
            unavailable("missingMethod", "methodDiscovery"),
        );
    }
    change_plan(&mut bulk)?;
    assert_eq!(decode(&bulk)?.raw_events().len(), 1200);
    packet(&mut bulk)["context"]["requests"][0]["method"]["value"]["name"] =
        "bulkGetter3499".into();
    change_plan(&mut bulk)?;
    rejects(&bulk, FactsUnavailableReason::Malformed)?;

    let mut dense = synthetic_fixture()?;
    for ordinal in 0..300 {
        let interfaces: Vec<_> = (0..ordinal)
            .map(|parent| format!("dense-{parent}"))
            .collect();
        packet(&mut dense)["context"]["runtime"]["classes"]
            .as_array_mut()
            .context("Classes")?
            .push(json!({
                "id":format!("dense-{ordinal}"),"name":format!("synthetic.Dense{ordinal}"),
                "loader":"kgp","origin":{"kind":"artifact","value":"kgp-jar"},
                "superclass":null,"interfaces":interfaces
            }));
    }
    packet(&mut dense)["context"]["runtime"]["classes"][6]["interfaces"] = json!(["dense-299"]);
    packet(&mut dense)["context"]["catalogues"][0]["methods"][0]["declaringClass"] =
        "dense-0".into();
    change_plan(&mut dense)?;
    let error = decode_with(
        &dense,
        CaptureLimits {
            ancestry_steps: 400,
            ..CaptureLimits::default()
        },
    )
    .expect_err("Dense edge work exceeds the small traversal budget");
    let error = error
        .downcast_ref::<android_tools::project_tree_facts::FactsUnavailable>()
        .context("Structured bounded-work failure")?;
    assert_eq!(error.reason, FactsUnavailableReason::UnsupportedShape);
    assert_eq!(
        decode_with(
            &dense,
            CaptureLimits {
                ancestry_steps: 200_000,
                ..CaptureLimits::default()
            }
        )?
        .raw_events()
        .len(),
        2
    );
    Ok(())
}
#[test]
fn legacy_basic_and_p2a_preservation() -> Result<()> {
    let mut fixture = synthetic_fixture()?;
    fixture
        .wire
        .as_object_mut()
        .context("Record")?
        .remove("kotlinFacts");
    let output = model_output(&fixture.wire)?;
    parse_model(&output, &fixture.model.root)?;
    parse_import_facts(&output, &fixture.model, import_binding())?;
    rejects(&fixture, FactsUnavailableReason::MissingMetadata)?;
    fixture.wire["kotlinFacts"] = json!({"invented":"invalid"});
    parse_model(&model_output(&fixture.wire)?, &fixture.model.root)?;
    parse_import_facts(
        &model_output(&fixture.wire)?,
        &fixture.model,
        import_binding(),
    )?;
    rejects(&fixture, FactsUnavailableReason::Malformed)
}
