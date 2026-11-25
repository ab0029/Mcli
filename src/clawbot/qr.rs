//! Minimal pure-Rust ISO/IEC 18004 QR Code encoder (Byte mode, ECC Level L, Versions 1..=10).
//! Supports up to 271 UTF-8 bytes, covering all Tencent iLink `qrcode_img_content` URLs (~84 bytes).

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrMatrix {
    pub size: usize,
    /// Row-major `size * size` dark modules (`true` = dark/black, `false` = light/white).
    pub modules: Vec<bool>,
}

impl QrMatrix {
    pub fn get(&self, x: usize, y: usize) -> bool {
        self.modules[y * self.size + x]
    }
}

struct VersionSpec {
    version: usize,
    total_codewords: usize,
    ec_per_block: usize,
    g1_blocks: usize,
    g1_data: usize,
    g2_blocks: usize,
    g2_data: usize,
    align: &'static [usize],
}

// ISO/IEC 18004 Table 9 (Error Correction Level L) for Versions 1..=10.
const VERSIONS_L: &[VersionSpec] = &[
    VersionSpec {
        version: 1,
        total_codewords: 26,
        ec_per_block: 7,
        g1_blocks: 1,
        g1_data: 19,
        g2_blocks: 0,
        g2_data: 0,
        align: &[],
    },
    VersionSpec {
        version: 2,
        total_codewords: 44,
        ec_per_block: 10,
        g1_blocks: 1,
        g1_data: 34,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 18],
    },
    VersionSpec {
        version: 3,
        total_codewords: 70,
        ec_per_block: 15,
        g1_blocks: 1,
        g1_data: 55,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 22],
    },
    VersionSpec {
        version: 4,
        total_codewords: 100,
        ec_per_block: 20,
        g1_blocks: 1,
        g1_data: 80,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 26],
    },
    VersionSpec {
        version: 5,
        total_codewords: 134,
        ec_per_block: 26,
        g1_blocks: 1,
        g1_data: 108,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 30],
    },
    VersionSpec {
        version: 6,
        total_codewords: 172,
        ec_per_block: 18,
        g1_blocks: 2,
        g1_data: 68,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 34],
    },
    VersionSpec {
        version: 7,
        total_codewords: 196,
        ec_per_block: 20,
        g1_blocks: 2,
        g1_data: 78,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 22, 38],
    },
    VersionSpec {
        version: 8,
        total_codewords: 242,
        ec_per_block: 24,
        g1_blocks: 2,
        g1_data: 97,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 24, 42],
    },
    VersionSpec {
        version: 9,
        total_codewords: 292,
        ec_per_block: 30,
        g1_blocks: 2,
        g1_data: 116,
        g2_blocks: 0,
        g2_data: 0,
        align: &[6, 26, 46],
    },
    VersionSpec {
        version: 10,
        total_codewords: 346,
        ec_per_block: 18,
        g1_blocks: 2,
        g1_data: 68,
        g2_blocks: 2,
        g2_data: 69,
        align: &[6, 28, 50],
    },
];

pub fn encode(text: &str) -> Result<QrMatrix, String> {
    let bytes = text.as_bytes();
    let spec = VERSIONS_L
        .iter()
        .find(|v| {
            let cap = v.g1_blocks * v.g1_data + v.g2_blocks * v.g2_data;
            let count_bits = if v.version <= 9 { 8 } else { 16 };
            let needed_bits = 4 + count_bits + bytes.len() * 8;
            needed_bits <= cap * 8
        })
        .ok_or_else(|| format!("二维码内容过长（{} 字节）", bytes.len()))?;

    let data_cap = spec.g1_blocks * spec.g1_data + spec.g2_blocks * spec.g2_data;
    let codewords = build_data_codewords(bytes, spec.version, data_cap);
    let interleaved = interleave_with_ecc(&codewords, spec);

    let size = spec.version * 4 + 17;
    let mut base = vec![false; size * size];
    let mut reserved = vec![false; size * size];

    place_function_patterns(&mut base, &mut reserved, size, spec);
    place_data_bits(&mut base, &reserved, size, &interleaved);

    let mut best_mask = 0u8;
    let mut best_penalty = u32::MAX;
    let mut best_modules = Vec::new();

    for mask in 0..8u8 {
        let mut candidate = base.clone();
        apply_mask(&mut candidate, &reserved, size, mask);
        write_format_bits(&mut candidate, size, mask);
        if spec.version >= 7 {
            write_version_bits(&mut candidate, size, spec.version);
        }
        let penalty = penalty_score(&candidate, size);
        if penalty < best_penalty {
            best_penalty = penalty;
            best_mask = mask;
            best_modules = candidate;
        }
    }
    let _ = best_mask;

    Ok(QrMatrix {
        size,
        modules: best_modules,
    })
}

