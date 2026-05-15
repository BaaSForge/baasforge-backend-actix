#[derive(Debug, Clone)]
pub struct FlowResolution {
    pub flow_id: &'static str,
    pub step_id: &'static str,
    pub allowed_tool_names: Vec<&'static str>,
    pub prompt_appendix: Option<String>,
}

pub struct FlowStep<C: 'static> {
    pub id: &'static str,
    pub when: fn(&C) -> bool,
    pub allowed_tools: fn(&C) -> Vec<&'static str>,
    pub prompt_appendix: fn(&C) -> Option<String>,
}

pub struct FlowDefinition<C: 'static> {
    pub id: &'static str,
    pub steps: &'static [FlowStep<C>],
    pub fallback: FlowStep<C>,
}

pub fn resolve_flow<C: 'static>(def: &FlowDefinition<C>, ctx: &C) -> FlowResolution {
    let step = def
        .steps
        .iter()
        .find(|s| (s.when)(ctx))
        .unwrap_or(&def.fallback);

    FlowResolution {
        flow_id: def.id,
        step_id: step.id,
        allowed_tool_names: (step.allowed_tools)(ctx),
        prompt_appendix: (step.prompt_appendix)(ctx),
    }
}
