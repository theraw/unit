use anyhow::{bail, Context, Result};
use bytes::{Bytes, BytesMut};
use http::header::{HeaderName, HeaderValue};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::Error;
use std::ffi::{CStr, CString};
use std::mem::MaybeUninit;
use std::process::exit;
use std::ptr;
use std::sync::OnceLock;
use tokio::sync::mpsc;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::p2::{
    add_to_linker_async, IoView, WasiCtx, WasiCtxBuilder, WasiView,
};
use wasmtime_wasi::{DirPerms, FilePerms};
use wasmtime_wasi_http::bindings::http::types::{ErrorCode, Scheme};
use wasmtime_wasi_http::bindings::ProxyPre;
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpView};

#[allow(
    non_camel_case_types,
    non_upper_case_globals,
    non_snake_case,
    dead_code,
    unknown_lints,
    unnecessary_transmutes,
    clippy::all
)]
mod bindings {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

    pub const fn nxt_string(s: &'static str) -> nxt_str_t {
        nxt_str_t {
            start: s.as_ptr().cast_mut(),
            length: s.len(),
        }
    }

    pub unsafe fn nxt_unit_sptr_get(sptr: &nxt_unit_sptr_t) -> *const u8 {
        sptr.base.as_ptr().offset(sptr.offset as isize)
    }
}

#[no_mangle]
pub static mut nxt_app_module: bindings::nxt_app_module_t = {
    const COMPAT: [u32; 2] = [bindings::NXT_VERNUM, bindings::NXT_DEBUG];
    let version = "0.1\0";
    bindings::nxt_app_module_t {
        compat: COMPAT.as_ptr().cast_mut(),
        compat_length: COMPAT.len() * 4,
        mounts: ptr::null(),
        nmounts: 0,
        type_: bindings::nxt_string("wasm-wasi-component"),
        version: version.as_ptr().cast(),
        setup: Some(setup),
        start: Some(start),
    }
};

static GLOBAL_CONFIG: OnceLock<GlobalConfig> = OnceLock::new();
static GLOBAL_STATE: OnceLock<GlobalState> = OnceLock::new();

unsafe extern "C" fn setup(
    task: *mut bindings::nxt_task_t,
    // TODO: should this get used?
    _process: *mut bindings::nxt_process_t,
    conf: *mut bindings::nxt_common_app_conf_t,
) -> bindings::nxt_int_t {
    handle_result(task, || {
        let wasm_conf = &(*conf).u.wasm_wc;
        let component = CStr::from_ptr(wasm_conf.component).to_str()?;
        let mut dirs = Vec::new();
        if !wasm_conf.access.is_null() {
            let dirs_ptr = bindings::nxt_conf_get_object_member(
                wasm_conf.access,
                &bindings::nxt_string("filesystem"),
                ptr::null_mut(),
            );
            if !dirs_ptr.is_null() {
                for i in 0..bindings::nxt_conf_object_members_count(dirs_ptr) {
                    let value =
                        bindings::nxt_conf_get_array_element(dirs_ptr, i);
                    let mut s = bindings::nxt_string("");
                    bindings::nxt_conf_get_string(value, &mut s);
                    dirs.push(
                        std::str::from_utf8(std::slice::from_raw_parts(
                            s.start, s.length,
                        ))?
                        .to_string(),
                    );
                }
            }
        }

        let result = GLOBAL_CONFIG.set(GlobalConfig {
            component: component.to_string(),
            dirs,
        });
        assert!(result.is_ok());
        Ok(())
    })
}

