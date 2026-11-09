use groove_core::{
    AcquireOptions, CleanupPolicy, Error, ErrorCode, Lease, LeaseId, Pool, PoolOptions, SlotId,
};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

const MAX_REQUESTS: usize = 4096;

#[derive(Clone)]
pub(crate) struct GrooveServer {
    state: Arc<Mutex<State>>,
    tool_router: ToolRouter<Self>,
}

struct State {
    repo: PathBuf,
    timeout: Duration,
    leases: BTreeMap<LeaseId, Lease>,
    requests: BTreeMap<String, (String, LeaseId)>,
    released: BTreeMap<LeaseId, SlotId>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InitInput {
    /// Fixed number of workspaces, from 1 through 4096.
    capacity: usize,
    /// pristine removes ignored files; warm may retain them until a rebuild.
    #[serde(default)]
    cleanup: Cleanup,
    /// Optional dedicated empty absolute checkout directory.
    worktree_root: Option<PathBuf>,
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Cleanup {
    #[default]
    Pristine,
    Warm,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AcquireInput {
    /// Commit, tag or branch to resolve before preparing the workspace.
    base: String,
    /// Unique task key (1..128 bytes). Retrying the same key/base on THIS server returns the same active lease.
    request_key: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReleaseInput {
    /// Slot returned by acquire or listed by groove_leases.
    slot_id: String,
    /// Acquisition UUID. A previous generation cannot release a successor.
    lease_id: String,
    /// Must be true only after all writers have stopped and results are saved.
    writers_stopped: bool,
}

impl GrooveServer {
    pub fn new(repo: PathBuf, timeout: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                repo,
                timeout,
                leases: BTreeMap::new(),
                requests: BTreeMap::new(),
                released: BTreeMap::new(),
            })),
            tool_router: Self::tool_router(),
        }
    }

    async fn operation(
        &self,
        operation: impl FnOnce(&mut State) -> groove_core::Result<Value> + Send + 'static,
    ) -> CallToolResult {
        let state = self.state.clone();
        // All mutations complete inside the blocking job, even if MCP cancels its
        // response future. An exposed path is protected before entering the map.
        let result = tokio::task::spawn_blocking(move || {
            let mut state = state.lock().map_err(|_| {
                Error::new(
                    ErrorCode::IoError,
                    "MCP lease state is poisoned; restart and inspect protected leases",
                )
            })?;
            operation(&mut state)
        })
        .await;
        match result {
            Ok(Ok(value)) => CallToolResult::structured(json!({"ok":true,"result":value})),
            Ok(Err(error)) => tool_error(error),
            Err(error) => tool_error(
                Error::new(ErrorCode::IoError, "MCP operation failed").with_source(error),
            ),
        }
    }
}

fn tool_error(error: Error) -> CallToolResult {
    CallToolResult::structured_error(json!({"ok":false,"error":{
        "code":error.code,"message":error.message,"retryable":error.retryable,"details":error.details
    }}))
}

