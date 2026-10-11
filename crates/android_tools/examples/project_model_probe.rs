use android_tools::{
    java,
    project_model::{self, VariantId},
};
use anyhow::{Context as _, Result};
use std::{env, fs, path::PathBuf, sync::Arc};

fn main() -> Result<()> {
    let mut arguments = env::args().skip(1);
    let root = PathBuf::from(arguments.next().context("Expected project root")?).canonicalize()?;
    let output = fs::read_to_string(arguments.next().context("Expected Gradle output file")?)?;
    let module = arguments
        .next()
        .context("Expected module path or --model-only")?;
    let model = Arc::new(project_model::parse_model(&output, &root)?);
    if module == "--model-only" {
        println!("{}", serde_json::to_string_pretty(&model)?);
        return Ok(());
    }
    let selected = VariantId {
        module,
        variant: arguments.next().context("Expected variant name")?,
    };
    let graph = model.select(selected)?;
    let (export, selection) = java::prepare_selected(&root, &graph)?;
    if let Some(java_output) = arguments.next() {
        let target = model
            .targets()
            .into_iter()
            .find(|target| VariantId::from(target) == graph.selected)
            .context("Expected an application target")?;
        let modules = java::parse_model(&fs::read_to_string(java_output)?, &root, &target)?;
        java::validate_selection(&modules, &graph)?;
        java::install_model(&root, &modules)?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "selected": graph.selected, "variants": graph.variants, "kotlinVariants": graph.kotlin_variants(),
            "javaExport": export, "javaSelection": selection,
            "modules": graph.modules().map(|(module, variant)| serde_json::json!({"path": module.path, "kind": module.kind, "components": variant.components})).collect::<Vec<_>>()
        }))?
    );
    Ok(())
}
