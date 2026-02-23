//! Multi-model routing for cost-optimized agent loops.
//!
//! Use a cheaper model for orchestration rounds (tool selection, planning)
//! and a more capable model for synthesis rounds (final output generation).
//! Can also route by task type or round number.

/// Model routing strategy.
#[derive(Debug, Clone)]
pub enum RoutingStrategy {
    /// Use a single model for all rounds.
    Single(String),
    /// Use a cheaper model for orchestration, capable model for synthesis.
    CheapOrchestration {
        orchestration_model: String,
        synthesis_model: String,
    },
    /// Use different models based on round number.
    RoundBased {
        /// Model for early rounds (exploration/planning).
        early_model: String,
        /// Model for later rounds (synthesis/output).
        late_model: String,
        /// Round at which to switch models.
        switch_at_round: u32,
    },
}

impl RoutingStrategy {
    /// Get the model to use for a given round.
    pub fn model_for_round(&self, round: u32, is_synthesis_round: bool) -> &str {
        match self {
            RoutingStrategy::Single(model) => model,
            RoutingStrategy::CheapOrchestration {
                orchestration_model,
                synthesis_model,
            } => {
                if is_synthesis_round {
                    synthesis_model
                } else {
                    orchestration_model
                }
            }
            RoutingStrategy::RoundBased {
                early_model,
                late_model,
                switch_at_round,
            } => {
                if round >= *switch_at_round {
                    late_model
                } else {
                    early_model
                }
            }
        }
    }
}

impl Default for RoutingStrategy {
    fn default() -> Self {
        RoutingStrategy::Single(crate::DEFAULT_MODEL.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_model() {
        let strategy = RoutingStrategy::Single("claude-sonnet".into());
        assert_eq!(strategy.model_for_round(1, false), "claude-sonnet");
        assert_eq!(strategy.model_for_round(10, true), "claude-sonnet");
    }

    #[test]
    fn cheap_orchestration() {
        let strategy = RoutingStrategy::CheapOrchestration {
            orchestration_model: "haiku".into(),
            synthesis_model: "sonnet".into(),
        };
        assert_eq!(strategy.model_for_round(1, false), "haiku");
        assert_eq!(strategy.model_for_round(1, true), "sonnet");
    }

    #[test]
    fn round_based() {
        let strategy = RoutingStrategy::RoundBased {
            early_model: "haiku".into(),
            late_model: "opus".into(),
            switch_at_round: 5,
        };
        assert_eq!(strategy.model_for_round(3, false), "haiku");
        assert_eq!(strategy.model_for_round(5, false), "opus");
        assert_eq!(strategy.model_for_round(10, false), "opus");
    }
}
