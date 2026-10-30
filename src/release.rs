// Tooling helper to compute release SHA256 without PowerShell or external shell dependencies
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

fn sha256_digest(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("Falha ao abrir {}: {e}", path.display()))?;
    // Native unpadded software SHA-256 implementation (zero external crate dependencies)
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    let mut total_bytes: u64 = 0;
    let mut buffer = [0u8; 64];
    let mut buf_len = 0;

    let mut chunk = [0u8; 16384];
    loop {
        let n = file.read(&mut chunk).map_err(|e| format!("Erro de leitura: {e}"))?;
        if n == 0 { break; }
        total_bytes += n as u64;
        let mut offset = 0;
        while offset < n {
            let to_copy = (64 - buf_len).min(n - offset);
            buffer[buf_len..buf_len + to_copy].copy_from_slice(&chunk[offset..offset + to_copy]);
            buf_len += to_copy;
            offset += to_copy;
            if buf_len == 64 {
                process_block(&mut h, &buffer, &K);
                buf_len = 0;
            }
        }
    }

    // Padding
    buffer[buf_len] = 0x80;
    buf_len += 1;
    if buf_len > 56 {
        buffer[buf_len..64].fill(0);
        process_block(&mut h, &buffer, &K);
        buffer[..56].fill(0);
    } else {
        buffer[buf_len..56].fill(0);
    }
    let bit_len = total_bytes * 8;
    buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());
    process_block(&mut h, &buffer, &K);

    let mut hex = String::with_capacity(64);
    for val in h {
        hex.push_str(&format!("{val:08x}"));
    }
    Ok(hex)
}

fn process_block(h: &mut [u32; 8], block: &[u8; 64], k: &[u32; 64]) {
    let mut w = [0u32; 64];
    for (i, chunk) in block.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_be_bytes(*chunk);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }

    let mut a = h[0];
    let mut b = h[1];
    let mut c = h[2];
    let mut d = h[3];
    let mut e = h[4];
    let mut f = h[5];
    let mut g = h[6];
    let mut l = h[7];

    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let temp1 = l.wrapping_add(s1).wrapping_add(ch).wrapping_add(k[i]).wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(maj);

        l = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
    h[5] = h[5].wrapping_add(f);
    h[6] = h[6].wrapping_add(g);
    h[7] = h[7].wrapping_add(l);
}

pub fn generate_sha256_file(exe_path: &Path) -> Result<String, String> {
    let hash = sha256_digest(exe_path)?;
    let sha_path = format!("{}.sha256", exe_path.display());
    let file_name = exe_path.file_name().unwrap_or_default().to_string_lossy();
    let content = format!("{hash}  {file_name}\n");
    let mut f = File::create(&sha_path).map_err(|e| format!("Erro ao criar {sha_path}: {e}"))?;
    f.write_all(content.as_bytes()).map_err(|e| format!("Erro ao gravar {sha_path}: {e}"))?;
    Ok(hash)
}