unsafe extern "C" fn start(
    task: *mut bindings::nxt_task_t,
    data: *mut bindings::nxt_process_data_t,
) -> bindings::nxt_int_t {
    let mut rc: i32 = 0;

    let result = handle_result(task, || {
        let config = GLOBAL_CONFIG.get().unwrap();
        let state = GlobalState::new(config)
            .context("failed to create initial state")?;
        let res = GLOBAL_STATE.set(state);
        assert!(res.is_ok());

        let conf = (*data).app;
        let mut wasm_init = MaybeUninit::uninit();
        let ret =
            bindings::nxt_unit_default_init(task, wasm_init.as_mut_ptr(), conf);
        if ret != bindings::NXT_OK as bindings::nxt_int_t {
            bail!("nxt_unit_default_init() failed");
        }
        let mut wasm_init = wasm_init.assume_init();
        wasm_init.callbacks.request_handler = Some(request_handler);

        let unit_ctx = bindings::nxt_unit_init(&mut wasm_init);
        if unit_ctx.is_null() {
            bail!("nxt_unit_init() failed");
        }

        rc = bindings::nxt_unit_run(unit_ctx);
        bindings::nxt_unit_done(unit_ctx);

        Ok(())
    });

    if result != bindings::NXT_OK as bindings::nxt_int_t {
        return result;
    }

    exit(rc);
}

unsafe fn handle_result(
    task: *mut bindings::nxt_task_t,
    func: impl FnOnce() -> Result<()>,
) -> bindings::nxt_int_t {
    let rc = match func() {
        Ok(()) => bindings::NXT_OK as bindings::nxt_int_t,
        Err(e) => {
            alert(task, &format!("{e:?}"));
            bindings::NXT_ERROR as bindings::nxt_int_t
        }
    };
    return rc;

    unsafe fn alert(task: *mut bindings::nxt_task_t, msg: &str) {
        let log = (*task).log;
        let msg = CString::new(msg).unwrap();
        ((*log).handler).unwrap()(
            bindings::NXT_LOG_ALERT as bindings::nxt_uint_t,
            log,
            c"%s".as_ptr().cast(),
            msg.as_ptr(),
        );
    }
}

unsafe extern "C" fn request_handler(
    info: *mut bindings::nxt_unit_request_info_t,
) {
    // Enqueue this request to get processed by the Tokio event loop, and
    // otherwise immediately return.
    let state = GLOBAL_STATE.get().unwrap();
    state.sender.blocking_send(NxtRequestInfo { info }).unwrap();
}

struct GlobalConfig {
    component: String,
    dirs: Vec<String>,
}

struct GlobalState {
    engine: Engine,
    component: ProxyPre<StoreState>,
    global_config: &'static GlobalConfig,
    sender: mpsc::Sender<NxtRequestInfo>,
}

impl GlobalState {
    fn new(global_config: &'static GlobalConfig) -> Result<GlobalState> {
        // Configure Wasmtime, e.g. the component model and async support are
        // enabled here. Other configuration can include:
        //
        // * Epochs/fuel - enables async yielding to prevent any one request
        //   starving others.
        // * Pooling allocator - accelerates instantiation at the cost of a
        //   large virtual memory reservation.
        // * Memory limits/etc.
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.async_support(true);
        let engine = Engine::new(&config)?;

        // Compile the binary component on disk in Wasmtime. This is then
        // pre-instantiated with host APIs defined by WASI. The result of
        // this is a "pre-instantiated instance" which can be used to
        // repeatedly instantiate later on. This will frontload
        // compilation/linking/type-checking/etc to happen once rather than on
        // each request.
        let component = Component::from_file(&engine, &global_config.component)
            .context("failed to compile component")?;
        let mut linker = Linker::<StoreState>::new(&engine);
        add_to_linker_async(&mut linker)
            .context("failed to add wasi to linker")?;
        wasmtime_wasi_http::add_only_http_to_linker_sync(&mut linker)
            .context("failed to add wasi:http to linker")?;
        let component = linker
            .instantiate_pre(&component)
            .context("failed to pre-instantiate the provided component")?;
        let proxy =
            ProxyPre::new(component).context("failed to conform to proxy")?;

        // Spin up the Tokio async runtime in a separate thread with a
        // communication channel into it. This thread will send requests to
        // Tokio and the results will be calculated there.
        let (sender, receiver) = mpsc::channel(10);
        std::thread::spawn(|| GlobalState::run(receiver));

        Ok(GlobalState {
            engine,
            component: proxy,
            sender,
            global_config,
        })
    }

