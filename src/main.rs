use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::executor::ThreadPool;
use futures::StreamExt;
use rand::{thread_rng, RngCore};
use serde::{Deserialize, Serialize};
use std::future::IntoFuture;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize)]
struct Response {
    block_height: usize,
    hash: String,
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
struct Job {
    buffer: Vec<u8>,
    hex_start: usize,
    remote_hash: String,
    pre: usize,
    post: u8,
    next_block_time: u64,
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

fn bitwork_match_hash(hash: &str, target: &str, pre: usize, post: u8) -> bool {
    let current_pre = hash.get(0..pre).unwrap();
    let current_post = hash.get(pre..pre + 1).unwrap();
    let target_pre = target.get(0..pre).unwrap();
    current_pre == target_pre && u8::from_str_radix(current_post, 16).unwrap() >= post
}

fn check_match(hex: [u8; 16], job: &Job) -> bool {
    let mut b = job.buffer.clone();
    b[job.hex_start..job.hex_start + 16].copy_from_slice(&hex);
    let digest = ring::digest::digest(&ring::digest::SHA256, &b);
    let digest = ring::digest::digest(&ring::digest::SHA256, digest.as_ref());
    let mut hash = (digest.as_ref() as &[u8]).to_vec();
    hash.reverse();
    bitwork_match_hash(&hex::encode(hash), &job.remote_hash, job.pre, job.post)
}

async fn task(
    pre: u8,
    mask: u8,
    job: Job,
    tx: UnboundedSender<Option<[u8; 16]>>,
    metrics_tx: UnboundedSender<u64>,
) {
    //println!("task pre {:x} mask {:x}", pre, mask);
    let mut rng = thread_rng();
    let mut count = 0;
    loop {
        count += 1;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        if tx.is_closed() || now > job.next_block_time as u128 {
            break;
        };
        let mut hex: [u8; 16] = [0; 16];
        rng.fill_bytes(&mut hex);
        hex[0] = (hex[0] & mask) | pre;
        if check_match(hex, &job) {
            let _ = tx.unbounded_send(Some(hex)).ok();
        }
    }
    let _ = metrics_tx.unbounded_send(count).ok();
    //println!("task pre {:x} done", pre);
}

#[async_std::main]
async fn main() {
    let miner_id = std::env::var("MINER_ID").unwrap_or_else(|_| {
        panic!("Please set your MINER_ID first, an alphanumeric string of 8 characters")
    });
    let args = std::env::args().collect::<Vec<_>>();
    let host = if args.len() >= 2 {
        &args[1]
    } else {
        "localhost"
    };
    let port = if args.len() >= 3 { &args[2] } else { "3030" };
    let url = format!("http://{host}:{port}");
    let pool = ThreadPool::new().unwrap();
    let mut block_height = 0;
    let submitted_tx: Arc<RwLock<Option<UnboundedSender<()>>>> = Arc::new(RwLock::new(None));
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
                if let Some(tx) = submitted_tx.write().unwrap().take() {
                    println!("Job submitted by others, terminating...");
                    let _ = tx.unbounded_send(()).ok();
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        } else {
            block_height = req.block_height;
            println!("Starting new job {}", block_height);
            let (tx, rx) = unbounded();
            *submitted_tx.write().unwrap() = Some(tx);
            let pool = pool.clone();
            let url = url.clone();
            let submitted_tx = Arc::clone(&submitted_tx);
            let miner_id = miner_id.clone();
            std::thread::spawn(move || {
                async_std::task::block_on(async {
                    if let Some(hash) = start_hashing(&pool, req, Some(rx)).await {
                        println!("Job {} done. Submitting hash {}", block_height, hash);
                        let res = Response { block_height, hash };
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
                            Ok(_) => {
                                *submitted_tx.write().unwrap() = None;
                                println!("Job {} submitted", block_height);
                            }
                        };
                    } else {
                        println!("Job {} aborted", block_height);
                    }
                })
            });
        }
    }
}

