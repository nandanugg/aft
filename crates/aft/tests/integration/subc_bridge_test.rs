use std::collections::HashSet;
use std::net::TcpListener as StdTcpListener;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use aft::callgraph::CallGraph;
use aft::config::Config;
use aft::context::AppContext;
use aft::executor::{Executor, ExecutorConfig};
use aft::harness::Harness;
use aft::parser::TreeSitterProvider;
use aft::protocol::{RawRequest, Response};
use aft::subc::run_subc_mode;
use serde_json::{json, Value};
use subc_protocol::manifest::ModuleManifest;
use subc_protocol::session::{ModuleControlRequest, ModuleControlResponse};
use subc_protocol::{
    BindIdentity, Flags, Frame, FrameType, ModuleHelloAckBody, ModuleHelloBody, Priority,
    RouteTarget, PROTOCOL_VERSION,
};
use subc_transport::connection_file::{self, ConnectionInfo, Endpoint, SCHEMA_VERSION};
use subc_transport::{authenticate_server, read_frame, write_frame};
use tokio::net::TcpListener;

static BRIDGE_STATE: OnceLock<Arc<BridgeState>> = OnceLock::new();

struct FakeDaemonInput {
    listener: TcpListener,
    key: Vec<u8>,
    daemon_id: [u8; subc_transport::DAEMON_ID_LEN],
    root1: std::path::PathBuf,
    root2: std::path::PathBuf,
    callgraph_root: std::path::PathBuf,
    callgraph_file: std::path::PathBuf,
    state: Arc<BridgeState>,
}

#[derive(Default)]
struct BridgeState {
    inner: Mutex<BridgeInner>,
    cv: Condvar,
}

#[derive(Default)]
struct BridgeInner {
    overlap_started: usize,
    overlap_current: usize,
    overlap_max: usize,
    overlap_release: bool,
    heavy_started: bool,
    heavy_release: bool,
    epoch_started: usize,
    epoch_current: usize,
    epoch_release: bool,
    route2_configure_started: bool,
    route2_configure_reads_at_start: Option<usize>,
}

impl BridgeState {
    fn wait_until(&self, label: &str, mut predicate: impl FnMut(&BridgeInner) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut guard = self.inner.lock().expect("bridge state lock");
        while !predicate(&guard) {
            let now = Instant::now();
            assert!(now < deadline, "timed out waiting for {label}");
            let remaining = deadline.saturating_duration_since(now);
            let (next, result) = self
                .cv
                .wait_timeout(guard, remaining)
                .expect("bridge state condvar");
            guard = next;
            assert!(
                !result.timed_out() || predicate(&guard),
                "timed out waiting for {label}"
            );
        }
    }

    fn release_overlap(&self) {
        let mut guard = self.inner.lock().expect("bridge state lock");
        guard.overlap_release = true;
        self.cv.notify_all();
    }

    fn release_heavy(&self) {
        let mut guard = self.inner.lock().expect("bridge state lock");
        guard.heavy_release = true;
        self.cv.notify_all();
    }

    fn release_epoch_reads(&self) {
        let mut guard = self.inner.lock().expect("bridge state lock");
        guard.epoch_release = true;
        self.cv.notify_all();
    }

    fn assert_overlap(&self) {
        let guard = self.inner.lock().expect("bridge state lock");
        assert!(
            guard.overlap_max >= 2,
            "expected overlapping PureRead jobs, max concurrent reads was {}",
            guard.overlap_max
        );
    }

    fn assert_route2_configure_not_started(&self) {
        let guard = self.inner.lock().expect("bridge state lock");
        assert!(
            !guard.route2_configure_started,
            "second RouteBind configure started while first-route reads were still in flight"
        );
    }

    fn assert_route2_configure_saw_no_reads(&self) {
        let guard = self.inner.lock().expect("bridge state lock");
        assert_eq!(
            guard.route2_configure_reads_at_start,
            Some(0),
            "RouteBind configure must run under the same actor epoch after reads drain"
        );
    }

    fn overlap_read(&self, id: String) -> Response {
        {
            let mut guard = self.inner.lock().expect("bridge state lock");
            guard.overlap_started += 1;
            guard.overlap_current += 1;
            guard.overlap_max = guard.overlap_max.max(guard.overlap_current);
            self.cv.notify_all();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !guard.overlap_release {
                let now = Instant::now();
                assert!(now < deadline, "timed out waiting for overlap release");
                let remaining = deadline.saturating_duration_since(now);
                let (next, result) = self
                    .cv
                    .wait_timeout(guard, remaining)
                    .expect("bridge state condvar");
                guard = next;
                assert!(
                    !result.timed_out() || guard.overlap_release,
                    "timed out waiting for overlap release"
                );
            }
            guard.overlap_current -= 1;
        }
        Response::success(id, json!({ "case": "overlap" }))
    }

