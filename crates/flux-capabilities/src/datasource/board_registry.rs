//! Typed multi-board registry (A-134).
//!
//! The registry owns no backend IO. It validates contracts at composition time, keeps source
//! labels for diagnostics, resolves omitted selectors only when unambiguous, and optionally holds
//! the existing execution [`WorkBoard`] port for tool installation.

use std::collections::BTreeMap;
use std::sync::Arc;

use flux_core::{Error, Result};
use flux_datasource::board::{BoardContract, BoardId, BoardProfile};

use super::board::WorkBoard;

/// One validated registry entry.
#[derive(Clone)]
pub struct BoardBinding {
    /// Pure contract shared across SDK, model tools and fleet.
    pub contract: BoardContract,
    /// Existing execution backend, present only for execution-profile tool projection.
    pub execution: Option<Arc<dyn WorkBoard>>,
}

impl std::fmt::Debug for BoardBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoardBinding")
            .field("contract", &self.contract)
            .field("execution", &self.execution.as_ref().map(|_| "WorkBoard"))
            .finish()
    }
}

/// Validated board bindings keyed by stable id.
#[derive(Clone, Debug, Default)]
pub struct BoardRegistry {
    bindings: BTreeMap<BoardId, BoardBinding>,
}

impl BoardRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate and register a pure contract. Duplicate ids are rejected with both sources.
    pub fn register(&mut self, contract: BoardContract) -> Result<()> {
        self.register_binding(BoardBinding {
            contract,
            execution: None,
        })
    }

    /// Validate and register the shipped execution backend.
    pub fn register_execution(
        &mut self,
        contract: BoardContract,
        backend: Arc<dyn WorkBoard>,
    ) -> Result<()> {
        if contract.profile != BoardProfile::Execution {
            return Err(Error::Config(format!(
                "board `{}` from {} supplies a WorkBoard execution backend for {:?} profile",
                contract.id, contract.source, contract.profile
            )));
        }
        self.register_binding(BoardBinding {
            contract,
            execution: Some(backend),
        })
    }

    fn register_binding(&mut self, binding: BoardBinding) -> Result<()> {
        binding
            .contract
            .validate()
            .map_err(|error| Error::Config(error.to_string()))?;
        if let Some(existing) = self.bindings.get(&binding.contract.id) {
            return Err(Error::Config(format!(
                "duplicate board `{}`: {} conflicts with {}",
                binding.contract.id, existing.contract.source, binding.contract.source
            )));
        }
        self.bindings.insert(binding.contract.id.clone(), binding);
        Ok(())
    }

    /// Resolve a board for an operation. An omitted selector is legal only with one candidate.
    pub fn resolve(&self, board: Option<&BoardId>, operation: &str) -> Result<&BoardBinding> {
        if let Some(board) = board {
            let binding = self.bindings.get(board).ok_or_else(|| {
                Error::Config(format!(
                    "unknown board `{board}`; registered boards: {}",
                    self.ids_text()
                ))
            })?;
            if !binding.contract.profile.supports(operation) {
                return Err(Error::Config(format!(
                    "board `{board}` profile {:?} does not support `{operation}`; supported: {}",
                    binding.contract.profile,
                    binding.contract.profile.operations().join(", ")
                )));
            }
            return Ok(binding);
        }

        let candidates = self
            .bindings
            .values()
            .filter(|binding| binding.contract.profile.supports(operation))
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [binding] => Ok(binding),
            [] => Err(Error::Config(format!(
                "no registered board supports `{operation}`"
            ))),
            _ => Err(Error::Config(format!(
                "board selector is ambiguous for `{operation}`; candidates: {}",
                candidates
                    .iter()
                    .map(|binding| binding.contract.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    /// Resolve an execution port, preserving the same selector/operation ambiguity rules.
    pub fn resolve_execution(
        &self,
        board: Option<&BoardId>,
        operation: &str,
    ) -> Result<(&BoardContract, Arc<dyn WorkBoard>)> {
        let binding = self.resolve(board, operation)?;
        let backend = binding.execution.clone().ok_or_else(|| {
            Error::Config(format!(
                "board `{}` has no execution WorkBoard backend",
                binding.contract.id
            ))
        })?;
        Ok((&binding.contract, backend))
    }

    /// Stable registration order.
    pub fn bindings(&self) -> impl Iterator<Item = &BoardBinding> {
        self.bindings.values()
    }

    /// Number of registered bindings.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Whether there are no registered bindings.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    fn ids_text(&self) -> String {
        if self.bindings.is_empty() {
            "<none>".into()
        } else {
            self.bindings
                .keys()
                .map(BoardId::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_datasource::board::{BoardBackend, BoardScope};

    fn contract(id: &str, profile: BoardProfile) -> BoardContract {
        BoardContract {
            id: BoardId::new(id).unwrap(),
            scope: BoardScope::Repository {
                repository_id: "repo".into(),
            },
            profile,
            backend: flux_datasource::board::BoardBackend::Memory,
            source: format!("test {id}"),
        }
    }

    #[test]
    fn duplicate_ids_name_both_sources() {
        let mut registry = BoardRegistry::new();
        registry
            .register(contract("planning", BoardProfile::Planning))
            .unwrap();
        let mut duplicate = contract("planning", BoardProfile::General);
        duplicate.source = "second file".into();
        let error = registry.register(duplicate).unwrap_err().to_string();
        assert!(error.contains("test planning"), "{error}");
        assert!(error.contains("second file"), "{error}");
    }

    #[test]
    fn omitted_selector_refuses_two_compatible_boards() {
        let mut registry = BoardRegistry::new();
        registry
            .register(contract("one", BoardProfile::Planning))
            .unwrap();
        registry
            .register(contract("two", BoardProfile::Planning))
            .unwrap();
        let error = registry.resolve(None, "get").unwrap_err().to_string();
        assert!(error.contains("one, two"), "{error}");
    }

    #[test]
    fn scope_backend_mismatch_is_rejected_before_use() {
        let mut value = contract("bad", BoardProfile::Planning);
        value.scope = BoardScope::Workspace {
            workspace_id: "all".into(),
        };
        value.backend = BoardBackend::Track;
        let error = BoardRegistry::new()
            .register(value)
            .unwrap_err()
            .to_string();
        assert!(error.contains("incompatible"), "{error}");
    }

    #[test]
    fn profiles_expose_exact_closed_operation_sets() {
        assert_eq!(BoardProfile::General.operations().len(), 8);
        assert_eq!(BoardProfile::Planning.operations().len(), 9);
        assert_eq!(BoardProfile::Execution.operations().len(), 11);
        assert!(BoardProfile::Planning.supports("update"));
        assert!(!BoardProfile::Planning.supports("claim"));
        assert!(BoardProfile::Execution.supports("record_dispatch"));
    }
}
