// Same algorithm as sortstr.nx.
//
// `Vec<&str>` rather than `Vec<String>`: NX shares one string object
// across every list slot that holds it, so a borrowed `&str` is the
// structure that matches what NX actually allocates. One allocation per
// distinct word, one fat pointer per list element.

fn main() {
    let m: usize = 200000;

    let letters: Vec<String> = "abcdefghijklmnopqrstuvwxyz".chars().map(|c| c.to_string()).collect();

    let nvocab: usize = 256;
    let mut vocab: Vec<&str> = Vec::with_capacity(nvocab);
    let mut i: usize = 0;
    while i < nvocab {
        let w: String = format!(
            "{}{}{}{}",
            letters[i % 26],
            letters[i / 26 % 26],
            letters[i / 676 % 26],
            letters[i % 7]
        );
        vocab.push(Box::leak(w.into_boxed_str()) as &str);
        i += 1;
    }
    println!("{} {} {}", vocab.len(), vocab[0], vocab[255]);

    let mut xs: Vec<&str> = Vec::with_capacity(m);
    let mut i: usize = 0;
    while i < m {
        xs.push(vocab[i * 7919 % nvocab]);
        i += 1;
    }

    let mut stack: Vec<usize> = vec![0usize; 64];
    let mut sp: usize = 0;
    stack[sp] = 0;
    sp += 1;
    stack[sp] = m - 1;
    sp += 1;
    let cut: usize = 16;

    let mut cmp: i64 = 0;
    let mut swp: i64 = 0;
    while sp > 0 {
        sp -= 1;
        let hi = stack[sp];
        sp -= 1;
        let lo = stack[sp];

        let mid = lo + (hi - lo) / 2;
        if xs[mid] < xs[lo] {
            xs.swap(mid, lo);
            cmp += 1;
        }
        if xs[hi] < xs[lo] {
            xs.swap(hi, lo);
            cmp += 1;
        }
        if xs[hi] < xs[mid] {
            xs.swap(hi, mid);
            cmp += 1;
        }
        xs.swap(mid, hi - 1);

        let pivot = xs[hi - 1];
        let mut k = lo;
        let mut j = lo;
        while j < hi - 1 {
            if xs[j] <= pivot {
                xs.swap(k, j);
                swp += 1;
                k += 1;
            }
            j += 1;
            cmp += 1;
        }
        xs.swap(k, hi - 1);

        if k - lo > cut {
            stack[sp] = lo;
            sp += 1;
            stack[sp] = k - 1;
            sp += 1;
        }
        if hi - k > cut {
            stack[sp] = k + 1;
            sp += 1;
            stack[sp] = hi;
            sp += 1;
        }
    }

    let mut i: usize = 1;
    while i < m {
        let key = xs[i];
        let mut j = i as i64 - 1;
        while j >= 0 && xs[j as usize] > key {
            xs[(j + 1) as usize] = xs[j as usize];
            j -= 1;
            cmp += 1;
        }
        xs[(j + 1) as usize] = key;
        i += 1;
    }

    let mut bad: i64 = 0;
    let mut i: usize = 0;
    while i < m {
        if i > 0 && xs[i - 1] > xs[i] {
            bad += 1;
        }
        i += 1;
    }

    let refs = ["aaaa", "abzz", "cfzz", "dlzz", "ezzz", "fyzz", "gznz", "hzzz"];

    let mut sig: i64 = 0;
    let mut samples: i64 = 0;
    let mut i: usize = 0;
    while i < m {
        if i % 997 == 0 {
            let mut rank: i64 = 0;
            let mut j: usize = 0;
            while j < 8 {
                if xs[i] > refs[j] {
                    rank += 1;
                }
                j += 1;
            }
            sig = sig * 9 + rank;
            samples += 1;
        }
        i += 1;
    }

    println!("{} {} {} {} {}", m, bad, cmp, swp, samples);
    println!(
        "{} {} {} {} {}",
        xs[0], xs[1], xs[m / 2], xs[m - 2], xs[m - 1]
    );
    println!("{}", sig);
}