async fn monitor_submitted<T>(
    mut submitted: UnboundedReceiver<()>,
    tx: UnboundedSender<Option<T>>,
) {
    submitted.next().into_future().await;
    let _ = tx.unbounded_send(None).ok();
}

async fn start_hashing(
    pool: &ThreadPool,
    job: JobDesc,
    submitted: Option<UnboundedReceiver<()>>,
) -> Option<String> {
    let begin = SystemTime::now();
    let job: Job = job.into();
    let (tx, mut rx) = unbounded();
    let (metrics_tx, metrics_rx) = unbounded();
    let mut bits = 0;
    let mut max_threads = 1;
    while max_threads * 2 < num_cpus::get() {
        bits += 1;
        max_threads = max_threads * 2;
    }
    println!("bits = {}, max_threads = {}", bits, max_threads);
    for i in 0..max_threads {
        let pre = (i as u8) << (8 - bits);
        let mask = (1 << (8 - bits)) - 1;
        pool.spawn_ok(task(pre, mask, job.clone(), tx.clone(), metrics_tx.clone()))
    }
    drop(metrics_tx);
    if let Some(submitted) = submitted {
        pool.spawn_ok(monitor_submitted(submitted, tx));
    }
    let result = rx.next().into_future().await.flatten().map(hex::encode);
    drop(rx);
    let count: u64 = metrics_rx
        .collect::<Vec<_>>()
        .into_future()
        .await
        .into_iter()
        .sum();
    let duration = SystemTime::now().duration_since(begin).unwrap().as_millis();
    println!("Hashrate: {}/s", count as u128 * 1000 / duration);
    return result;
}

#[async_std::test]
async fn test_start_hashing() {
    let pool = ThreadPool::new().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let job = JobDesc {
        buffer: "0100000001ce15e74c3df9a9084b84cd74c4fa135ac76d235c358639a6cb8280ac77c9b9670000000000fdffffff034905000000000000225120096d50b1af027bd0efd97c0ed9b8ba8cdc8e0678c2ed93c3c4640c849382a38f0000000000000000126a109d4b1212d0c917e668e55bbeb5eda7171b51010000000000225120bf247ef829e040cd0a4f4a879aeb2ac7fd3adfd23fbdda7760a9fbfdc781a0ba00000000".to_string(),
        hex_start: 101,
        remote_hash:"ce15e74c3df9a9084b84cd74c4fa135ac76d235c358639a6cb8280ac77c9b967".to_string(),
        pre: 6,
        post_hex: "7".to_string(),
        next_block_time: now + 1000 * 60,
        block_height: 123,
        submitted: false,
    };
    assert!(start_hashing(&pool, job, None).await.is_some())
}

#[test]
fn test_bitwork_match_hash() {
    let buffer =  hex::decode("0100000001ce15e74c3df9a9084b84cd74c4fa135ac76d235c358639a6cb8280ac77c9b9670000000000fdffffff034905000000000000225120096d50b1af027bd0efd97c0ed9b8ba8cdc8e0678c2ed93c3c4640c849382a38f0000000000000000126a109d4b1212d0c917e668e55bbeb5eda7171b51010000000000225120bf247ef829e040cd0a4f4a879aeb2ac7fd3adfd23fbdda7760a9fbfdc781a0ba00000000").unwrap();
    let hex = hex::decode("3d3d8373e1ad7b715efdb93cfa69431f").unwrap();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hex);
    let remote_hash = "ce15e74c3df9a9084b84cd74c4fa135ac76d235c358639a6cb8280ac77c9b967";
    let job = Job {
        buffer: buffer.clone(),
        hex_start: 101,
        remote_hash: remote_hash.to_string(),
        pre: 6,
        post: 7,
        next_block_time: 0,
    };
    assert!(check_match(bytes, &job));
}
