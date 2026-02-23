pub struct Engine {}

impl Engine {
    pub fn start(&self) {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = async { /* Implementation details */ } => {}
                }
            }
        });
    }

    pub fn new() -> Self {
        Engine {}
    }
}
