use super::{import_service_schema, load_service_document, Authority, BackendConfig};
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::Path;

impl BackendConfig {
    pub fn load_schemas(&mut self, root: &Path) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        for relative in self.schemas.clone() {
            let path = root.join(&relative);
            let document = load_service_document(&path)?;
            for imported in import_service_schema(&document) {
                if let Some(declared) = self.operations.iter_mut().find(|operation| {
                    operation.id == imported.id && operation.authority == Authority::Declared
                }) {
                    if declared.input.is_none() {
                        declared.input = imported.input;
                    }
                    if declared.output.is_none() {
                        declared.output = imported.output;
                    }
                    declared
                        .outputs_by_status
                        .extend(imported.outputs_by_status);
                    declared.success_statuses.extend(imported.success_statuses);
                    declared.success_statuses.sort_unstable();
                    declared.success_statuses.dedup();
                    declared.read_only |= imported.read_only;
                    declared.idempotent |= imported.idempotent;
                } else {
                    self.operations.push(imported);
                }
            }
        }
        let mut seen = BTreeSet::new();
        self.operations
            .retain(|operation| seen.insert((operation.id.clone(), operation.authority)));
        Ok(())
    }
}
