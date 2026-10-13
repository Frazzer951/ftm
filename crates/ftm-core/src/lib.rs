pub mod config;
pub mod model;
pub mod paths;
pub mod protocol;

pub const ENV_SOCKET: &str = "FTM_SOCKET";
pub const ENV_WORKSPACE_ID: &str = "FTM_WORKSPACE_ID";
pub const ENV_PANE_ID: &str = "FTM_PANE_ID";
pub const ENV_NAME: &str = "FTM_NAME";
/// Read by ftmd: the command an agent pane runs instead of `claude` (tests point it at a fake agent).
pub const ENV_AGENT_CMD: &str = "FTM_AGENT_CMD";