    fn heavy(&self, id: String) -> Response {
        {
            let mut guard = self.inner.lock().expect("bridge state lock");
            guard.heavy_started = true;
            self.cv.notify_all();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !guard.heavy_release {
                let now = Instant::now();
                assert!(now < deadline, "timed out waiting for heavy release");
                let remaining = deadline.saturating_duration_since(now);
                let (next, result) = self
                    .cv
                    .wait_timeout(guard, remaining)
                    .expect("bridge state condvar");
                guard = next;
                assert!(
                    !result.timed_out() || guard.heavy_release,
                    "timed out waiting for heavy release"
                );
            }
        }
        Response::success(id, json!({ "case": "heavy" }))
    }

    fn epoch_read(&self, id: String) -> Response {
        {
            let mut guard = self.inner.lock().expect("bridge state lock");
            guard.epoch_started += 1;
            guard.epoch_current += 1;
            self.cv.notify_all();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !guard.epoch_release {
                let now = Instant::now();
                assert!(now < deadline, "timed out waiting for epoch release");
                let remaining = deadline.saturating_duration_since(now);
                let (next, result) = self
                    .cv
                    .wait_timeout(guard, remaining)
                    .expect("bridge state condvar");
                guard = next;
                assert!(
                    !result.timed_out() || guard.epoch_release,
                    "timed out waiting for epoch release"
                );
            }
            guard.epoch_current -= 1;
        }
        Response::success(id, json!({ "case": "epoch" }))
    }

    fn configure(&self, req: &RawRequest) {
        if req.id == "subc-bind-2" {
            let mut guard = self.inner.lock().expect("bridge state lock");
            guard.route2_configure_started = true;
            guard.route2_configure_reads_at_start = Some(guard.epoch_current);
            self.cv.notify_all();
        }
    }
}

fn configure_bridge_context(req: &RawRequest, ctx: &AppContext) -> Response {
    let root = match req.params.get("project_root").and_then(Value::as_str) {
        Some(root) => std::path::PathBuf::from(root),
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "configure: missing required param 'project_root'",
            );
        }
    };
    let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());

    ctx.update_config(|config| {
        config.project_root = Some(root.clone());
        config.harness = Some(Harness::Opencode);
        config.callgraph_store = false;
        config.search_index = false;
        config.semantic_search = false;
    });
    ctx.set_harness(Harness::Opencode);
    ctx.set_canonical_cache_root(canonical_root);
    ctx.set_cache_role(false, None);
    *ctx.callgraph().lock() = Some(CallGraph::new(root.clone()));
    *ctx.callgraph_store()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    *ctx.callgraph_store_rx().lock() = None;

    Response::success(
        &req.id,
        json!({
            "configured": true,
            "project_root": root.to_string_lossy(),
        }),
    )
}

fn bridge_dispatch(req: RawRequest, ctx: &AppContext) -> Response {
    let state = Arc::clone(BRIDGE_STATE.get().expect("bridge state installed"));
    match req.command.as_str() {
        "configure" => {
            state.configure(&req);
            configure_bridge_context(&req, ctx)
        }
        "read" => match req.params.get("case").and_then(Value::as_str) {
            Some("overlap") => state.overlap_read(req.id),
            Some("fast") => Response::success(req.id, json!({ "case": "fast" })),
            Some("epoch") => state.epoch_read(req.id),
            other => Response::error(
                req.id,
                "unexpected_read_case",
                format!("unexpected read case: {other:?}"),
            ),
        },
        "semantic_search" => state.heavy(req.id),
        "enable_callgraph_store_for_test" => {
            ctx.update_config(|config| config.callgraph_store = true);
            Response::success(req.id, json!({ "callgraph_store": true }))
        }
        "callers" => aft::commands::callers::handle_callers(&req, ctx),
        other => Response::error(
            req.id,
            "unexpected_command",
            format!("unexpected test command: {other}"),
        ),
    }
}

