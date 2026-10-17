// Streaming hub (stub; Phase 5+).

/// In-process stream hub. Phase 5 implements real SSE/WebSocket fan-out.
#[derive(Debug, Clone, Default)]
pub struct Hub;

impl Hub {
    pub fn new() -> Self {
        Self
    }
}

/// Creates a hub and a sink handle. Stubbed for Phase 0 compilation.
pub fn hub_and_sink(_capacity: usize) -> (Hub, Hub) {
    (Hub::new(), Hub::new())
}
