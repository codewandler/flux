//! Lowering of semantic [`FlowEffect`]s onto the host-resource [`Effect`] + policy [`Action`] that
//! the existing authorization bridge understands. This keeps `flux_spec::Effect` host-resource-shaped
//! while letting operations declare workflow *meaning* (sends mail, costs money, …).

use flux_policy::Action;
use flux_spec::Effect;

use crate::ast::FlowEffect;

impl FlowEffect {
    /// Lower this semantic effect to the host-resource [`Effect`] it implies (if any) and the
    /// additional policy [`Action`] it requires (if any).
    ///
    /// Host effects reuse the existing `effect_requests` bridge in `flux-runtime`; semantic-only
    /// effects (`SendExternal`, `Money`, `Calendar`, …) carry a dedicated `flow.*` action a policy
    /// `Grant` can allow / deny / require-approval (`flow.delete` / `flow.money` are denied by
    /// default in policy).
    pub fn lower(self) -> (Option<Effect>, Option<Action>) {
        use FlowEffect::*;
        match self {
            Pure => (None, None),
            Read => (Some(Effect::Read), None),
            Model => (None, Some(Action::from("model.invoke"))),
            Network => (Some(Effect::Network), None),
            WriteFile => (Some(Effect::Write), None),
            WriteDb => (Some(Effect::Network), Some(Action::from("flow.write_db"))),
            SendExternal => (
                Some(Effect::Network),
                Some(Action::from("flow.send_external")),
            ),
            Delete => (Some(Effect::Write), Some(Action::from("flow.delete"))),
            Money => (None, Some(Action::from("flow.money"))),
            Calendar => (None, Some(Action::from("flow.calendar"))),
            HumanVisible => (None, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
