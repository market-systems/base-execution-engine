use common::{DecisionStatus, ExecutionDecision, Mode, RoutePlan, SimulationResult, SimulationStatus};
use ethers::types::I256;

#[derive(Debug, Clone, Copy)]
pub struct ViabilityPolicy {
    mode: Mode,
    min_confidence_bps: u16,
    max_route_gas: u64,
    min_net_surplus: I256,
    require_post_trigger: bool,
}

impl ViabilityPolicy {
    pub fn new(
        mode: Mode,
        min_confidence_bps: u16,
        max_route_gas: u64,
        min_net_surplus: u64,
        require_post_trigger: bool,
    ) -> Self {
        Self {
            mode,
            min_confidence_bps,
            max_route_gas,
            min_net_surplus: I256::from(min_net_surplus),
            require_post_trigger,
        }
    }

    pub fn evaluate(&self, plan: RoutePlan, simulation: SimulationResult) -> ExecutionDecision {
        if simulation.status != SimulationStatus::Success {
            return denied(
                "simulation did not complete successfully",
                Some(plan),
                Some(simulation),
            );
        }

        if !plan.is_cycle() {
            return denied(
                "planned route does not settle back into the settlement token",
                Some(plan),
                Some(simulation),
            );
        }

        if !plan.is_executable() {
            return denied(
                "planned route contains at least one non-executable venue",
                Some(plan),
                Some(simulation),
            );
        }

        if plan.estimated_gas > self.max_route_gas {
            return denied(
                format!(
                    "estimated route gas {} exceeds configured cap {}",
                    plan.estimated_gas, self.max_route_gas
                ),
                Some(plan),
                Some(simulation),
            );
        }

        if simulation.confidence_bps < self.min_confidence_bps {
            return denied(
                format!(
                    "simulation confidence {}bps below configured minimum {}bps",
                    simulation.confidence_bps, self.min_confidence_bps
                ),
                Some(plan),
                Some(simulation),
            );
        }

        if self.require_post_trigger && !simulation.post_trigger_checked {
            return denied(
                "post-trigger simulation is required but was not available",
                Some(plan),
                Some(simulation),
            );
        }

        if simulation.net_surplus < self.min_net_surplus {
            return denied(
                format!(
                    "net surplus {} below configured minimum {}",
                    simulation.net_surplus, self.min_net_surplus
                ),
                Some(plan),
                Some(simulation),
            );
        }

        let reason = match self.mode {
            Mode::Research => "candidate passed viability checks in research mode",
            Mode::Shadow => "candidate passed viability checks in shadow mode",
            Mode::Live => "candidate passed viability checks in live mode",
        };

        ExecutionDecision {
            status: DecisionStatus::Approved,
            reason: reason.to_string(),
            plan: Some(plan),
            simulation: Some(simulation),
        }
    }
}

fn denied(
    reason: impl Into<String>,
    plan: Option<RoutePlan>,
    simulation: Option<SimulationResult>,
) -> ExecutionDecision {
    ExecutionDecision {
        status: DecisionStatus::Denied,
        reason: reason.into(),
        plan,
        simulation,
    }
}