    /// Worker thread that executes the Tokio runtime, infinitely receiving
    /// messages from the provided `receiver` and handling those requests.
    ///
    /// Each request is handled in a separate subtask so processing can all
    /// happen concurrently.
    fn run(mut receiver: mpsc::Receiver<NxtRequestInfo>) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            while let Some(msg) = receiver.recv().await {
                let state = GLOBAL_STATE.get().unwrap();
                tokio::task::spawn(async move {
                    if let Err(e) = state.handle(msg).await {
                        eprintln!("failed to handle request: {e:?}");
                    }
                });
            }
        });
    }

    async fn handle(&'static self, mut info: NxtRequestInfo) -> Result<()> {
        let result = self.handle_impl(&mut info).await;
        info.request_done();
        result
    }

    async fn handle_impl(
        &'static self,
        info: &mut NxtRequestInfo,
    ) -> Result<()> {
        // Create a "Store" which is the unit of per-request isolation in
        // Wasmtime.
        let data = StoreState {
            ctx: {
                let mut cx = WasiCtxBuilder::new();
                // NB: while useful for debugging untrusted code probably
                // shouldn't get raw access to stdout/stderr.
                cx.inherit_stdout();
                cx.inherit_stderr();
                cx.inherit_env();
                for dir in self.global_config.dirs.iter() {
                    cx.preopened_dir(
                        dir,
                        dir,
                        DirPerms::all(),
                        FilePerms::all(),
                    )?;
                }
                cx.build()
            },
            table: ResourceTable::default(),
            http: WasiHttpCtx::new(),
        };
        let mut store = Store::new(&self.engine, data);

        // Convert the `nxt_*` representation into the representation required
        // by Wasmtime's `wasi-http` implementation using the Rust `http`
        // crate.
        let request = self.to_request_builder(info)?;
        let body = self.to_request_body(info)?;
        let request = request.body(body)?;

        let (sender, receiver) = tokio::sync::oneshot::channel();

        // Instantiate the WebAssembly component and invoke its `handle`
        // function which receives a request and where to put a response.
        //
        // Note that this is done in a sub-task to work concurrently with
        // writing the response when it's available. This enables wasm to
        // generate headers, write those below, and then compute the body
        // afterwards.
        let task = tokio::spawn(async move {
            let req = store
                .data_mut()
                .new_incoming_request(Scheme::Http, request)?;
            let out = store.data_mut().new_response_outparam(sender)?;
            self.component
                .instantiate_async(&mut store)
                .await?
                .wasi_http_incoming_handler()
                .call_handle(&mut store, req, out)
                .await
                .context("failed to invoke wasm `handle`")?;
            Ok::<_, anyhow::Error>(())
        });

        // Wait for the wasm to produce the initial response. If this succeeds
        // then propagate that failure. If this fails then wait for the above
        // task to complete to see if it failed, otherwise panic since that's
        // unexpected.
        let response = match receiver.await {
            Ok(response) => response.context("response generation failed")?,
            Err(_) => {
                task.await.context("wasm task failed to join")??;
                bail!("sender of response disappeared");
            }
        };

        // Send the headers/status which will extract the body for the next
        // phase.
        let body = self.send_response(info, response)?;

        // Send the body, a blocking operation, over time as it becomes
        // available.
        self.send_response_body(info, body)
            .await
            .context("failed to write response body")?;

        // Join on completion of the wasm task which should be done by this
        // point.
        task.await.context("wasm task failed to join")??;

        Ok(())
    }

    fn to_request_builder(
        &self,
        info: &NxtRequestInfo,
    ) -> Result<http::request::Builder> {
        let mut request = http::Request::builder();

        request = request.method(info.method()?);
        request = match info.version()? {
            "HTTP/0.9" => request.version(http::Version::HTTP_09),
            "HTTP/1.0" => request.version(http::Version::HTTP_10),
            "HTTP/1.1" => request.version(http::Version::HTTP_11),
            "HTTP/2.0" => request.version(http::Version::HTTP_2),
            "HTTP/3.0" => request.version(http::Version::HTTP_3),
            version => {
                println!("unknown version: {version}");
                request
            }
        };

        let uri = http::Uri::builder()
            .scheme(if info.tls() { "https" } else { "http" })
            .authority(info.server_name()?)
            .path_and_query(info.target()?)
            .build()
            .context("failed to build URI")?;
        request = request.uri(uri);

        for (name, value) in info.fields()? {
            let name = HeaderName::from_bytes(name)
                .context("invalid request header name")?;
            let value = HeaderValue::from_bytes(value)
                .context("invalid request header value")?;
            request = request.header(name, value);
        }
        Ok(request)
    }

    fn to_request_body(
        &self,
        info: &mut NxtRequestInfo,
    ) -> Result<BoxBody<Bytes, Error>> {
        // TODO: should convert the body into a form of `Stream` to become an
        // async stream of frames. The return value can represent that here
        // but for now this slurps up the entire body into memory and puts it
        // all in a single `BytesMut` which is then converted to `Bytes`.
        let content_length = info
            .content_length()
            .try_into()
            .context("request body is too large")?;
        let mut body = BytesMut::with_capacity(content_length);

        // TODO: how to make this async at the nxt level?
        info.request_read(&mut body)?;

        Ok(Full::new(body.freeze()).map_err(|e| match e {}).boxed())
    }

    fn send_response<T>(
        &self,
        info: &mut NxtRequestInfo,
        response: http::Response<T>,
    ) -> Result<T> {
        let headers = response
            .headers()
            .len()
            .try_into()
            .context("too many response headers")?;
        let headers_size = response
            .headers()
            .iter()
            .map(|(k, v)| k.as_str().len() + v.len())
            .sum::<usize>()
            .try_into()
            .context("response headers are too large")?;

        info.init_response(response.status().as_u16(), headers, headers_size)?;
        for (k, v) in response.headers() {
            info.add_field(k.as_str().as_bytes(), v.as_bytes())?;
        }
        info.send_response()?;

        Ok(response.into_body())
    }

    async fn send_response_body(
        &self,
        info: &mut NxtRequestInfo,
        mut body: BoxBody<Bytes, ErrorCode>,
    ) -> Result<()> {
        loop {
            // Acquire the next frame, and because nothing is actually async
            // at the moment this should never block meaning that the
            // `Pending` case should not happen.
            let frame = match body.frame().await {
                Some(Ok(frame)) => frame,
                Some(Err(e)) => break Err(e.into()),
                None => break Ok(()),
            };
            match frame.data_ref() {
                Some(data) => {
                    info.response_write(data)?;
                }
                None => {
                    // TODO: what to do with trailers?
                }
            }
        }
    }
}

