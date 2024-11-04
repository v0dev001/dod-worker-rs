use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures::StreamExt;
use getopts::Options;
use serde::{Deserialize, Serialize};
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

// Sending `None` to signal the computation should be terminated
type ResultSender = UnboundedSender<Option<utils::MatchResult>>;

type HashCountSender = UnboundedSender<u64>;

pub type OnTermination = async_broadcast::Receiver<()>;

pub trait Hasher: Clone {
    fn start_hashing(
        &self,
        job: Job,
        on_termination: OnTermination,
    ) -> (
        UnboundedReceiver<u64>,
        UnboundedReceiver<Option<utils::MatchResult>>,
    );
}

fn usage(program: &str, opts: &Options) {
    let brief = format!("Usage: {} URL [options]", program);
    print!("\n{}", opts.usage(&brief));
}

#[async_std::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let program = args[0].clone();

    let mut opts = Options::new();
    opts.optopt("", "miner-id", "", "MINER_ID");
    opts.optflag("", "help", "");
    let matches = match opts.parse(&args[1..]) {
        Ok(m) => m,
        Err(f) => {
            panic!("{}", f)
        }
    };
    if matches.opt_present("help") {
        usage(&program, &opts);
        std::process::exit(1)
    };
    let miner_id = matches.opt_str("miner-id").unwrap_or_else(|| {
        std::env::var("MINER_ID").unwrap_or_else(|_| {
            println!("ERROR: use '--miner-id XXXX' or set the MINER_ID environment variable.");
            usage(&program, &opts);
            std::process::exit(1)
        })
    });
    let url_str = if !matches.free.is_empty() {
        matches.free[0].clone()
    } else {
        "http://localhost:3030".to_string()
    };
    let url = [
        url_str.clone(),
        format!("http://{}:3030", url_str),
        format!("http://{}", url_str),
    ]
    .iter()
    .flat_map(|s| url::Url::parse(s).ok())
    .next()
    .unwrap_or_else(|| {
        println!("ERROR: use a valid mining pool URL");
        usage(&program, &opts);
        std::process::exit(1)
    });
    #[cfg(not(feature = "gpu"))]
    let hasher = cpu::setup();
    #[cfg(feature = "gpu")]
    let hasher = gpu::setup().unwrap();
    let mut block_height = 0;
    let mut notify: Option<async_broadcast::Sender<()>> = None;
    println!("Connecting to DOD mining pool at {}", url);
    loop {
        let mut job_url = url.clone();
        job_url.set_path("job");
        let res = match minreq::get(job_url.as_str()).send() {
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
                if let Some(notify_submission) = notify.take() {
                    println!("Job submitted by others, terminating...");
                    notify_submission.close();
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        } else {
            block_height = req.block_height;
            println!("Starting new job {}", block_height);
            let (notify_submission, on_termination) = async_broadcast::broadcast(1);
            notify = Some(notify_submission);
            let hasher = hasher.clone();
            let url = url.clone();
            let miner_id = miner_id.clone();
            std::thread::spawn(move || {
                async_std::task::block_on(start_round(hasher, &url, &miner_id, req, on_termination))
            });
        }
    }
}

async fn start_round<T: Hasher>(
    hasher: T,
    url: &url::Url,
    miner_id: &str,
    job: JobDesc,
    on_termination: OnTermination,
) {
    let begin = SystemTime::now();
    let block_height = job.block_height;
    let pre = job.pre;
    let (hash_count_rx, mut nonce_rx) = hasher.start_hashing(job.into(), on_termination.clone());
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
                let mut post_url = url.clone();
                post_url.set_path("answer");
                match minreq::post(post_url.as_str())
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
    if duration > 0 {
        println!("Hashrate: {}/s", count as u128 * 1000 / duration);
    }
}
