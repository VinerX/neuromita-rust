use super::{EffectPhase, diagnostics::DiagnosticState};

#[derive(Debug, Clone)]
pub struct RequestTransition {
    pub revision: u64,
    pub phase: EffectPhase,
    pub state: DiagnosticState,
}
