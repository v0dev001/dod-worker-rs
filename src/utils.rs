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
pub struct MatchResult {
    pub nonce: Nonce,
    pub matched_length: usize,
    pub postfix: u8,
    pub fully_matched: bool,
}

pub type Nonce = [u8; 16];

impl MatchResult {
    pub fn better_than(&self, r: Option<&MatchResult>) -> bool {
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

pub fn check_match(nonce: Nonce, job: &super::Job) -> MatchResult {
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

#[test]
fn test_bitwork_match_hash() {
    let buffer =  hex::decode("0100000001ce15e74c3df9a9084b84cd74c4fa135ac76d235c358639a6cb8280ac77c9b9670000000000fdffffff034905000000000000225120096d50b1af027bd0efd97c0ed9b8ba8cdc8e0678c2ed93c3c4640c849382a38f0000000000000000126a109d4b1212d0c917e668e55bbeb5eda7171b51010000000000225120bf247ef829e040cd0a4f4a879aeb2ac7fd3adfd23fbdda7760a9fbfdc781a0ba00000000").unwrap();
    let hex = hex::decode("3d3d8373e1ad7b715efdb93cfa69431f").unwrap();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hex);
    let remote_hash = "ce15e74c3df9a9084b84cd74c4fa135ac76d235c358639a6cb8280ac77c9b967";
    let job = super::Job {
        buffer: buffer.clone(),
        hex_start: 101,
        remote_hash: remote_hash.to_string(),
        pre: 6,
        post: 7,
        next_block_time: 0,
    };
    assert!(check_match(bytes, &job).fully_matched);
}