struct NxtRequestInfo {
    info: *mut bindings::nxt_unit_request_info_t,
}

// TODO: is this actually safe?
unsafe impl Send for NxtRequestInfo {}
unsafe impl Sync for NxtRequestInfo {}

impl NxtRequestInfo {
    fn method(&self) -> Result<&str> {
        unsafe {
            let raw = (*self.info).request;
            self.get_str(&(*raw).method, (*raw).method_length.into())
        }
    }

    fn tls(&self) -> bool {
        unsafe { (*(*self.info).request).tls != 0 }
    }

    fn version(&self) -> Result<&str> {
        unsafe {
            let raw = (*self.info).request;
            self.get_str(&(*raw).version, (*raw).version_length.into())
        }
    }

    fn server_name(&self) -> Result<&str> {
        unsafe {
            let raw = (*self.info).request;
            self.get_str(&(*raw).server_name, (*raw).server_name_length)
        }
    }

    fn target(&self) -> Result<&str> {
        unsafe {
            let raw = (*self.info).request;
            self.get_str(&(*raw).target, (*raw).target_length)
        }
    }

    fn content_length(&self) -> u64 {
        unsafe {
            let raw_request = (*self.info).request;
            (*raw_request).content_length
        }
    }

    fn fields(&self) -> Result<Vec<(&[u8], &[u8])>> {
        unsafe {
            let raw = (*self.info).request;
            let count = (*raw).fields_count as usize;
            let mut fields = Vec::with_capacity(count);

            for i in 0..count {
                let field = (*raw).fields.as_ptr().add(i);
                let name = self
                    .get_slice(&(*field).name, (*field).name_length.into())?;
                let value =
                    self.get_slice(&(*field).value, (*field).value_length)?;
                fields.push((name, value));
            }

            Ok(fields)
        }
    }

