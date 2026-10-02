// Same algorithm as strscan.nx, on `Vec<u8>` with the ASCII bytes that the
// NX one-character strings encode.
//
// The one deliberate difference: `text[i]` in NX mallocs a one-byte buffer
// and returns a fresh one-character string, so NX allocates once per
// character read. Here an index is a borrow of a byte. The counts are
// identical, which is the gate; the allocation behaviour is the finding.

fn main() {
    let passes: usize = 200;

    let vowels = b"aeiou";
    let chunk = b"the quick brown fox jumps over a lazy dog 0123456789 ";

    let mut text: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while i < 400 {
        text.extend_from_slice(chunk);
        i += 1;
    }

    println!("{}", text.len());

    let nl = text.len();
    let mut nv: usize = 0;
    let mut nc: usize = 0;
    let mut p = 0usize;
    while p < passes {
        let mut i = 0usize;
        while i < nl {
            let c = text[i];
            if vowels.contains(&c) {
                nv += 1;
            }
            if c < b'm' {
                nc += 1;
            }
            i += 1;
        }
        p += 1;
    }
    println!("{} {}", nv, nc);

    let mut no: usize = 0;
    let mut p = 0usize;
    while p < passes {
        for &c in text.iter() {
            if c == b'o' {
                no += 1;
            }
        }
        p += 1;
    }
    println!("{}", no);

    let mut hits: usize = 0;
    let mut p = 0usize;
    while p < passes {
        let mut i = 0usize;
        let lim = nl - 2;
        while i < lim {
            if text[i] == b'f' && text[i + 1] == b'o' && text[i + 2] == b'x' {
                hits += 1;
                i += 3;
            } else {
                i += 1;
            }
        }
        p += 1;
    }
    println!("{}", hits);

    let mut sl: usize = 0;
    let mut p = 0usize;
    while p < passes {
        let mut i = 0usize;
        while i < 800 {
            let w = &text[i..i + 2];
            if w < b"ab" {
                sl += 1;
            }
            i += 2;
        }
        p += 1;
    }
    println!("{}", sl);

    let mut found: usize = 0;
    for &c in b"abcdefghijklmnopqrstuvwxyz".iter() {
        if text.contains(&c) {
            found += 1;
        }
    }
    println!("{}", found);

    let mut cmp: usize = 0;
    let mut p = 0usize;
    while p < passes {
        let mut i = 0usize;
        while i < 200 {
            if &text[0..64] < &text[1..65] {
                cmp += 1;
            }
            i += 1;
        }
        p += 1;
    }
    println!("{}", cmp);
}