#[tool_router]
impl GrooveServer {
    #[tool(
        name = "groove_init",
        description = "Initialize this repository's fixed workspace pool. Repeat only with identical settings. Existing slots/results are disposable; pristine cleanup removes ignored files."
    )]
    async fn init(&self, Parameters(input): Parameters<InitInput>) -> CallToolResult {
        self.operation(move |state| {
            let result = Pool::init(
                &state.repo,
                PoolOptions {
                    capacity: input.capacity,
                    cleanup_policy: match input.cleanup {
                        Cleanup::Pristine => CleanupPolicy::Pristine,
                        Cleanup::Warm => CleanupPolicy::Warm,
                    },
                    worktree_root: input.worktree_root,
                    git_timeout: state.timeout,
                },
            )?;
            Ok(serde_json::to_value(result)?)
        })
        .await
    }

    #[tool(
        name = "groove_acquire",
        description = "Reserve and verify a workspace at base. Use only the returned worktree_path. Reuse the same request_key/base to retry a lost response on this server. Reservation survives disconnect/crash until explicit release; never delete lock/protection files. If full, retry later instead of creating overflow worktrees."
    )]
    async fn acquire(&self, Parameters(input): Parameters<AcquireInput>) -> CallToolResult {
        self.operation(move |state| {
            if input.request_key.is_empty() || input.request_key.len() > 128 || input.base.is_empty() || input.base.len() > 4096 {
                return Err(Error::new(ErrorCode::InvalidArgument, "request_key must be 1..128 bytes and base 1..4096 bytes"));
            }
            if let Some((base, id)) = state.requests.get(&input.request_key) {
                if base != &input.base {
                    return Err(Error::new(ErrorCode::InvalidArgument, "request_key was already used with another base"));
                }
                let lease = state.leases.get(id).ok_or_else(|| Error::new(ErrorCode::InvalidLease, "This request_key was already released; use a new key for a new task"))?;
                return Ok(json!({"request_key":input.request_key,"lease":lease.info(),"reuse_protected":true}));
            }
            if state.requests.len() >= MAX_REQUESTS {
                return Err(Error::new(ErrorCode::InvalidArgument, "Session request limit reached; start a new server after releasing active tasks"));
            }
            let pool = Pool::open(&state.repo)?;
            let lease = pool.acquire(AcquireOptions { base: input.base.clone(), git_timeout: state.timeout, ..Default::default() })?;
            lease.protect_from_reuse()?;
            let id = lease.info().lease_id;
            let result = json!({"request_key":input.request_key,"lease":lease.info(),"reuse_protected":true});
            state.requests.insert(input.request_key, (input.base, id));
            state.leases.insert(id, lease);
            Ok(result)
        }).await
    }

    #[tool(
        name = "groove_leases",
        description = "List protected leases in this repository, including abandoned reservations from a prior server. owned_by_this_server is a handle check, not a proof that external writers have stopped. Inspect these after a lost response or server restart.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn leases(&self) -> CallToolResult {
        self.operation(|state| {
            let pool = Pool::open(&state.repo)?;
            let leases: Vec<_> = pool.protected_leases()?.into_iter().map(|info| {
                let request_key = state.requests.iter().find(|(_, (_, id))| *id == info.lease_id).map(|(key, _)| key);
                json!({"owned_by_this_server":state.leases.contains_key(&info.lease_id),"request_key":request_key,"lease":info})
            }).collect();
            Ok(json!({"leases":leases}))
        }).await
    }

    #[tool(
        name = "groove_release",
        description = "Release a workspace only AFTER saving results outside its disposable contents and stopping every writer/watcher. Set writers_stopped=true to confirm. Also releases abandoned protected leases after their owner has exited. Never use a released path until acquired again."
    )]
    async fn release(&self, Parameters(input): Parameters<ReleaseInput>) -> CallToolResult {
        self.operation(move |state| {
            if !input.writers_stopped {
                return Err(Error::new(
                    ErrorCode::InvalidArgument,
                    "Stop all writers and save results before confirming writers_stopped=true",
                ));
            }
            let slot: SlotId = input.slot_id.parse()?;
            let id: LeaseId = input.lease_id.parse()?;
            if let Some(previous_slot) = state.released.get(&id) {
                if previous_slot != &slot {
                    return Err(Error::new(
                        ErrorCode::LeaseMismatch,
                        "Released lease slot mismatch",
                    ));
                }
                return Ok(
                    json!({"slot_id":slot,"lease_id":id,"released":true,"cleanup":"deferred"}),
                );
            }
            if state.released.len() >= MAX_REQUESTS {
                return Err(Error::new(
                    ErrorCode::InvalidArgument,
                    "Release history limit reached; restart server and inspect protected leases",
                ));
            }
            let result = if let Some(lease) = state.leases.get_mut(&id) {
                if lease.info().slot_id != slot {
                    return Err(Error::new(
                        ErrorCode::LeaseMismatch,
                        "Active lease slot mismatch",
                    ));
                }
                lease.release()?
            } else {
                Pool::open(&state.repo)?.release_protected(&slot, id)?
            };
            state.leases.remove(&id);
            state.released.insert(id, slot);
            Ok(serde_json::to_value(result)?)
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GrooveServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("groove-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions("Initialize the pool, acquire before editing, and use the returned path for all project tools. Keep results on a task-owned branch or export them outside the workspace. Stop every writer before release. Leases stay protected after disconnect/crash: list and explicitly release abandoned leases only after stopping their writers. Never infer ownership from a path or UUID. This server is scoped to one local repository.")
    }
}
