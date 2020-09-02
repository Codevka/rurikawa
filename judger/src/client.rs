pub mod model;

use crate::{
    client::model::JobResultKind,
    config::{JudgeToml, JudgerPublicConfig},
    fs,
    prelude::*,
    tester::exec::JudgerPrivateConfig,
};
use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use futures::{
    stream::{SplitSink, SplitStream},
    FutureExt, Sink, SinkExt, Stream, StreamExt,
};
use http::Uri;
use model::*;
use serde::{Deserialize, Serialize};
use serde_json::from_slice;
use std::{collections::HashMap, error::Error, path::PathBuf, sync::Arc};
use tokio::{net::TcpStream, sync::Mutex};
use tokio_tungstenite::{connect_async, tungstenite, MaybeTlsStream, WebSocketStream};
use tungstenite::Message;

pub type WsDuplex = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub type WsSink = SplitSink<WsDuplex, Message>;
pub type WsStream = SplitStream<WsDuplex>;

#[async_trait]
pub trait SendJsonMessage<M, T>
where
    T: Sink<Message> + Unpin + Send + Sync,
    M: Serialize,
{
    type Error;
    async fn send_msg(&mut self, msg: &M) -> Result<(), Self::Error>;
}

#[async_trait]
impl<M, T> SendJsonMessage<M, T> for T
where
    T: Sink<Message> + Unpin + Send + Sync,
    M: Serialize + Sync,
{
    type Error = T::Error;
    async fn send_msg(&mut self, msg: &M) -> Result<(), Self::Error> {
        let serialized = serde_json::to_string(msg).unwrap();
        let msg = Message::text(serialized);
        self.send(msg).await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    pub host: String,
    pub token: Option<String>,
    pub cache_folder: PathBuf,
}

#[derive(Debug)]
pub struct SharedClientData {
    pub cfg: ClientConfig,
    /// All test suites whose folder is being edited.
    ///
    ///
    pub locked_test_suite: DashMap<FlowSnake, Arc<Mutex<()>>>,
}

impl SharedClientData {
    pub fn new(cfg: ClientConfig) -> SharedClientData {
        SharedClientData {
            cfg,
            locked_test_suite: DashMap::new(),
        }
    }

    pub fn websocket_endpoint(&self) -> String {
        format!("{}/api/v1/judger/ws", self.cfg.host)
    }

    pub fn test_suite_download_endpoint(&self, suite_id: FlowSnake) -> String {
        format!("{}/api/v1/test_suite/{}", self.cfg.host, suite_id)
    }

    pub fn job_folder_root(&self) -> PathBuf {
        let mut job_temp_folder = self.cfg.cache_folder.clone();
        job_temp_folder.push("jobs");
        job_temp_folder
    }

    pub fn test_suite_folder_root(&self) -> PathBuf {
        let mut test_suite_temp_folder = self.cfg.cache_folder.clone();
        test_suite_temp_folder.push("suites");
        test_suite_temp_folder
    }

    pub fn job_folder(&self, job_id: FlowSnake) -> PathBuf {
        let mut job_temp_folder = self.job_folder_root();
        job_temp_folder.push(job_id.to_string());
        job_temp_folder
    }

    pub fn test_suite_folder(&self, suite_id: FlowSnake) -> PathBuf {
        let mut test_suite_temp_folder = self.test_suite_folder_root();
        test_suite_temp_folder.push(suite_id.to_string());
        test_suite_temp_folder
    }

    pub fn temp_file_folder_root(&self) -> PathBuf {
        let mut test_suite_temp_folder = self.cfg.cache_folder.clone();
        test_suite_temp_folder.push("files");
        test_suite_temp_folder
    }

    pub fn obtain_suite_lock(&self, suite_id: FlowSnake) -> Arc<Mutex<()>> {
        let cur = self
            .locked_test_suite
            .get(&suite_id)
            .map(|pair| pair.value().clone());
        if let Some(cur) = cur {
            cur
        } else {
            let arc = Arc::new(Mutex::new(()));
            self.locked_test_suite.insert(suite_id, arc.clone());
            arc
        }
    }

    pub fn suite_unlock(&self, suite_id: FlowSnake) {
        self.locked_test_suite.remove(&suite_id);
    }
}

#[derive(Debug)]
pub enum ClientErr {
    NoSuchFile(String),
    Io(std::io::Error),
    Ws(tungstenite::Error),
    Json(serde_json::Error),
    TomlDes(toml::de::Error),
    Exec(crate::tester::ExecError),
    Any(anyhow::Error),
}

impl From<std::io::Error> for ClientErr {
    fn from(e: std::io::Error) -> Self {
        ClientErr::Io(e)
    }
}

impl From<anyhow::Error> for ClientErr {
    fn from(e: anyhow::Error) -> Self {
        ClientErr::Any(e)
    }
}

impl From<crate::tester::ExecError> for ClientErr {
    fn from(e: crate::tester::ExecError) -> Self {
        ClientErr::Exec(e)
    }
}

impl From<serde_json::Error> for ClientErr {
    fn from(e: serde_json::Error) -> Self {
        ClientErr::Json(e)
    }
}

impl From<tungstenite::error::Error> for ClientErr {
    fn from(e: tungstenite::error::Error) -> Self {
        match e {
            tungstenite::Error::Io(e) => ClientErr::Io(e),
            _ => ClientErr::Ws(e),
        }
    }
}

impl From<toml::de::Error> for ClientErr {
    fn from(e: toml::de::Error) -> Self {
        ClientErr::TomlDes(e)
    }
}

pub async fn connect_to_coordinator(
    cfg: &SharedClientData,
) -> Result<(WsSink, WsStream), tungstenite::Error> {
    let endpoint = cfg.websocket_endpoint();
    let mut req = http::Request::builder().uri(&endpoint);
    if let Some(token) = cfg.cfg.token.as_ref() {
        req = req.header("Authorization", format!("Bearer {}", token));
    } else {
        req = req.header("Authorization", "");
    }
    log::info!("Connecting to {}", endpoint);
    let (client, _) = connect_async(req.body(()).unwrap()).await?;
    let (cli_sink, cli_stream) = client.split();
    log::info!("Connection success");
    Ok((cli_sink, cli_stream))
}

pub async fn check_download_read_test_suite(
    suite_id: FlowSnake,
    cfg: &SharedClientData,
) -> Result<JudgerPublicConfig, ClientErr> {
    let endpoint = cfg.test_suite_download_endpoint(suite_id);

    let suite_folder_root = cfg.test_suite_folder_root();
    tokio::fs::create_dir_all(suite_folder_root).await?;
    let suite_folder = cfg.test_suite_folder(suite_id);
    {
        // Lock this specific test suite and let all other concurrent tasks to wait
        // until downloading completes
        let lock = cfg.obtain_suite_lock(suite_id);
        lock.lock().await;

        let dir_exists = {
            let create_dir = tokio::fs::create_dir(&suite_folder).await;
            match create_dir {
                Ok(()) => false,
                Err(e) => match e.kind() {
                    std::io::ErrorKind::AlreadyExists => true,
                    _ => return Err(e.into()),
                },
            }
        };
        if !dir_exists {
            fs::net::download_unzip(&endpoint, &suite_folder, &cfg.temp_file_folder_root()).await?;
        }
        cfg.suite_unlock(suite_id);
    }

    let mut judger_conf_dir = suite_folder.clone();
    judger_conf_dir.push("testconf.json");
    let judger_conf = match tokio::fs::read(&judger_conf_dir).await {
        Ok(c) => c,
        Err(e) => match e.kind() {
            std::io::ErrorKind::NotFound => {
                return Err(ClientErr::NoSuchFile(
                    judger_conf_dir.to_string_lossy().to_owned().to_string(),
                ));
            }
            _ => return Err(ClientErr::Io(e)),
        },
    };
    let judger_conf = serde_json::from_slice::<JudgerPublicConfig>(&judger_conf)?;

    Ok(judger_conf)
}

pub async fn handle_job_wrapper(job: NewJob, send: Arc<Mutex<WsSink>>, cfg: Arc<SharedClientData>) {
    // TODO: Handle failed cases and report
    let job_id = job.job.id;
    match handle_job(job, send.clone(), cfg).await {
        Ok(_res) => {
            let send_res = send
                .lock()
                .await
                .send_msg(&ClientMsg::JobResult(_res))
                .await;
            match send_res {
                Ok(_) => {}
                Err(e) => log::error!("Error when sending job result mesage:\n{}", e),
            }
        }
        Err(_err) => {
            let (err, msg) = match _err {
                ClientErr::NoSuchFile(f) => (
                    JobResultKind::CompileError,
                    format!("Cannot find file: {}", f),
                ),
                ClientErr::Io(e) => (JobResultKind::JudgerError, format!("IO error: {:?}", e)),
                ClientErr::Ws(e) => (
                    JobResultKind::JudgerError,
                    format!("Websocket error: {:?}", e),
                ),
                ClientErr::Json(e) => (JobResultKind::JudgerError, format!("JSON error: {:?}", e)),
                ClientErr::TomlDes(e) => (
                    JobResultKind::JudgerError,
                    format!("TOML deserialization error: {:?}", e),
                ),
                ClientErr::Exec(e) => (JobResultKind::PipelineError, format!("{:?}", e)),
                ClientErr::Any(e) => (JobResultKind::OtherError, format!("{:?}", e)),
            };
            let send_res = send
                .lock()
                .await
                .send_msg(&ClientMsg::JobResult(JobResultMsg {
                    job_id,
                    test_results: HashMap::new(),
                    job_result: err,
                    message: Some(msg),
                }))
                .await;
            match send_res {
                Ok(_) => {}
                Err(e) => log::error!("Error when sending job result mesage:\n{}", e),
            }
        }
    }
}

pub async fn handle_job(
    job: NewJob,
    send: Arc<Mutex<WsSink>>,
    cfg: Arc<SharedClientData>,
) -> Result<JobResultMsg, ClientErr> {
    let job = job.job;

    let mut public_cfg = check_download_read_test_suite(job.test_suite, &*cfg).await?;

    send.lock()
        .await
        .send_msg(&ClientMsg::JobProgress(JobProgressMsg {
            id: job.id,
            job_stage: JobStage::Fetching,
        }))
        .await?;

    // Clone the repo specified in job
    let job_path = cfg.job_folder(job.id);
    fs::net::git_clone(
        Some(&job_path),
        fs::net::GitCloneOptions {
            repo: job.repo,
            branch: job.branch,
            depth: 3,
        },
    )
    .await?;

    let judge_cfg: PathBuf = fs::find_judge_root(&job_path).await?;
    let mut job_path = judge_cfg.clone();
    job_path.pop();

    let judge_cfg = tokio::fs::read(judge_cfg).await?;
    let judge_cfg = toml::from_slice::<JudgeToml>(&judge_cfg)?;

    let judge_job_cfg = judge_cfg
        .jobs
        .get(&public_cfg.name)
        .ok_or_else(|| ClientErr::NoSuchFile(public_cfg.name.to_owned()))?;

    let image = judge_job_cfg.image.clone();

    // Set run script
    let run = judge_job_cfg
        .run
        .iter()
        .chain(public_cfg.run.iter())
        .map(|x| x.to_owned())
        .collect::<Vec<_>>();
    public_cfg.run = run;

    let private_cfg = JudgerPrivateConfig {
        test_root_dir: job_path,
    };

    let options = crate::tester::exec::TestSuiteOptions {
        tests: job.test.clone(),
        time_limit: None,
        mem_limit: None,
        build_image: true,
        remove_image: true,
    };

    let mut suite =
        crate::tester::exec::TestSuite::from_config(image, private_cfg, public_cfg, options)?;

    let (ch_send, ch_recv) = futures::channel::mpsc::unbounded();

    let recv_handle = tokio::spawn({
        let mut recv = ch_recv;
        let ws_send = send.clone();
        let job_id = job.id;
        async move {
            while let Some((key, res)) = recv.next().await {
                ws_send
                    .lock()
                    .await
                    .send_msg(&ClientMsg::PartialResult(PartialResultMsg {
                        job_id,
                        test_id: key,
                        test_result: res,
                    }))
                    .await;
            }
        }
    });

    let result = suite
        .run(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Some(ch_send),
        )
        .await;

    recv_handle.await;

    let job_result = JobResultMsg {
        job_id: job.id,
        test_results: todo!("Create job results"),
        job_result: JobResultKind::Accepted,
        message: None,
    };
    Ok(job_result)
}

pub async fn client_loop(
    mut ws_recv: WsStream,
    mut ws_send: WsSink,
    client_config: Arc<SharedClientData>,
) {
    ws_send
        .send_msg(&ClientMsg::ClientStatus(ClientStatusMsg {
            active_task_count: 0,
            can_accept_new_task: true,
        }))
        .await
        .unwrap();

    let ws_send = Arc::new(Mutex::new(ws_send));
    while let Some(Some(Ok(x))) = {
        let mut ws_lock = ws_recv.next().fuse();
        // TODO: add abort mechaisms
        // let mut abort_lock = abort.next().fuse();
        let mut abort_lock = futures::future::pending::<()>();
        futures::select_biased! {
            abort = abort_lock => None,
            ws = ws_lock => Some(ws)
        }
    } {
        if x.is_text() {
            let msg = from_slice::<ServerMsg>(&x.into_data());
            if let Ok(msg) = msg {
                match msg {
                    ServerMsg::NewJob(job) => {
                        let send = ws_send.clone();
                        tokio::spawn(handle_job_wrapper(job, send, client_config.clone()));
                    }
                    ServerMsg::AbortJob(job) => todo!("Abort {}", job.job_id),
                }
            } else {
                log::warn!("Unknown binary message");
            }
        }
    }

    ws_send.lock().await.close().await.unwrap();
    log::warn!("Disconnected!");
}