    fn request_read(&mut self, dst: &mut BytesMut) -> Result<()> {
        const MAX_READ_SIZE: usize = 32 * 1024 * 1024;

        let total_bytes_read = unsafe {
            let rest = dst.spare_capacity_mut();
            let mut total_bytes_read = 0usize;

            while total_bytes_read < rest.len() {
                let remaining = rest.len() - total_bytes_read;
                let read_size = remaining.min(MAX_READ_SIZE);
                let amt = bindings::nxt_unit_request_read(
                    self.info,
                    rest.as_mut_ptr().wrapping_add(total_bytes_read).cast(),
                    read_size,
                );

                if amt < 0 {
                    bail!("nxt_unit_request_read() failed: {amt}");
                }

                let amt = amt as usize;
                if amt == 0 {
                    bail!("nxt_unit_request_read() returned unexpected EOF");
                }
                if amt > remaining {
                    bail!(
                        "nxt_unit_request_read() returned {amt} bytes, but only \
                         {remaining} bytes were requested"
                    );
                }

                total_bytes_read += amt;
            }

            total_bytes_read
        };

        unsafe {
            dst.set_len(dst.len() + total_bytes_read);
        }

        Ok(())
    }

    fn response_write(&mut self, data: &[u8]) -> Result<()> {
        unsafe {
            let rc = bindings::nxt_unit_response_write(
                self.info,
                data.as_ptr().cast(),
                data.len(),
            );
            if rc != 0 {
                bail!("nxt_unit_response_write() failed: {rc}");
            }
        }

        Ok(())
    }

    fn init_response(
        &mut self,
        status: u16,
        headers: u32,
        headers_size: u32,
    ) -> Result<()> {
        unsafe {
            let rc = bindings::nxt_unit_response_init(
                self.info,
                status,
                headers,
                headers_size,
            );
            if rc != 0 {
                bail!("nxt_unit_response_init() failed: {rc}");
            }
        }

        Ok(())
    }

    fn add_field(&mut self, key: &[u8], val: &[u8]) -> Result<()> {
        let key_len =
            key.len().try_into().context("header name is too long")?;
        let val_len =
            val.len().try_into().context("header value is too long")?;

        unsafe {
            let rc = bindings::nxt_unit_response_add_field(
                self.info,
                key.as_ptr().cast(),
                key_len,
                val.as_ptr().cast(),
                val_len,
            );
            if rc != 0 {
                bail!("nxt_unit_response_add_field() failed: {rc}");
            }
        }

        Ok(())
    }

    fn send_response(&mut self) -> Result<()> {
        unsafe {
            let rc = bindings::nxt_unit_response_send(self.info);
            if rc != 0 {
                bail!("nxt_unit_response_send() failed: {rc}");
            }
        }

        Ok(())
    }

    fn request_done(self) {
        unsafe {
            bindings::nxt_unit_request_done(
                self.info,
                bindings::NXT_UNIT_OK as i32,
            );
        }
    }

    fn get_slice(
        &self,
        ptr: &bindings::nxt_unit_sptr_t,
        len: u32,
    ) -> Result<&[u8]> {
        let len: usize = len.try_into().context("string is too long")?;

        unsafe {
            let ptr = bindings::nxt_unit_sptr_get(ptr);
            Ok(std::slice::from_raw_parts(ptr, len))
        }
    }

    fn get_str(
        &self,
        ptr: &bindings::nxt_unit_sptr_t,
        len: u32,
    ) -> Result<&str> {
        std::str::from_utf8(self.get_slice(ptr, len)?)
            .context("request string is not valid UTF-8")
    }
}

struct StoreState {
    ctx: WasiCtx,
    http: WasiHttpCtx,
    table: ResourceTable,
}

impl IoView for StoreState {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}

impl WasiView for StoreState {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.ctx
    }
}

impl WasiHttpView for StoreState {
    fn ctx(&mut self) -> &mut WasiHttpCtx {
        &mut self.http
    }
}

impl StoreState {}
