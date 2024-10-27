use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::executor::ThreadPool;
use futures::StreamExt;
use rand::{thread_rng, RngCore};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

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

pub fn longest_common_prefix(arr: &[&str]) -> String {
    if arr.len() == 0 {
        return String::new();
    }
    let mut result = String::with_capacity(
        arr.iter()
            .min_by(|p, n| p.len().cmp(&n.len()))
            .unwrap()
            .len(),
    );
    let mut first = arr[0].chars();
    let mut rests: Vec<_> = arr[1..].iter().map(|s| s.chars()).collect();
    while let Some(ch) = first.next() {
        for i in 0..rests.len() {
            let current = &mut rests[i];
            match current.next() {
                Some(c) if c == ch => continue,
                _ => return result,
            }
        }
        result.push(ch);
    }
    result
}

#[derive(Clone)]
struct MatchResult {
    nonce: Nonce,
    matched_length: usize,
    postfix: u8,
    fully_matched: bool,
}

type Nonce = [u8; 16];

impl MatchResult {
    fn better_than(&self, r: Option<&MatchResult>) -> bool {
        match r {
            None => true,
            Some(r) => {
                self.fully_matched
                    || self.matched_length > r.matched_length
                    || self.matched_length == r.matched_length && self.postfix > r.postfix
            }
        }
    }
}

fn bitwork_match_hash(nonce: Nonce, hash: &str, target: &str, pre: usize, post: u8) -> MatchResult {
    let prefix_str = longest_common_prefix(&[hash, target]);
    let matched_length = prefix_str.len().min(pre);
    let postfix =
        u8::from_str_radix(hash.get(matched_length..matched_length + 1).unwrap(), 16).unwrap();
    if matched_length == pre {
        MatchResult {
            nonce,
            matched_length: pre,
            fully_matched: postfix >= post,
            postfix,
        }
    } else {
        MatchResult {
            nonce,
            matched_length,
            postfix,
            fully_matched: false,
        }
    }
}

fn check_match(nonce: Nonce, job: &Job) -> MatchResult {
    let mut b = job.buffer.clone();
    b[job.hex_start..job.hex_start + 16].copy_from_slice(&nonce);
    let digest = ring::digest::digest(&ring::digest::SHA256, &b);
    let digest = ring::digest::digest(&ring::digest::SHA256, digest.as_ref());
    let mut hash = (digest.as_ref() as &[u8]).to_vec();
    hash.reverse();
    bitwork_match_hash(
        nonce,
        &hex::encode(hash),
        &job.remote_hash,
        job.pre,
        job.post,
    )
}

// Sending `None` to signal the computation should be terminated
type ResultSender = UnboundedSender<Option<MatchResult>>;

type HashCountSender = UnboundedSender<u64>;

async fn task(pre: u8, mask: u8, job: Job, tx: ResultSender, hr: HashCountSender) {
    //println!("task pre {:x} mask {:x}", pre, mask);
    let mut rng = thread_rng();
    let mut count = 0;
    let mut previous_result = None;
    loop {
        count += 1;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        if tx.is_closed() || now > job.next_block_time as u128 {
            break;
        };
        let mut nonce: [u8; 16] = [0; 16];
        rng.fill_bytes(&mut nonce);
        nonce[0] = (nonce[0] & mask) | pre;
        let result = check_match(nonce, &job);
        if result.better_than(previous_result.as_ref()) {
            previous_result = Some(result.clone());
            let _ = tx.unbounded_send(Some(result)).ok();
        }
    }
    let _ = hr.unbounded_send(count).ok();
    //println!("task pre {:x} done", pre);
}

type SubmissionNotifier = Arc<RwLock<Option<UnboundedSender<()>>>>;

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
    let pool = ThreadPool::new().unwrap();
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
            let pool = pool.clone();
            let url = url.clone();
            let notify = Arc::clone(&notify_submission);
            let miner_id = miner_id.clone();
            std::thread::spawn(move || {
                async_std::task::block_on(start_round(&url, &miner_id, &pool, req, notify))
            });
        }
    }
}

async fn start_round(
    url: &str,
    miner_id: &str,
    pool: &ThreadPool,
    job: JobDesc,
    notify: SubmissionNotifier,
) {
    let begin = SystemTime::now();
    let block_height = job.block_height;
    let (tx, rx) = unbounded();
    *notify.write().unwrap() = Some(tx);
    let pre = job.pre;
    let (hash_count_rx, mut nonce_rx) = start_hashing(&pool, job, rx);
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
    println!("Hashrate: {}/s", count as u128 * 1000 / duration);
    /*
        if !found {
            println!("Job {} aborted", block_height);
        }
    */
}

fn start_hashing(
    pool: &ThreadPool,
    job: JobDesc,
    on_termination: UnboundedReceiver<()>,
) -> (
    UnboundedReceiver<u64>,
    UnboundedReceiver<Option<MatchResult>>,
) {
    let job: Job = job.into();
    let (tx, rx) = unbounded();
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
    pool.spawn_ok(monitor_termination(on_termination, tx));
    return (metrics_rx, rx);
}

async fn monitor_termination(mut on_termination: UnboundedReceiver<()>, tx: ResultSender) {
    on_termination.next().await;
    println!("termination detected");
    let _ = tx.unbounded_send(None).ok();
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
        pre: 5,
        post_hex: "7".to_string(),
        next_block_time: now + 1000 * 60,
        block_height: 123,
        submitted: false,
    };
    let (_tx, rx) = unbounded();
    let (_, mut nonce_rx) = start_hashing(&pool, job, rx);
    let mut last_result = None;
    while let Some(Some(result)) = nonce_rx.next().await {
        if result.better_than(last_result.as_ref()) {
            last_result = Some(result.clone());
        }
        if result.fully_matched {
            break;
        }
    }
    assert!(last_result.is_some());
    assert!(last_result.unwrap().fully_matched);
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
    assert!(check_match(bytes, &job).fully_matched);
}
