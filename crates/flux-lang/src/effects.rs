//! Tests for the lowering of semantic [`FlowEffect`]s onto the host-resource [`Effect`] + policy
//! [`Action`] that the existing authorization bridge understands.
//!
//! `FlowEffect` and its `lower` live in `flux-spec` (C-141): the enum is plugin wire vocabulary, so
//! a guest must be able to name it without compiling the language front-end. The lowering contract
//! is exercised here, where the language's own effect handling can regress against it.

#[cfg(test)]
mod tests {
    use crate::ast::FlowEffect;
    use flux_policy::Action;
    use flux_spec::Effect;

    #[test]
    fn host_effects_map_to_flux_spec_effects() {
        assert_eq!(FlowEffect::Read.lower(), (Some(Effect::Read), None));
        assert_eq!(FlowEffect::WriteFile.lower(), (Some(Effect::Write), None));
        assert_eq!(FlowEffect::Network.lower(), (Some(Effect::Network), None));
        assert_eq!(FlowEffect::Pure.lower(), (None, None));
    }

    #[test]
    fn semantic_effects_carry_a_flow_action() {
        assert_eq!(
            FlowEffect::SendExternal.lower(),
            (
                Some(Effect::Network),
                Some(Action::from("flow.send_external"))
            )
        );
        assert_eq!(
            FlowEffect::Money.lower(),
            (None, Some(Action::from("flow.money")))
        );
        assert_eq!(
            FlowEffect::Model.lower(),
            (None, Some(Action::from("model.invoke")))
        );
    }
}
