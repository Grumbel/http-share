// SPDX-FileCopyrightText: 2026 Ingo Ruhnke <grumbel@gmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later

//! Minimal QR encoder (byte mode, ECC-L, versions 1–6).

pub(crate) fn qr_print(text: &str) {
    match qr_encode(text.as_bytes()) {
        Some(matrix) => {
            let n = matrix.len();
            let border = 2;
            println!();
            for y in -(border as isize)..(n as isize + border as isize) {
                print!("  ");
                for x in -(border as isize)..(n as isize + border as isize) {
                    let on = if x >= 0 && y >= 0 && (x as usize) < n && (y as usize) < n {
                        matrix[y as usize][x as usize]
                    } else {
                        false
                    };
                    if on {
                        print!("██");
                    } else {
                        print!("  ");
                    }
                }
                println!();
            }
            println!();
        }
        None => {
            eprintln!("(QR: data too long for built-in encoder)");
        }
    }
}

pub(crate) fn qr_encode(data: &[u8]) -> Option<Vec<Vec<bool>>> {
    const CAP: [usize; 7] = [0, 19, 34, 55, 80, 108, 136];
    const SIZE: [usize; 7] = [0, 21, 25, 29, 33, 37, 41];
    const ECC_CW: [usize; 7] = [0, 7, 10, 15, 20, 26, 36];
    const NBLOCKS: [usize; 7] = [0, 1, 1, 1, 1, 1, 2];

    let need = data.len() + 3;
    let mut version = 0;
    for v in 1..=6 {
        if CAP[v] >= need {
            version = v;
            break;
        }
    }
    if version == 0 {
        return None;
    }

    let size = SIZE[version];
    let data_cw = CAP[version];
    let ecc_cw = ECC_CW[version];
    let nblocks = NBLOCKS[version];

    let mut bits: Vec<bool> = Vec::new();
    for b in [false, true, false, false] {
        bits.push(b);
    }
    let len = data.len() as u16;
    for i in (0..8).rev() {
        bits.push((len >> i) & 1 == 1);
    }
    for &byte in data {
        for i in (0..8).rev() {
            bits.push((byte >> i) & 1 == 1);
        }
    }
    for _ in 0..4 {
        if bits.len() >= data_cw * 8 {
            break;
        }
        bits.push(false);
    }
    while bits.len() % 8 != 0 {
        bits.push(false);
    }
    let mut pad = true;
    while bits.len() / 8 < data_cw {
        let p: u8 = if pad { 0xEC } else { 0x11 };
        pad = !pad;
        for i in (0..8).rev() {
            bits.push((p >> i) & 1 == 1);
        }
    }
    bits.truncate(data_cw * 8);

    let data_bytes: Vec<u8> = bits
        .chunks(8)
        .map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b as u8))
        .collect();

    let gen = rs_generator(ecc_cw / nblocks.max(1));
    let block_data_len = data_cw / nblocks;
    let block_ecc_len = ecc_cw / nblocks;
    let mut ecc_blocks: Vec<Vec<u8>> = Vec::new();
    for b in 0..nblocks {
        let start = b * block_data_len;
        let end = if b + 1 == nblocks { data_cw } else { start + block_data_len };
        let block = &data_bytes[start..end];
        ecc_blocks.push(rs_encode(block, &gen));
    }

    let mut final_bytes: Vec<u8> = Vec::new();
    let max_d = (data_bytes.len() + nblocks - 1) / nblocks;
    for i in 0..max_d {
        for b in 0..nblocks {
            let start = b * block_data_len;
            let end = if b + 1 == nblocks { data_cw } else { start + block_data_len };
            if i < end - start {
                final_bytes.push(data_bytes[start + i]);
            }
        }
    }
    for i in 0..block_ecc_len {
        for b in 0..nblocks {
            final_bytes.push(ecc_blocks[b][i]);
        }
    }

    let mut matrix = vec![vec![None::<bool>; size]; size];

    place_finder(&mut matrix, 0, 0);
    place_finder(&mut matrix, size - 7, 0);
    place_finder(&mut matrix, 0, size - 7);

    for i in 0..8 {
        if i < size {
            if matrix[7][i].is_none() { matrix[7][i] = Some(false); }
            if matrix[i][7].is_none() { matrix[i][7] = Some(false); }
            if matrix[size - 8][i].is_none() { matrix[size - 8][i] = Some(false); }
            if matrix[i][size - 8].is_none() { matrix[i][size - 8] = Some(false); }
            if matrix[size - 1 - i][7].is_none() { matrix[size - 1 - i][7] = Some(false); }
            if matrix[7][size - 1 - i].is_none() { matrix[7][size - 1 - i] = Some(false); }
        }
    }

    for i in 8..size - 8 {
        if matrix[6][i].is_none() { matrix[6][i] = Some(i % 2 == 0); }
        if matrix[i][6].is_none() { matrix[i][6] = Some(i % 2 == 0); }
    }

    if version >= 2 {
        let positions: &[usize] = match version {
            2 => &[6, 18],
            3 => &[6, 22],
            4 => &[6, 26],
            5 => &[6, 30],
            6 => &[6, 34],
            _ => &[],
        };
        for &r in positions {
            for &c in positions {
                if (r == 6 && c == 6) || (r == 6 && c == size - 7) || (r == size - 7 && c == 6) {
                    continue;
                }
                place_alignment(&mut matrix, r, c);
            }
        }
    }

    matrix[size - 8][8] = Some(true);

    for i in 0..9 {
        if matrix[8][i].is_none() { matrix[8][i] = Some(false); }
        if matrix[i][8].is_none() { matrix[i][8] = Some(false); }
    }
    for i in 0..8 {
        if matrix[8][size - 1 - i].is_none() { matrix[8][size - 1 - i] = Some(false); }
        if matrix[size - 1 - i][8].is_none() { matrix[size - 1 - i][8] = Some(false); }
    }

    let mut bit_idx = 0;
    let total_bits = final_bytes.len() * 8;
    let mut col = size as isize - 1;
    let mut upward = true;
    while col > 0 {
        if col == 6 { col -= 1; }
        let row_range: Vec<isize> = if upward {
            (0..size as isize).rev().collect()
        } else {
            (0..size as isize).collect()
        };
        for row in row_range {
            for dc in [0, -1] {
                let c = col + dc;
                if c < 0 || c >= size as isize { continue; }
                if matrix[row as usize][c as usize].is_some() { continue; }
                let bit = if bit_idx < total_bits {
                    let byte = final_bytes[bit_idx / 8];
                    let b = (byte >> (7 - (bit_idx % 8))) & 1 == 1;
                    bit_idx += 1;
                    b
                } else {
                    false
                };
                let mask = (row + c) % 2 == 0;
                matrix[row as usize][c as usize] = Some(bit ^ mask);
            }
        }
        upward = !upward;
        col -= 2;
    }

    let format: u16 = 0b111011111000100;
    for i in 0..6 {
        matrix[8][i] = Some((format >> (14 - i)) & 1 == 1);
    }
    matrix[8][7] = Some((format >> 8) & 1 == 1);
    matrix[8][8] = Some((format >> 7) & 1 == 1);
    matrix[7][8] = Some((format >> 6) & 1 == 1);
    for i in 0..6 {
        matrix[5 - i][8] = Some((format >> (5 - i)) & 1 == 1);
    }
    for i in 0..7 {
        matrix[size - 1 - i][8] = Some((format >> (14 - i)) & 1 == 1);
    }
    for i in 0..8 {
        matrix[8][size - 8 + i] = Some((format >> (7 - i)) & 1 == 1);
    }

    Some(
        matrix
            .into_iter()
            .map(|row| row.into_iter().map(|c| c.unwrap_or(false)).collect())
            .collect(),
    )
}