fn push_bits(bits: &mut Vec<bool>, val: usize, count: usize) {
    for shift in (0..count).rev() {
        bits.push(((val >> shift) & 1) != 0);
    }
}

fn build_data_codewords(data: &[u8], version: usize, data_cap: usize) -> Vec<u8> {
    let mut bits = Vec::with_capacity(data_cap * 8);
    // Mode indicator: Byte = 0100
    push_bits(&mut bits, 0b0100, 4);
    let count_bits = if version <= 9 { 8 } else { 16 };
    push_bits(&mut bits, data.len(), count_bits);
    for &b in data {
        push_bits(&mut bits, b as usize, 8);
    }
    let total_bits = data_cap * 8;
    let term = (total_bits - bits.len()).min(4);
    for _ in 0..term {
        bits.push(false);
    }
    while bits.len() % 8 != 0 {
        bits.push(false);
    }
    let mut out = Vec::with_capacity(data_cap);
    for chunk in bits.chunks_exact(8) {
        let mut byte = 0u8;
        for &bit in chunk {
            byte = (byte << 1) | u8::from(bit);
        }
        out.push(byte);
    }
    let mut pad_idx = 0;
    const PAD: [u8; 2] = [0xec, 0x11];
    while out.len() < data_cap {
        out.push(PAD[pad_idx % 2]);
        pad_idx += 1;
    }
    out
}

fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut res = 0u8;
    for _ in 0..8 {
        if (b & 1) != 0 {
            res ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1d;
        }
        b >>= 1;
    }
    res
}

fn rs_generator(degree: usize) -> Vec<u8> {
    let mut poly = vec![0u8; degree];
    poly[degree - 1] = 1;
    let mut root = 1u8;
    for _ in 0..degree {
        for j in 0..degree {
            poly[j] = gf_mul(poly[j], root);
            if j + 1 < degree {
                poly[j] ^= poly[j + 1];
            }
        }
        root = gf_mul(root, 0x02);
    }
    poly
}

fn rs_remainder(data: &[u8], generator: &[u8]) -> Vec<u8> {
    let mut rem = vec![0u8; generator.len()];
    for &b in data {
        let factor = b ^ rem[0];
        rem.rotate_left(1);
        *rem.last_mut().unwrap() = 0;
        for (r, &g) in rem.iter_mut().zip(generator.iter()) {
            *r ^= gf_mul(g, factor);
        }
    }
    rem
}

fn interleave_with_ecc(data: &[u8], spec: &VersionSpec) -> Vec<u8> {
    let generator = rs_generator(spec.ec_per_block);
    let mut data_blocks: Vec<&[u8]> = Vec::new();
    let mut ec_blocks: Vec<Vec<u8>> = Vec::new();
    let mut offset = 0;

    for _ in 0..spec.g1_blocks {
        let slice = &data[offset..offset + spec.g1_data];
        offset += spec.g1_data;
        ec_blocks.push(rs_remainder(slice, &generator));
        data_blocks.push(slice);
    }
    for _ in 0..spec.g2_blocks {
        let slice = &data[offset..offset + spec.g2_data];
        offset += spec.g2_data;
        ec_blocks.push(rs_remainder(slice, &generator));
        data_blocks.push(slice);
    }

    let mut result = Vec::with_capacity(spec.total_codewords);
    let max_data = spec.g1_data.max(spec.g2_data);
    for i in 0..max_data {
        for block in &data_blocks {
            if i < block.len() {
                result.push(block[i]);
            }
        }
    }
    for i in 0..spec.ec_per_block {
        for block in &ec_blocks {
            result.push(block[i]);
        }
    }
    debug_assert_eq!(result.len(), spec.total_codewords);
    result
}

fn set_fn_module(
    modules: &mut [bool],
    reserved: &mut [bool],
    size: usize,
    x: usize,
    y: usize,
    dark: bool,
) {
    let idx = y * size + x;
    modules[idx] = dark;
    reserved[idx] = true;
}