#[test]
fn subc_bridge_runs_route_calls_concurrently_and_preserves_epoch() {
    let state = Arc::new(BridgeState::default());
    let _ = BRIDGE_STATE.set(Arc::clone(&state));

    let root1 = tempfile::tempdir().expect("root1 tempdir");
    let root2 = tempfile::tempdir().expect("root2 tempdir");
    let callgraph_root = tempfile::tempdir().expect("callgraph root tempdir");
    let callgraph_src = callgraph_root.path().join("src");
    std::fs::create_dir_all(&callgraph_src).expect("callgraph src dir");
    let callgraph_file = callgraph_src.join("lib.rs");
    std::fs::write(
        &callgraph_file,
        "pub fn caller() { callee(); }\npub fn callee() {}\n",
    )
    .expect("callgraph source file");
    let storage = tempfile::tempdir().expect("storage tempdir");
    let conn_dir = tempfile::tempdir().expect("connection tempdir");
    let conn_path = conn_dir.path().join("subc-connection.json");

    let std_listener = StdTcpListener::bind("127.0.0.1:0").expect("bind fake daemon");
    std_listener
        .set_nonblocking(true)
        .expect("set fake daemon nonblocking");
    let port = std_listener.local_addr().expect("fake daemon addr").port();
    let key = vec![0x42; subc_transport::KEY_LEN];
    let daemon_id = [0x24; subc_transport::DAEMON_ID_LEN];
    let conn = ConnectionInfo {
        schema: SCHEMA_VERSION,
        endpoints: vec![Endpoint {
            host: "127.0.0.1".to_string(),
            port,
        }],
        key: key.clone(),
        daemon_id,
        pid: std::process::id(),
        daemon_ver: "subc-test".to_string(),
    };
    connection_file::write_atomic(&conn_path, &conn).expect("write connection file");

    let daemon_state = Arc::clone(&state);
    let root1_path = root1.path().to_path_buf();
    let root2_path = root2.path().to_path_buf();
    let callgraph_root_path = callgraph_root.path().to_path_buf();
    let callgraph_file_path = callgraph_file.clone();
    let daemon = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fake daemon runtime");
        runtime.block_on(async move {
            let listener = TcpListener::from_std(std_listener).expect("tokio listener");
            tokio::time::timeout(
                Duration::from_secs(10),
                drive_fake_daemon(FakeDaemonInput {
                    listener,
                    key,
                    daemon_id,
                    root1: root1_path,
                    root2: root2_path,
                    callgraph_root: callgraph_root_path,
                    callgraph_file: callgraph_file_path,
                    state: daemon_state,
                }),
            )
            .await
            .expect("subc bridge test watchdog")
        });
    });

    let ctx = Arc::new(AppContext::new(
        Box::new(TreeSitterProvider::new()),
        Config {
            storage_dir: Some(storage.path().to_path_buf()),
            ..Config::default()
        },
    ));
    let executor = Arc::new(Executor::with_config(ExecutorConfig {
        pool_size: 4,
        read_cap: 3,
        actor_cap: 3,
        heavy_permits: 2,
        drr_quantum: 1,
    }));

    run_subc_mode(&conn_path, ctx, executor, bridge_dispatch).expect("subc mode exits cleanly");
    daemon.join().expect("fake daemon joins");

    state.assert_overlap();
    state.assert_route2_configure_saw_no_reads();

    drop(root1);
    drop(root2);
    drop(callgraph_root);
    drop(storage);
}

