use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

#[cfg(not(feature = "gpu"))]
mod cpu;
#[cfg(feature = "gpu")]
mod gpu;
mod utils;

const MIN_MATCH_GAP: usize = 4;

#[derive(Debug, Serialize)]
struct Response {
    block_height: usize,
    nonce: String,
}

type Request = JobDesc;

#[derive(Eq, PartialEq, Debug, Serialize, Deserialize)]
struct JobDesc {
    buffer: String,
    hex_start: usize,
    remote_hash: String,
    pre: usize,
    post_hex: String,
    next_block_time: u64,
    block_height: usize,
    submitted: bool,
}

#[derive(Clone)]
pub struct Job {
    pub buffer: Vec<u8>,
    pub hex_start: usize,
    pub remote_hash: String,
    pub pre: usize,
    pub post: u8,
    pub next_block_time: u64,
}

impl From<JobDesc> for Job {
    fn from(desc: JobDesc) -> Self {
        Job {
            buffer: hex::decode(&desc.buffer).unwrap(),
            hex_start: desc.hex_start,
            remote_hash: desc.remote_hash,
            pre: desc.pre,
            post: u8::from_str_radix(&desc.post_hex, 16).unwrap(),
            next_block_time: desc.next_block_time,
        }
    }
}

type SubmissionNotifier = Arc<RwLock<Option<UnboundedSender<()>>>>;

// Sending `None` to signal the computation should be terminated
type ResultSender = UnboundedSender<Option<utils::MatchResult>>;

type HashCountSender = UnboundedSender<u64>;

pub trait Hasher: Clone {
    fn start_hashing(
        &self,
        job: Job,
        on_termination: UnboundedReceiver<()>,
    ) -> (
        UnboundedReceiver<u64>,
        UnboundedReceiver<Option<utils::MatchResult>>,
    );
}

#[async_std::main]
async fn main() {
    let miner_id =
        std::env::var("MINER_ID").unwrap_or_else(|_| panic!("Please set your MINER_ID first."));
    let args = std::env::args().collect::<Vec<_>>();
    let host = if args.len() >= 2 {
        &args[1]
    } else {
        "localhost"
    };
    let port = if args.len() >= 3 { &args[2] } else { "3030" };
    let url = format!("http://{host}:{port}");
    #[cfg(not(feature = "gpu"))]
    let hasher = cpu::setup();
    #[cfg(feature = "gpu")]
    let hasher = gpu::setup().unwrap();
    let mut block_height = 0;
    let notify_submission: SubmissionNotifier = Arc::new(RwLock::new(None));
    loop {
        let res = match minreq::get(&format!("{url}/job")).send() {
            Err(err) => {
                eprintln!("{}", err);
                std::thread::sleep(std::time::Duration::from_millis(1000));
                continue;
            }
            Ok(res) => res,
        };
        let req: Request = match serde_json::from_str(res.as_str().unwrap()) {
            Err(err) => {
                eprintln!("{}", err);
                std::thread::sleep(std::time::Duration::from_millis(1000));
                continue;
            }
            Ok(job) => job,
        };
        if req.block_height == block_height {
            if req.submitted {
                if let Some(tx) = notify_submission.write().unwrap().take() {
                    println!("Job submitted by others, terminating...");
                    let _ = tx.unbounded_send(()).ok();
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        } else {
            block_height = req.block_height;
            println!("Starting new job {}", block_height);
            let hasher = hasher.clone();
            let url = url.clone();
            let notify = Arc::clone(&notify_submission);
            let miner_id = miner_id.clone();
            std::thread::spawn(move || {
                async_std::task::block_on(start_round(hasher, &url, &miner_id, req, notify))
            });
        }
    }
}

async fn start_round<T: Hasher>(
    hasher: T,
    url: &str,
    miner_id: &str,
    job: JobDesc,
    notify: SubmissionNotifier,
) {
    let begin = SystemTime::now();
    let block_height = job.block_height;
    let (tx, rx) = unbounded();
    *notify.write().unwrap() = Some(tx);
    let pre = job.pre;
    let (hash_count_rx, mut nonce_rx) = hasher.start_hashing(job.into(), rx);
    let mut last_result = None;
    while let Some(Some(result)) = nonce_rx.next().await {
        if result.better_than(last_result.as_ref()) {
            last_result = Some(result.clone());
            // Submit only when result is fully matched or meets minimum length
            if result.fully_matched || result.matched_length + MIN_MATCH_GAP > pre {
                let nonce = hex::encode(result.nonce);
                let percent = if result.fully_matched {
                    100
                } else {
                    result.matched_length * 100 / (pre + 1)
                };
                println!("Job {} submitting {} ({}%)", block_height, nonce, percent);
                let res = Response {
                    block_height,
                    nonce,
                };
                match minreq::post(&format!("{url}/answer"))
                    .with_json(&res)
                    .unwrap()
                    .with_header("Content-Type", "application/json")
                    .with_header("miner-id", miner_id)
                    .send()
                {
                    Err(err) => {
                        eprintln!("{}", err);
                    }
                    Ok(res) => {
                        println!("Job {} submitted: {}", block_height, res.as_str().unwrap());
                        if result.fully_matched {
                            *notify.write().unwrap() = None;
                        }
                    }
                }
                if result.fully_matched {
                    break;
                }
            }
        }
    }
    drop(nonce_rx);
    let count: u64 = hash_count_rx.collect::<Vec<_>>().await.into_iter().sum();
    let duration = SystemTime::now().duration_since(begin).unwrap().as_millis();
    if duration > 0 { println!("Hashrate: {}/s", count as u128 * 1000 / duration); }
    /*
        if !found {
            println!("Job {} aborted", block_height);
        }
    */
}
