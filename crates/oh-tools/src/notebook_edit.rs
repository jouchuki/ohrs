//! Edit Jupyter notebook cells tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use std::path::{Path, PathBuf};

pub struct NotebookEditTool;

#[async_trait]
impl crate::traits::Tool for NotebookEditTool {
    fn name(&self) -> &str {
        "NotebookEdit"
    }

    fn description(&self) -> &str {
        "Edit Jupyter notebook cells"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "notebook_path": {
                    "type": "string",
                    "description": "Path to the .ipynb notebook file"
                },
                "cell_index": {
                    "type": "integer",
                    "description": "Zero-based cell index to edit"
                },
                "new_source": {
                    "type": "string",
                    "description": "New source content for the cell"
                }
            },
            "required": ["notebook_path", "cell_index", "new_source"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let notebook_path = match arguments.get("notebook_path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Missing required parameter: notebook_path"),
        };

        let cell_index = match arguments.get("cell_index").and_then(|v| v.as_u64()) {
            Some(i) => i as usize,
            None => return ToolResult::error("Missing required parameter: cell_index"),
        };

        let new_source = match arguments.get("new_source").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required parameter: new_source"),
        };

        let path = resolve_path(&context.cwd, notebook_path);

        match edit_notebook_cell(&path, cell_index, new_source) {
            Ok(()) => ToolResult::success(format!(
                "Updated cell {} in {}",
                cell_index,
                path.display()
            )),
            Err(e) => ToolResult::error(format!("Failed to edit notebook: {}", e)),
        }
    }
}

fn resolve_path(base: &Path, candidate: &str) -> PathBuf {
    let p = PathBuf::from(candidate);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

fn edit_notebook_cell(
    path: &Path,
    cell_index: usize,
    new_source: &str,
) -> Result<(), String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;

    let mut notebook: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Invalid JSON: {}", e))?;

    let cells = notebook
        .get_mut("cells")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| "Notebook has no 'cells' array".to_string())?;

    if cell_index >= cells.len() {
        return Err(format!(
            "Cell index {} out of range (notebook has {} cells)",
            cell_index,
            cells.len()
        ));
    }

    cells[cell_index]["source"] = serde_json::Value::String(new_source.to_string());

    let output = serde_json::to_string_pretty(&notebook)
        .map_err(|e| format!("Failed to serialize: {}", e))?;

    std::fs::write(path, format!("{}\n", output))
        .map_err(|e| format!("Cannot write {}: {}", path.display(), e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;
    use std::path::PathBuf;

    fn sample_notebook() -> serde_json::Value {
        serde_json::json!({
            "cells": [
                {
                    "cell_type": "code",
                    "metadata": {},
                    "source": "print('hello')",
                    "outputs": [],
                    "execution_count": null
                },
                {
                    "cell_type": "markdown",
                    "metadata": {},
                    "source": "# Title"
                }
            ],
            "metadata": {
                "language_info": {"name": "python"}
            },
            "nbformat": 4,
            "nbformat_minor": 5
        })
    }

    #[test]
    fn test_edit_notebook_cell_updates_source() {
        let dir = tempfile::tempdir().unwrap();
        let nb_path = dir.path().join("test.ipynb");
        let nb = sample_notebook();
        std::fs::write(&nb_path, serde_json::to_string_pretty(&nb).unwrap()).unwrap();

        edit_notebook_cell(&nb_path, 0, "print('world')").unwrap();

        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&nb_path).unwrap()).unwrap();
        assert_eq!(updated["cells"][0]["source"], "print('world')");
        // Other cell unchanged
        assert_eq!(updated["cells"][1]["source"], "# Title");
    }

    #[test]
    fn test_edit_notebook_cell_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        let nb_path = dir.path().join("test.ipynb");
        let nb = sample_notebook();
        std::fs::write(&nb_path, serde_json::to_string_pretty(&nb).unwrap()).unwrap();

        let result = edit_notebook_cell(&nb_path, 5, "x");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("out of range"));
    }

    #[test]
    fn test_edit_notebook_missing_file() {
        let result = edit_notebook_cell(Path::new("/nonexistent/test.ipynb"), 0, "x");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notebook_edit_tool_execute() {
        let dir = tempfile::tempdir().unwrap();
        let nb_path = dir.path().join("test.ipynb");
        let nb = sample_notebook();
        std::fs::write(&nb_path, serde_json::to_string_pretty(&nb).unwrap()).unwrap();

        let tool = NotebookEditTool;
        let ctx = ToolExecutionContext::new(dir.path().to_path_buf());
        let result = tool
            .execute(
                serde_json::json!({
                    "notebook_path": nb_path.to_str().unwrap(),
                    "cell_index": 1,
                    "new_source": "## Updated Title"
                }),
                &ctx,
            )
            .await;

        assert!(!result.is_error);
        assert!(result.output.contains("Updated cell 1"));

        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&nb_path).unwrap()).unwrap();
        assert_eq!(updated["cells"][1]["source"], "## Updated Title");
    }

    #[tokio::test]
    async fn test_notebook_edit_missing_params() {
        let tool = NotebookEditTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
        assert!(result.output.contains("notebook_path"));
    }
}
