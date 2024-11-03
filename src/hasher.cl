#define TX_MAX_BYTES 256
#define NONCE_BYTES 16
#define HASH_BYTES 32

__constant uchar hex[16] = { '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f' };
void reverse_hex_encode(uchar *arr, int len, uchar *p) {
  for (int i = len - 1; i > 0; i--) {
    uchar hi = arr[i] >> 4;
    uchar lo = arr[i] & 0xf;
    *(p++) = hex[hi];
    *(p++) = hex[lo]; 
  }
}

void fill_nonce(uchar *buffer, ulong hi, ulong lo)                
{
   ulong * p = (ulong *) buffer;
   *p = hi;
   *(p + 1) = lo;
}

void sha256d(uchar *input, int input_len, uchar * output) {
  sha256((__private const unsigned int *)input, input_len, (__private unsigned int *)output);
  sha256((__private const unsigned int *)output, 32, (__private unsigned int *)output);
}

__kernel void hasher( __global uchar * transaction, uint32_t tx_len,
                                uint   hex_start,
                               ulong   rng_seed,
                                uint   iterations,
                      __global uchar * remote_hash,
                      __global uchar * result_nonces,
                      __global uchar * result_hashes,
                    __global uint8_t * result_matched) {
  ulong idx = get_global_id(0);
  kiss09_state rng;
  kiss09_seed(&rng, rng_seed + idx);

  uchar tx[TX_MAX_BYTES];
  uchar * p = (uchar *) tx;
  memcpy_from_global(p, transaction, (size_t) tx_len);

  uchar result_nonce[NONCE_BYTES];
  uchar result_hash[HASH_BYTES];
  int matched = 0;

  while (iterations > 0) {
    iterations--; 
    uchar nonce[16];
    fill_nonce(nonce, kiss09_ulong(rng), kiss09_ulong(rng));
    memcpy(p + hex_start, nonce, NONCE_BYTES);
    //print_byte_array_hex(nonce, 16);

    uchar hash[32];
    sha256d(p, tx_len, (uchar *)hash);
    //print_byte_array_hex(hash, 32);

    uchar hex[64];
    reverse_hex_encode((uchar *)hash, 32, (uchar *)hex);
    int i = 0;
    while (i<HASH_BYTES && hex[i] == remote_hash[i]) { i++; };
    if (i > matched) {
      matched = i;
      memcpy(result_nonce, nonce, NONCE_BYTES);
      memcpy(result_hash, hash, HASH_BYTES);
    }
  }
  result_matched[idx] = (uint8_t) matched;  
/*
  if (idx == 0) {
  printf("matched = %d\n", matched);
  print_byte_array_hex(result_nonce, 16);
  print_byte_array_hex(result_hash, 32);
  }
*/
  memcpy_to_global(result_nonces + NONCE_BYTES * idx, (uchar *) result_nonce, NONCE_BYTES);
  memcpy_to_global(result_hashes + HASH_BYTES * idx, (uchar *) result_hash, HASH_BYTES);
}