pub(crate) fn place_finder(m: &mut [Vec<Option<bool>>], row: usize, col: usize) {
    for dr in 0..7 {
        for dc in 0..7 {
            let on = dr == 0 || dr == 6 || dc == 0 || dc == 6
                || (dr >= 2 && dr <= 4 && dc >= 2 && dc <= 4);
            m[row + dr][col + dc] = Some(on);
        }
    }
}

pub(crate) fn place_alignment(m: &mut [Vec<Option<bool>>], cx: usize, cy: usize) {
    for dr in -2isize..=2 {
        for dc in -2isize..=2 {
            let r = (cx as isize + dr) as usize;
            let c = (cy as isize + dc) as usize;
            let on = dr.abs() == 2 || dc.abs() == 2 || (dr == 0 && dc == 0);
            m[r][c] = Some(on);
        }
    }
}

pub(crate) fn rs_generator(nsym: usize) -> Vec<u8> {
    let mut g = vec![1u8];
    for i in 0..nsym {
        let mut ng = vec![0u8; g.len() + 1];
        let alpha = gf_pow(2, i as u32);
        for (j, &c) in g.iter().enumerate() {
            ng[j] ^= c;
            ng[j + 1] ^= gf_mul_correct(c, alpha);
        }
        g = ng;
    }
    g
}

pub(crate) fn gf_pow(mut base: u8, mut exp: u32) -> u8 {
    let mut r = 1u8;
    while exp > 0 {
        if exp & 1 != 0 {
            r = gf_mul_correct(r, base);
        }
        base = gf_mul_correct(base, base);
        exp >>= 1;
    }
    r
}

pub(crate) fn gf_mul_correct(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = (a & 0x80) != 0;
        a <<= 1;
        if hi {
            a ^= 0x1d;
        }
        b >>= 1;
    }
    p
}

pub(crate) fn rs_encode(data: &[u8], gen: &[u8]) -> Vec<u8> {
    let nsym = gen.len() - 1;
    let mut res = vec![0u8; data.len() + nsym];
    res[..data.len()].copy_from_slice(data);
    for i in 0..data.len() {
        let coef = res[i];
        if coef != 0 {
            for j in 0..gen.len() {
                res[i + j] ^= gf_mul_correct(gen[j], coef);
            }
        }
    }
    res[data.len()..].to_vec()
}