async fn drive_fake_daemon(input: FakeDaemonInput) {
    let FakeDaemonInput {
        listener,
        key,
        daemon_id,
        root1,
        root2,
        callgraph_root,
        callgraph_file,
        state,
    } = input;
    let (mut stream, _) = listener.accept().await.expect("accept aft client");
    authenticate_server(
        &mut stream,
        &key,
        &daemon_id,
        "subc-test",
        Duration::from_secs(5),
    )
    .await
    .expect("authenticate aft client");

    let hello = read_frame_timeout(&mut stream, "ModuleHello").await;
    assert_eq!(hello.header.ty, FrameType::Hello);
    let hello_body: ModuleHelloBody = serde_json::from_slice(&hello.body).expect("hello body");
    let _: ModuleManifest = hello_body.manifest;
    send_frame(
        &mut stream,
        Frame::build(
            FrameType::HelloAck,
            control_flags(),
            0,
            hello.header.corr,
            serde_json::to_vec(&ModuleHelloAckBody {
                negotiated_ver: PROTOCOL_VERSION,
                subc_ops: Vec::new(),
                subc_capabilities: Vec::new(),
            })
            .expect("hello ack body"),
        )
        .expect("hello ack frame"),
    )
    .await;

    send_route_bind(&mut stream, 1, 10, &root1).await;
    expect_route_bind_ack(&mut stream, 10).await;

    // 1. Overlap: three PureRead calls on one route must all reach dispatch
    // before any is released.
    for corr in 100..103 {
        send_tool_call(&mut stream, 1, corr, "read", json!({ "case": "overlap" })).await;
    }
    state.wait_until("overlap reads started", |inner| inner.overlap_started == 3);
    state.assert_overlap();
    state.release_overlap();
    let overlap_corrs = collect_response_corrs(&mut stream, 3).await;
    assert_eq!(overlap_corrs, HashSet::from([100, 101, 102]));

    // 2. Slow HeavyInit + fast PureRead: a read submitted after the heavy job has
    // started returns before the heavy response.
    send_tool_call(
        &mut stream,
        1,
        200,
        "semantic_search",
        json!({ "case": "heavy" }),
    )
    .await;
    state.wait_until("heavy started", |inner| inner.heavy_started);
    send_tool_call(&mut stream, 1, 201, "read", json!({ "case": "fast" })).await;
    let fast = read_frame_timeout(&mut stream, "fast read response").await;
    assert_eq!(fast.header.ty, FrameType::Response);
    assert_eq!(fast.header.channel, 1);
    assert_eq!(
        fast.header.corr, 201,
        "fast read should beat heavy response"
    );
    assert_eq!(tool_response_json(&fast)["id"], "subc-1-201");
    state.release_heavy();
    let heavy = read_frame_timeout(&mut stream, "heavy response").await;
    assert_eq!(heavy.header.corr, 200);

    // 4. Epoch safety + Blocker 1: route 2 binds a different root while route 1
    // reads are in flight. The configure job must collapse onto the single actor
    // and wait for the first route's reads to drain.
    for corr in 300..302 {
        send_tool_call(&mut stream, 1, corr, "read", json!({ "case": "epoch" })).await;
    }
    state.wait_until("epoch reads started", |inner| inner.epoch_started == 2);
    send_route_bind(&mut stream, 2, 30, &root2).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    state.assert_route2_configure_not_started();
    state.release_epoch_reads();
    let epoch_corrs = collect_response_corrs(&mut stream, 2).await;
    assert_eq!(epoch_corrs, HashSet::from([300, 301]));
    expect_route_bind_ack(&mut stream, 30).await;
    state.assert_route2_configure_saw_no_reads();

    send_tool_call(&mut stream, 2, 400, "read", json!({ "case": "fast" })).await;
    let route2_read = read_frame_timeout(&mut stream, "route 2 read response").await;
    assert_eq!(route2_read.header.channel, 2);
    assert_eq!(route2_read.header.corr, 400);

    // 3 + 5. Bind a real source project with callgraph_store disabled so bind-time
    // configure cannot spawn/install the store. Enable the store under the executor,
    // then concurrent callers trigger exactly one cold build. A later identical
    // callers request succeeds only after the maintenance tick drains that build.
    send_route_bind(&mut stream, 3, 40, &callgraph_root).await;
    expect_route_bind_ack(&mut stream, 40).await;
    send_tool_call(
        &mut stream,
        3,
        500,
        "enable_callgraph_store_for_test",
        json!({}),
    )
    .await;
    let enabled = read_frame_timeout(&mut stream, "enable callgraph response").await;
    assert_eq!(enabled.header.corr, 500);
    assert!(tool_response_json(&enabled)["success"]
        .as_bool()
        .unwrap_or(false));

    aft::context::reset_callgraph_cold_build_spawn_count_for_test();
    let callers_args = json!({
        "file": callgraph_file.to_string_lossy(),
        "symbol": "callee",
        "depth": 1,
    });
    send_tool_call(&mut stream, 3, 501, "callers", callers_args.clone()).await;
    send_tool_call(&mut stream, 3, 502, "callers", callers_args.clone()).await;
    let cold_one = read_frame_timeout(&mut stream, "cold callers response 1").await;
    let cold_two = read_frame_timeout(&mut stream, "cold callers response 2").await;
    let cold_responses = [tool_response_json(&cold_one), tool_response_json(&cold_two)];
    assert_eq!(
        aft::context::callgraph_cold_build_spawn_count_for_test(),
        1,
        "concurrent cold callers through subc must share one background build"
    );
    assert!(
        cold_responses.iter().any(is_callgraph_building),
        "at least one cold callers response should report callgraph_building: {cold_responses:?}"
    );

    let ready = poll_callers_until_ready(&mut stream, 3, 510, callers_args).await;
    assert!(ready["success"].as_bool().unwrap_or(false));
    assert_ne!(
        ready.get("code").and_then(Value::as_str),
        Some("callgraph_building")
    );
    assert!(
        ready["total_callers"].as_u64().unwrap_or(0) >= 1,
        "ready callers response should contain the built result: {ready:?}"
    );

    send_frame(
        &mut stream,
        Frame::build(FrameType::Goodbye, control_flags(), 0, 99, Vec::new())
            .expect("goodbye frame"),
    )
    .await;
}