fn place_finder(modules: &mut [bool], reserved: &mut [bool], size: usize, cx: isize, cy: isize) {
    for dy in -4..=4isize {
        for dx in -4..=4isize {
            let x = cx + dx;
            let y = cy + dy;
            if x >= 0 && y >= 0 && (x as usize) < size && (y as usize) < size {
                let dist = dx.abs().max(dy.abs());
                let dark = dist != 2 && dist != 4;
                set_fn_module(modules, reserved, size, x as usize, y as usize, dark);
            }
        }
    }
}

fn place_alignment(modules: &mut [bool], reserved: &mut [bool], size: usize, cx: usize, cy: usize) {
    for dy in -2..=2isize {
        for dx in -2..=2isize {
            let x = (cx as isize + dx) as usize;
            let y = (cy as isize + dy) as usize;
            let dist = dx.abs().max(dy.abs());
            set_fn_module(modules, reserved, size, x, y, dist != 1);
        }
    }
}

fn place_function_patterns(
    modules: &mut [bool],
    reserved: &mut [bool],
    size: usize,
    spec: &VersionSpec,
) {
    // Timing patterns
    for i in 0..size {
        set_fn_module(modules, reserved, size, 6, i, i % 2 == 0);
        set_fn_module(modules, reserved, size, i, 6, i % 2 == 0);
    }
    // Finder patterns + separators
    place_finder(modules, reserved, size, 3, 3);
    place_finder(modules, reserved, size, (size - 4) as isize, 3);
    place_finder(modules, reserved, size, 3, (size - 4) as isize);

    // Alignment patterns
    let n = spec.align.len();
    for (iy, &cy) in spec.align.iter().enumerate() {
        for (ix, &cx) in spec.align.iter().enumerate() {
            if (ix == 0 && iy == 0) || (ix == 0 && iy == n - 1) || (ix == n - 1 && iy == 0) {
                continue;
            }
            place_alignment(modules, reserved, size, cx, cy);
        }
    }

    // Reserve format info strips + dark module
    for i in 0..=8 {
        if i != 6 {
            reserved[8 * size + i] = true;
            reserved[i * size + 8] = true;
        }
    }
    for i in (size - 8)..size {
        reserved[8 * size + i] = true;
        reserved[i * size + 8] = true;
    }
    set_fn_module(modules, reserved, size, 8, size - 8, true);

    // Reserve version info areas for version >= 7
    if spec.version >= 7 {
        for i in 0..6 {
            for j in 0..3 {
                reserved[i * size + (size - 11 + j)] = true;
                reserved[(size - 11 + j) * size + i] = true;
            }
        }
    }
}

fn place_data_bits(modules: &mut [bool], reserved: &[bool], size: usize, codewords: &[u8]) {
    let mut bit_idx = 0usize;
    let total_bits = codewords.len() * 8;
    let mut upward = true;
    let mut right = size as isize - 1;
    while right >= 1 {
        if right == 6 {
            right -= 1;
        }
        for vert in 0..size {
            let y = if upward { size - 1 - vert } else { vert };
            for j in 0..2 {
                let x = (right - j) as usize;
                let idx = y * size + x;
                if !reserved[idx] {
                    let dark = if bit_idx < total_bits {
                        ((codewords[bit_idx >> 3] >> (7 - (bit_idx & 7))) & 1) != 0
                    } else {
                        false
                    };
                    modules[idx] = dark;
                    bit_idx += 1;
                }
            }
        }
        upward = !upward;
        right -= 2;
    }
}

fn apply_mask(modules: &mut [bool], reserved: &[bool], size: usize, mask: u8) {
    for y in 0..size {
        for x in 0..size {
            let idx = y * size + x;
            if reserved[idx] {
                continue;
            }
            let invert = match mask {
                0 => (x + y) % 2 == 0,
                1 => y % 2 == 0,
                2 => x % 3 == 0,
                3 => (x + y) % 3 == 0,
                4 => (y / 2 + x / 3) % 2 == 0,
                5 => (x * y) % 2 + (x * y) % 3 == 0,
                6 => ((x * y) % 2 + (x * y) % 3) % 2 == 0,
                7 => ((x + y) % 2 + (x * y) % 3) % 2 == 0,
                _ => false,
            };
            if invert {
                modules[idx] = !modules[idx];
            }
        }
    }
}

fn write_format_bits(modules: &mut [bool], size: usize, mask: u8) {
    // ECC Level L format indicator = 01
    let data = (0b01u32 << 3) | u32::from(mask);
    let mut rem = data;
    for _ in 0..10 {
        rem = (rem << 1) ^ ((rem >> 9) * 0x537);
    }
    let bits = ((data << 10) | rem) ^ 0x5412;

    // Top-left horizontal + vertical
    for i in 0..=5 {
        modules[i * size + 8] = ((bits >> i) & 1) != 0;
    }
    modules[7 * size + 8] = ((bits >> 6) & 1) != 0;
    modules[8 * size + 8] = ((bits >> 7) & 1) != 0;
    modules[8 * size + 7] = ((bits >> 8) & 1) != 0;
    for i in 9..15 {
        modules[8 * size + (14 - i)] = ((bits >> i) & 1) != 0;
    }
    // Right horizontal + bottom-left vertical
    for i in 0..8 {
        modules[8 * size + (size - 1 - i)] = ((bits >> i) & 1) != 0;
    }
    for i in 8..15 {
        modules[(size - 15 + i) * size + 8] = ((bits >> i) & 1) != 0;
    }
    modules[(size - 8) * size + 8] = true;
}

fn write_version_bits(modules: &mut [bool], size: usize, version: usize) {
    let mut rem = version as u32;
    for _ in 0..12 {
        rem = (rem << 1) ^ ((rem >> 11) * 0x1f25);
    }
    let bits = ((version as u32) << 12) | rem;
    for i in 0..18 {
        let bit = ((bits >> i) & 1) != 0;
        let a = size - 11 + i % 3;
        let b = i / 3;
        modules[b * size + a] = bit;
        modules[a * size + b] = bit;
    }
}

fn penalty_score(modules: &[bool], size: usize) -> u32 {
    let mut score = 0u32;
    // Rule 1: adjacent modules in row/col in same color
    for y in 0..size {
        let mut run = 1u32;
        for x in 1..size {
            if modules[y * size + x] == modules[y * size + x - 1] {
                run += 1;
            } else {
                if run >= 5 {
                    score += 3 + (run - 5);
                }
                run = 1;
            }
        }
        if run >= 5 {
            score += 3 + (run - 5);
        }
    }
    for x in 0..size {
        let mut run = 1u32;
        for y in 1..size {
            if modules[y * size + x] == modules[(y - 1) * size + x] {
                run += 1;
            } else {
                if run >= 5 {
                    score += 3 + (run - 5);
                }
                run = 1;
            }
        }
        if run >= 5 {
            score += 3 + (run - 5);
        }
    }
    // Rule 2: 2x2 blocks
    for y in 0..size - 1 {
        for x in 0..size - 1 {
            let c = modules[y * size + x];
            if c == modules[y * size + x + 1]
                && c == modules[(y + 1) * size + x]
                && c == modules[(y + 1) * size + x + 1]
            {
                score += 3;
            }
        }
    }
    // Rule 4: balance of dark modules
    let dark = modules.iter().filter(|&&m| m).count();
    let total = size * size;
    let pct = (dark * 100) / total;
    let k = pct.abs_diff(50) / 5;
    score += (k as u32) * 10;
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_real_ilink_url_into_valid_finder_matrix() {
        let url = "https://liteapp.weixin.qq.com/q/7GiQu1?qrcode=5553fa6833bb075879062f71388720f1&bot_type=3";
        let qr = encode(url).unwrap();
        assert_eq!(qr.size, 37); // Version 5 (108 data bytes capacity for L)
        assert_eq!(qr.modules.len(), 37 * 37);
        // Check 3 finder pattern centers and outer borders.
        for (cx, cy) in [(3usize, 3usize), (33, 3), (3, 33)] {
            assert!(qr.get(cx, cy));
            assert!(qr.get(cx - 3, cy - 3));
            assert!(!qr.get(cx - 2, cy - 2));
        }
        // Dark module at (8, size - 8)
        assert!(qr.get(8, qr.size - 8));
    }

    #[test]
    fn encodes_short_and_v7_payloads_and_rejects_oversized() {
        assert_eq!(encode("hello").unwrap().size, 21);
        assert_eq!(encode(&"a".repeat(150)).unwrap().size, 45); // Version 7
        assert!(encode(&"x".repeat(300)).is_err());
    }
}