async fn send_route_bind(
    stream: &mut tokio::net::TcpStream,
    route_channel: u16,
    corr: u64,
    root: &std::path::Path,
) {
    let request = ModuleControlRequest::RouteBind {
        route_channel,
        target: RouteTarget::ToolProvider {
            module_id: "aft".to_string(),
        },
        identity: BindIdentity {
            project_root: root.to_path_buf(),
            harness: "opencode".to_string(),
            session: format!("session-{route_channel}"),
        },
        config: vec![user_config_tier(json!({
            "callgraph_store": false,
            "search_index": false,
            "semantic_search": false,
        }))],
    };
    send_frame(
        stream,
        Frame::build(
            FrameType::Request,
            control_flags(),
            0,
            corr,
            serde_json::to_vec(&request).expect("route bind body"),
        )
        .expect("route bind frame"),
    )
    .await;
}

async fn send_tool_call(
    stream: &mut tokio::net::TcpStream,
    channel: u16,
    corr: u64,
    name: &str,
    arguments: Value,
) {
    let body = json!({ "name": name, "arguments": arguments });
    send_frame(
        stream,
        Frame::build(
            FrameType::Request,
            Flags::new(false, Priority::Interactive, false),
            channel,
            corr,
            serde_json::to_vec(&body).expect("tool call body"),
        )
        .expect("tool call frame"),
    )
    .await;
}

async fn send_frame(stream: &mut tokio::net::TcpStream, frame: Frame) {
    write_frame(stream, &frame).await.expect("write frame");
}

async fn read_frame_timeout(stream: &mut tokio::net::TcpStream, label: &str) -> Frame {
    tokio::time::timeout(Duration::from_secs(3), read_frame(stream))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {label}"))
        .expect("read frame")
        .unwrap_or_else(|| panic!("EOF waiting for {label}"))
}

async fn expect_route_bind_ack(stream: &mut tokio::net::TcpStream, corr: u64) {
    let frame = read_frame_timeout(stream, "RouteBindAck").await;
    assert_eq!(frame.header.ty, FrameType::Response);
    assert_eq!(frame.header.channel, 0);
    assert_eq!(frame.header.corr, corr);
    let ack: ModuleControlResponse = serde_json::from_slice(&frame.body).expect("ack body");
    assert_eq!(ack, ModuleControlResponse::RouteBindAck {});
}

async fn collect_response_corrs(stream: &mut tokio::net::TcpStream, count: usize) -> HashSet<u64> {
    let mut corrs = HashSet::new();
    for _ in 0..count {
        let frame = read_frame_timeout(stream, "tool response").await;
        assert_eq!(frame.header.ty, FrameType::Response);
        assert!(tool_response_json(&frame)["success"]
            .as_bool()
            .unwrap_or(false));
        corrs.insert(frame.header.corr);
    }
    corrs
}

fn tool_response_json(frame: &Frame) -> Value {
    let body: Value = serde_json::from_slice(&frame.body).expect("tool result body");
    let text = body["content"][0]["text"]
        .as_str()
        .expect("tool result text");
    serde_json::from_str(text).expect("embedded AFT response JSON")
}

async fn poll_callers_until_ready(
    stream: &mut tokio::net::TcpStream,
    channel: u16,
    first_corr: u64,
    arguments: Value,
) -> Value {
    for attempt in 0..30 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let corr = first_corr + attempt;
        send_tool_call(stream, channel, corr, "callers", arguments.clone()).await;
        let frame = read_frame_timeout(stream, "poll callers response").await;
        assert_eq!(frame.header.corr, corr);
        let response = tool_response_json(&frame);
        if response["success"].as_bool() == Some(true) {
            return response;
        }
        assert_eq!(
            response.get("code").and_then(Value::as_str),
            Some("callgraph_building"),
            "callers should build or become ready, got {response:?}"
        );
    }
    panic!("callgraph store did not become ready after maintenance drain ticks");
}

fn is_callgraph_building(response: &Value) -> bool {
    response.get("code").and_then(Value::as_str) == Some("callgraph_building")
}

fn user_config_tier(doc: Value) -> subc_protocol::session::ConfigTier {
    subc_protocol::session::ConfigTier {
        tier: "user".to_string(),
        source: "/tmp/aft-subc-bridge-test-user.jsonc".to_string(),
        doc: doc.to_string(),
    }
}

fn control_flags() -> Flags {
    Flags::new(false, Priority::Passive, false)
}
